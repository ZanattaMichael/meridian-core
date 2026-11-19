//! One run, stage by stage: reading each document, compiling it, asking
//! the policy, handing the artifact to its tool, and recording what came
//! of it. [`run_selected`](crate::run_selected) drives it.

use super::*;

pub(crate) struct Run<'a> {
    pub(crate) bundle: &'a Bundle,
    pub(crate) registry: &'a Registry,
    pub(crate) mode: Mode,
    pub(crate) options: &'a Options,
    /// The outputs of each Infrastructure document, once known.
    pub(crate) outputs: BTreeMap<String, Vec<Output>>,
    /// Sensitive values seen so far, masked out of everything shown.
    pub(crate) secrets: Vec<String>,
    /// Every stage's last apply, as read when the run started and as this
    /// run has changed it since.
    pub(crate) state: RunState,
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) run_id: String,
    /// The run this one continues, when it was asked to.
    pub(crate) resume: Option<Resumed>,
    pub(crate) warnings: Vec<String>,
    /// Every stage this run has handed to a tool to apply, in order, with
    /// what is needed to put it back.
    pub(crate) touched: Vec<rollback::Touched>,
    /// What the policy said about each stage the preflight asked about, and
    /// the artifact it was asked about.
    pub(crate) checked: BTreeMap<StageId, (String, Decisions)>,
    /// What the analyzers said about each stage the preflight put through
    /// them, and the artifact they looked at.
    pub(crate) analyzed: BTreeMap<StageId, (String, Vec<Analysis>)>,
    /// What each document compiled to in this run, for the stages that
    /// import a file from it.
    pub(crate) artifacts: BTreeMap<StageId, Artifact>,
    /// Whether this is an apply's first pass, which asks the policy about
    /// every stage it can and applies nothing.
    pub(crate) preflight: bool,
    /// The filter for the secrets the stage now running fetched, for what is
    /// said about a failure. Empty between stages.
    pub(crate) masker: Masker,
    /// The secrets of the stage now running that could not be locked in
    /// memory, and whether the run has already said so.
    pub(crate) unlocked: Vec<String>,
    pub(crate) warned_unlocked: bool,
}

/// A stage, compiled.
pub(crate) struct Compiled {
    /// The tree the target was handed.
    pub(crate) graph: meridian_sdk::ResourceGraph,
    pub(crate) emitted: meridian_sdk::Emitted,
    /// The machine the stage configures; empty when there is none.
    pub(crate) host: String,
    /// The values the stage's tool is handed.
    pub(crate) inputs: Data,
    /// The credential the host is reached with, when the document names
    /// one, with its secret fields as slots.
    pub(crate) credential: Option<meridian_ir::Credential>,
}

/// Why a stage did not compile, or cannot yet.
pub(crate) enum NotCompiled {
    Failed {
        reason: Reason,
        why: String,
        /// The stage's host, when it was worked out before the failure.
        host: String,
    },
    Deferred {
        waiting_on: StageId,
        output: String,
    },
}

/// A stage's document, as this run reads it.
pub(crate) enum Read {
    /// The document, holding only what will be compiled, what its
    /// conditions removed, and the secrets it reads. The secrets must
    /// outlive every use of the document.
    Kept {
        document: Box<Document>,
        removed: Vec<Removed>,
        secrets: Secrets,
    },
    /// The document's own `spec.when` was false.
    LeftOut {
        condition: String,
    },
    Failed(String),
}

/// Why a stage stopped before reaching the tool.
pub(crate) enum Halt {
    Failed(String),
    Deferred { waiting_on: StageId, output: String },
}

/// The credential as a plan names it, with how it reaches the tool when
/// that is worth reviewing: a cloud credential goes in the tool's own
/// environment, rank 4 of the plan's channels, because the providers take
/// one no other way. Never any part of a secret.
fn credential_line(
    credential: &meridian_ir::Credential,
    endpoint: Option<&meridian_ir::Endpoint>,
) -> String {
    let summary = credential.summary();
    match endpoint {
        // An ambient one hands nothing secret, but its plain fields (a
        // profile, a region) still go the same way.
        Some(e) if e.r#type == meridian_ir::ENDPOINT_TYPE_CLOUD => {
            format!("{summary}, in the tool's environment")
        }
        _ => summary,
    }
}

impl Run<'_> {
    /// `None` when `target` takes the credential, by `takes` of its
    /// capabilities; otherwise the loaded targets that do, as a refusal
    /// lists them.
    fn refusing_credential(
        &self,
        target: &str,
        takes: impl Fn(&meridian_sdk::Capabilities) -> bool,
    ) -> Option<String> {
        if self
            .registry
            .target(target)
            .is_some_and(|t| takes(&t.info().capabilities))
        {
            return None;
        }
        let takers: Vec<&str> = self
            .registry
            .names()
            .into_iter()
            .filter(|name| {
                self.registry
                    .target(name)
                    .is_some_and(|t| takes(&t.info().capabilities))
            })
            .collect();
        Some(if takers.is_empty() {
            "no loaded target does".to_owned()
        } else {
            format!("targets that do: {}", takers.join(", "))
        })
    }

    pub(crate) fn stage(&mut self, stage: Stage<'_>, observer: &mut dyn Observer) -> StageReport {
        let mut report = self.stage_with_secrets(stage, observer);
        // Said once in a run: the reason is the machine's, not the stage's.
        let unlocked = std::mem::take(&mut self.unlocked);
        if !unlocked.is_empty() && !self.warned_unlocked {
            self.warned_unlocked = true;
            report.warnings.push(Warning {
                target: "secrets".to_owned(),
                resource: String::new(),
                msg: format!(
                    "{} could not be locked in memory and may reach swap. {} \
                     --strict-secrets makes this an error",
                    secrets::named(&unlocked),
                    secrets::RAISE_THE_LIMIT
                ),
            });
        }
        // The stage is over: its secrets are wiped now, not at the end of
        // the run, and so is the filter that held their forms.
        self.masker = Masker::new();
        report
    }

    fn stage_with_secrets(&mut self, stage: Stage<'_>, observer: &mut dyn Observer) -> StageReport {
        let id = stage.id();
        let mut report = StageReport {
            id: id.clone(),
            target: stage.target().to_owned(),
            host: String::new(),
            credential: String::new(),
            artifact: None,
            warnings: Vec::new(),
            removed: Vec::new(),
            outcome: Outcome::Compiled,
            reason: None,
            done_before_failing: Vec::new(),
            previous_good: String::new(),
            policy: None,
            analyses: Vec::new(),
        };
        let fail = |mut report: StageReport, reason: Reason, why: String| {
            report.outcome = Outcome::Failed(why);
            report.reason = Some(reason);
            report
        };

        // What the document's conditions and interpolations come to. A
        // document its own `when` leaves out stops here, before a plugin is
        // even looked for.
        //
        // The first pass of an apply with a policy fetches no secret: one is
        // fetched once, at the turn of the stage that reads it. A document
        // that reads secrets cannot be decided without them, so the policy
        // is asked about it at its turn instead.
        if self.preflight && !stage.document().secrets().is_empty() {
            return report;
        }
        let (document, secrets) = match self.read(stage, !self.preflight) {
            Read::Kept {
                document,
                removed,
                secrets,
            } => {
                report.removed = removed;
                report.target = document.target().to_owned();
                self.unlocked = secrets.unlocked();
                (document, secrets)
            }
            Read::LeftOut { condition } => {
                // A plan that previewed this stage promised a change here;
                // leaving it out now would make the plan a lie.
                if let Some(planned) = self.planned(&id) {
                    if planned.status != PlannedStatus::LeftOut {
                        return fail(
                            report,
                            Reason::PlanDiffers,
                            format!(
                                "the plan {} this stage, and its spec.when ({condition}) now \
                                 leaves it out; take a new plan",
                                planned.status.as_str()
                            ),
                        );
                    }
                }
                report.outcome = Outcome::LeftOut { condition };
                return report;
            }
            Read::Failed(why) => return fail(report, Reason::CompileRefused, why),
        };
        let stage = Stage::of(&document);

        let Compiled {
            graph,
            emitted,
            host,
            mut inputs,
            credential,
        } = match self.compile(stage, &secrets.names()) {
            Ok(compiled) => compiled,
            Err(NotCompiled::Failed { reason, why, host }) => {
                report.host = host;
                return fail(report, reason, why);
            }
            Err(NotCompiled::Deferred { waiting_on, output }) => {
                report.outcome = Outcome::Deferred { waiting_on, output };
                return report;
            }
        };
        let registry = self.registry;
        let target = registry
            .target(stage.target())
            .expect("the stage was compiled by its target");
        let info = target.info();
        report.host = host;
        report.credential = credential
            .as_ref()
            .map(|c| credential_line(c, document.endpoint()))
            .unwrap_or_default();
        report.warnings = emitted.warnings;
        self.artifacts.insert(id.clone(), emitted.artifact.clone());
        report.artifact = Some(emitted.artifact.clone());

        // The analyzers look at what the document compiled to, before any
        // tool does. The first pass of an apply has looked already when the
        // artifact is the same.
        if !self.options.analyzers.is_empty() {
            let hash = artifact_hash(&emitted.artifact);
            let known = self
                .analyzed
                .get(&id)
                .filter(|(looked_at, _)| *looked_at == hash)
                .map(|(_, analyses)| analyses.clone());
            report.analyses = match known {
                Some(analyses) => analyses,
                None => {
                    let analyses = self.analyze(&stage, &report, info.category, &graph);
                    if self.preflight {
                        self.analyzed.insert(id.clone(), (hash, analyses.clone()));
                    }
                    analyses
                }
            };
            if let Some((reason, why)) = self.found_wanting(&report.analyses) {
                return fail(report, reason, why);
            }
        }
        if self.mode == Mode::Compile {
            return report;
        }

        // What the stage is handed from other documents. Compiling needs
        // none of it: the values reach the tool, not the artifact.
        if let Some(document) = stage.in_bundle(self.bundle) {
            match self.imports_of(document) {
                Ok(imports) => inputs.extend(imports),
                Err(NotCompiled::Failed { reason, why, .. }) => return fail(report, reason, why),
                Err(NotCompiled::Deferred { waiting_on, output }) => {
                    report.outcome = Outcome::Deferred { waiting_on, output };
                    return report;
                }
            }
        }

        let operations = target.info().operations;
        let options = self.options;
        let asked = options
            .policy
            .as_ref()
            .filter(|_| matches!(self.mode, Mode::Plan | Mode::Apply));
        if self.mode == Mode::Plan && !operations.plan {
            // Compiling is still a result worth having; the target just
            // cannot say what applying it would change. A policy is still
            // asked, about what the document compiled to.
            if let Some(policy) = asked {
                let decisions = self.decide(policy, &stage, &report, info.category, &graph, None);
                report.policy = Some(decisions.clone());
                if !decisions.allowed() {
                    return fail(report, Reason::PolicyDenied, denial(&decisions));
                }
            }
            return report;
        }
        let hash = artifact_hash(&emitted.artifact);
        // An apply from a saved plan applies what was planned, or nothing:
        // the document must still compile to the very artifact the plan was
        // taken for. A stage the plan could not preview is applied as any
        // apply would, and the report says it was not checked.
        let mut saved_plan = None;
        if let Some(planned) = self.planned(&id) {
            match planned.status {
                PlannedStatus::Deferred => report.warnings.push(Warning {
                    target: "plan".to_owned(),
                    resource: String::new(),
                    msg: "the plan could not preview this stage, which waited on an output \
                          that did not exist yet, so its artifact was not checked against \
                          the plan"
                        .to_owned(),
                }),
                _ if planned.artifact_hash != hash => {
                    let data = self.changed_data();
                    return fail(
                        report,
                        Reason::PlanDiffers,
                        format!(
                            "the document no longer compiles to what was planned (the plan \
                             has {}, and it now compiles to {hash}){data}; take a new plan",
                            if planned.artifact_hash.is_empty() {
                                "no artifact".to_owned()
                            } else {
                                planned.artifact_hash.clone()
                            }
                        ),
                    );
                }
                _ => {}
            }
            if info.saves_plans {
                saved_plan = planned.saved_for_apply();
            }
        }
        // What was last applied here, if anything was.
        let last = self.state.get(&id).cloned();
        if self.mode == Mode::Drift {
            if last.is_none() {
                report.outcome = Outcome::Drift(Drift::NeverApplied);
                return report;
            }
            if !operations.plan {
                report.outcome = Outcome::Drift(Drift::Unknown);
                return report;
            }
        }
        if self.mode == Mode::Apply && !operations.apply {
            return fail(
                report,
                Reason::NotSupported,
                format!(
                    "target {:?} only compiles, so Meridian cannot apply this document; \
                     use `meridian compile` and apply the result with the tool itself",
                    stage.target()
                ),
            );
        }

        // The value of each secret a param holds, for the tool. The target
        // declared it takes them, or the read would have stopped.
        let handed = match secrets::handed(&graph, secrets.table()) {
            Ok(handed) => handed,
            Err(why) => return fail(report, Reason::CompileRefused, why),
        };
        let held = match secrets::handed_credential(credential.as_ref(), secrets.table()) {
            Ok(held) => held,
            Err(why) => return fail(report, Reason::CompileRefused, why),
        };
        let mut context =
            match self.context(&id, stage.target(), &report.host, inputs, handed, held) {
                Ok(context) => context,
                Err(why) => return fail(report, Reason::StateFailed, why),
            };
        context.saved_plan = saved_plan;
        // More of the tool's own output, when asked for this stage. Its lines
        // are masked as every tool line is.
        if options.tool_debug > 0 && options.debugs(&id) {
            if info.debug_levels.is_empty() {
                report.warnings.push(Warning {
                    target: stage.target().to_owned(),
                    resource: String::new(),
                    msg: format!(
                        "target {:?} has no debug output, so --tool-debug changes nothing for {}",
                        stage.target(),
                        id.name
                    ),
                });
            } else {
                context.debug_level = options.tool_debug;
            }
        }
        // A plan that will be written asks each tool that can for the plan
        // it saved, so the apply can carry out exactly that.
        context.save_plan = self.mode == Mode::Plan && options.save_plan && info.saves_plans;

        // A resumed run leaves alone what the run it continues already
        // applied, as long as the document still compiles to the same thing.
        if let Some(applied_in) = self.already_applied(&id, &hash, operations.outputs) {
            report.outcome = Outcome::Skipped { applied_in };
            return report;
        }
        // What the tool prints is searched for every sensitive output of
        // earlier stages and for every secret this stage fetched, however
        // the tool came to print it.
        let sensitive = self.secrets.clone();
        let masker = secrets.masker();
        self.masker = secrets.masker();
        let mut log = |mut line: LogLine| {
            line.text = masker.mask(&mask(&line.text, &sensitive));
            observer.log(&id, &line);
        };

        // Ask the policy, unless the first pass already asked about this
        // very artifact.
        let mut planned: Option<Plan> = None;
        if let Some(policy) = asked {
            let known = self
                .checked
                .get(&id)
                .filter(|(checked, _)| *checked == hash)
                .map(|(_, decisions)| decisions.clone());
            let decisions = match known {
                Some(decisions) => decisions,
                None => {
                    if operations.plan {
                        match target.plan_with(
                            &context,
                            &emitted.artifact,
                            &mut log,
                            &self.options.stop,
                        ) {
                            Ok(plan) => planned = Some(plan),
                            // What cannot be previewed yet, because an
                            // earlier stage has to be applied first, is
                            // asked about at its turn.
                            Err(_) if self.preflight => return report,
                            Err(e) => return fail(report, reason_of(&e), self.describe(e)),
                        }
                    }
                    self.decide(
                        policy,
                        &stage,
                        &report,
                        info.category,
                        &graph,
                        planned.as_ref(),
                    )
                }
            };
            report.policy = Some(decisions.clone());
            if !decisions.allowed() {
                return fail(report, Reason::PolicyDenied, denial(&decisions));
            }
            if self.preflight {
                self.checked.insert(id.clone(), (hash.clone(), decisions));
            }
        }
        if self.preflight {
            return report;
        }

        // An interrupt that arrived while the stage was compiling stops it
        // here, before a tool is started that would only be stopped again.
        if self.interrupted() && self.mode != Mode::Compile {
            return fail(
                report,
                Reason::Interrupted,
                "interrupted: the run was stopped before the tool was started".to_owned(),
            );
        }
        let stop = &self.options.stop;

        let plan_now = match planned.take() {
            Some(plan) if self.mode == Mode::Plan => Some(Ok(plan)),
            _ => None,
        };
        match self.mode {
            Mode::Plan => match plan_now
                .unwrap_or_else(|| target.plan_with(&context, &emitted.artifact, &mut log, stop))
            {
                Ok(plan) => report.outcome = Outcome::Planned(plan),
                Err(e) => return fail(report, reason_of(&e), self.describe(e)),
            },
            Mode::Drift => match target.plan_with(&context, &emitted.artifact, &mut log, stop) {
                Ok(plan) => {
                    let last = last.unwrap_or_default();
                    let same = last.applied && last.artifact_hash == hash;
                    report.outcome = Outcome::Drift(if plan.unknown {
                        Drift::Unknown
                    } else if !same {
                        Drift::Pending {
                            changes: plan.changes,
                            last_apply_failed: !last.applied,
                        }
                    } else if plan.changes.is_empty() {
                        Drift::InSync
                    } else {
                        Drift::Drifted(plan.changes)
                    });
                }
                Err(e) => return fail(report, reason_of(&e), self.describe(e)),
            },
            Mode::Apply => {
                report.previous_good = last.map(|l| l.last_good).unwrap_or_default();
                // Kept without the stage's secrets, which are wiped when the
                // stage ends; a rollback fetches them again at its turn.
                self.touched.push(rollback::Touched {
                    id: id.clone(),
                    target: stage.target().to_owned(),
                    context: RunContext {
                        secrets: meridian_sdk::Secrets::none(),
                        credential: None,
                        ..context.clone()
                    },
                    previous: report.previous_good.clone(),
                    attempted: hash.clone(),
                    masker: Masker::new(),
                });
                let outcome = target.apply_with(&context, &emitted.artifact, &mut log, stop);
                match outcome {
                    Ok(mut applied) => {
                        // A tool that hands a sensitive value straight back
                        // as an output has not made it any less sensitive.
                        for output in &mut applied.outputs {
                            if let Value::String(text) = &output.value {
                                if self.secrets.iter().any(|secret| secret == text) {
                                    output.sensitive = true;
                                }
                            }
                        }
                        self.record(&id, stage.target(), &hash, true);
                        self.snapshot(&id, &emitted.artifact, &report.previous_good);
                        self.learn(&id, &applied.outputs);
                        report.outcome = Outcome::Applied {
                            changes: applied.changes,
                            outputs: applied.outputs,
                        };
                    }
                    Err(e) => {
                        self.record(&id, stage.target(), &hash, false);
                        match &e {
                            RunFailure::Tool(tool) => {
                                report.done_before_failing = tool.changes.clone();
                            }
                            RunFailure::Stopped(stopped) => {
                                report.done_before_failing = stopped.changes.clone();
                            }
                            RunFailure::Plugin(_) => {}
                        }
                        return fail(report, reason_of(&e), self.describe(e));
                    }
                }
            }
            Mode::Compile | Mode::Rollback => {}
        }
        report
    }

    /// Compiles a stage: settles what is the core's to settle, works out
    /// the host, builds the tree and hands it to the target. No tool is
    /// asked to change anything.
    /// Decides everything the core decides about a document: its conditions
    /// and its interpolations, the target among them.
    ///
    /// With `fetch`, the document's secrets are fetched: that is for a stage
    /// at its own turn, exactly once, and only in a run that drives a tool.
    /// A document its own `when` leaves out without reading a secret fetches
    /// nothing. Without `fetch` the document is read as `compile` reads it:
    /// a secret in a param is written as what stands for it, and a
    /// condition that reads one cannot be decided.
    fn read(&self, stage: Stage<'_>, fetch: bool) -> Read {
        let document = stage.document();
        let mut secrets = Secrets::declared(&document);
        // What the target's plugin declares of its tool's own expression
        // syntax, so that a value placed from data is literal to the tool
        // as well as to Meridian. A target no plugin provides gets only
        // Meridian's own escape, and is refused for want of a plugin later.
        let registry = self.registry;
        let escapes_for = |target: &str| {
            registry
                .target(target)
                .map(|t| t.info().placed_text.clone())
                .unwrap_or_default()
        };
        let mut resolution =
            meridian_ir::resolve_for(&document, &self.options.data, &secrets, &escapes_for);
        let fetches = fetch
            && self.mode != Mode::Compile
            && !document.secrets().is_empty()
            && !matches!(resolution, Ok(Resolution::Skipped { .. }));
        if fetches {
            let fetched = secrets::fetch(&document, &mut secrets).and_then(|()| {
                let unlocked = secrets.unlocked();
                if self.options.strict_secrets && !unlocked.is_empty() {
                    return Err(format!(
                        "{} could not be locked in memory and may reach swap, and \
                         --strict-secrets is set. {}",
                        secrets::named(&unlocked),
                        secrets::RAISE_THE_LIMIT
                    ));
                }
                Ok(())
            });
            resolution = match fetched {
                Ok(()) => {
                    meridian_ir::resolve_for(&document, &self.options.data, &secrets, &escapes_for)
                }
                Err(why) => Err(meridian_ir::Error {
                    pos: document.position().clone(),
                    path: "spec.secrets".to_owned(),
                    msg: why,
                }),
            };
        }
        let (mut document, removed) = match resolution {
            Ok(Resolution::Kept { document, removed }) => (*document, removed),
            Ok(Resolution::Skipped { condition }) => return Read::LeftOut { condition },
            Err(e) => return Read::Failed(e.to_string()),
        };

        // A secret in a param. A target that declares it takes one is handed
        // the secret as such: its emitter writes a reference to it into the
        // artifact, and `plan` and `apply` hand its runner the value by a
        // channel that is not the compiled files. For any other target, a
        // read that fetched nothing writes what stands for the secret, and a
        // read that fetched it stops here instead of writing a secret into
        // a file or dropping it silently.
        if let Some((resource, param)) = secrets::first_in_params(&document) {
            let takes_secrets = self
                .registry
                .target(document.target())
                .is_some_and(|target| target.info().capabilities.secrets);
            if takes_secrets {
                // Kept as it is: `Value::Secret`, which the plugin names.
            } else if !fetches {
                secrets::describe_in_params(&mut document, secrets.table());
            } else {
                let takers: Vec<&str> = self
                    .registry
                    .names()
                    .into_iter()
                    .filter(|name| {
                        self.registry
                            .target(name)
                            .is_some_and(|t| t.info().capabilities.secrets)
                    })
                    .collect();
                let takers = if takers.is_empty() {
                    "no loaded target does".to_owned()
                } else {
                    format!("targets that do: {}", takers.join(", "))
                };
                return Read::Failed(format!(
                    "resource {resource:?}: {param} holds a secret, and target {:?} does not \
                     take one: it has no private channel to hand a secret to its tool, so the \
                     value would be written into a file or dropped. A secret can still decide \
                     a when for it; {takers}",
                    document.target()
                ));
            }
        }
        // A credential named for the host. Only a target that declares it
        // takes one is handed it, in every mode: the artifact of any other
        // target would be run as whoever the tool is configured to be, which
        // is not what the document says.
        if let Some(name) = document
            .as_resource_set()
            .and_then(|d| d.spec.target_host.as_ref())
            .map(|th| th.credential.as_str())
            .filter(|name| !name.is_empty())
        {
            let takes = |capabilities: &meridian_sdk::Capabilities| capabilities.credentials;
            if let Some(takers) = self.refusing_credential(document.target(), takes) {
                return Read::Failed(format!(
                    "spec.targetHost names the credential {name:?}, and target {:?} does not \
                     take one: it has no way to reach its host as that identity, so the \
                     document would be applied as whoever the tool is configured to be. \
                     Leave the credential out to use the tool's own; {takers}",
                    document.target()
                ));
            }
        }
        // A credential named for the endpoint, the same way: a provider that
        // is not handed it would use whatever login it finds on this machine.
        if let Some(endpoint) = document.endpoint() {
            let takes = |capabilities: &meridian_sdk::Capabilities| {
                endpoint.r#type == meridian_ir::ENDPOINT_TYPE_CLOUD
                    && capabilities.cloud_credentials
            };
            if let Some(takers) = self.refusing_credential(document.target(), takes) {
                return Read::Failed(format!(
                    "spec.endpoint names the credential {:?} for a {}, and target {:?} does \
                     not take one: it has no way to hand its tool that identity, so the \
                     document would be applied as whatever login the tool finds on this \
                     machine. Leave the endpoint out to use the tool's own; {takers}",
                    endpoint.credential,
                    endpoint.r#type,
                    document.target()
                ));
            }
        }
        // An adoption. A target that cannot adopt, which is every plugin
        // built before `adopt` existed, would ignore the field and create a
        // second thing beside the one the document says exists.
        if let Some(r) = document.resources().iter().find(|r| !r.adopt.is_empty()) {
            let adopts = |name: &str| {
                self.registry
                    .target(name)
                    .is_some_and(|t| t.info().capabilities.adopt != meridian_sdk::Adoption::None)
            };
            if !adopts(document.target()) {
                let takers: Vec<&str> = self
                    .registry
                    .names()
                    .into_iter()
                    .filter(|name| adopts(name))
                    .collect();
                let takers = if takers.is_empty() {
                    "no loaded target does".to_owned()
                } else {
                    format!("targets that do: {}", takers.join(", "))
                };
                return Read::Failed(format!(
                    "resource {:?} adopts {:?}, and target {:?} cannot take a thing that \
                     already exists under management, so it would create another beside \
                     it. Remove adopt; {takers}",
                    r.id,
                    r.adopt,
                    document.target()
                ));
            }
        }
        Read::Kept {
            document: Box::new(document),
            removed,
            secrets,
        }
    }

    /// Compiles a document that has been [read](Run::read).
    fn compile(
        &mut self,
        stage: Stage<'_>,
        declared_secrets: &[(String, SecretRef)],
    ) -> Result<Compiled, NotCompiled> {
        let id = stage.id();
        let refuse = |reason: Reason, why: String| NotCompiled::Failed {
            reason,
            why,
            host: String::new(),
        };
        let registry = self.registry;
        let options = self.options;
        let Some(target) = registry.target(stage.target()) else {
            let known = registry.names().join(", ");
            let known = if known.is_empty() {
                "no plugins are loaded".to_owned()
            } else {
                format!("loaded targets: {known}")
            };
            return Err(refuse(
                Reason::NoPlugin,
                format!("no plugin provides target {:?} ({known})", stage.target()),
            ));
        };

        let info = target.info();
        if let Err(why) = kind_fits(id.kind, info.category) {
            return Err(refuse(Reason::CompileRefused, why));
        }
        let asked = stage.target_version();
        if !asked.is_empty() {
            if info.target_versions.is_empty() {
                return Err(refuse(
                    Reason::CompileRefused,
                    format!(
                        "the document asks for targetVersion {asked:?}, but target {:?} does \
                         not say which versions of its tool it compiles for, so the request \
                         cannot be honoured; remove targetVersion",
                        stage.target()
                    ),
                ));
            }
            if !version_fits(asked, &info.target_versions) {
                return Err(refuse(
                    Reason::CompileRefused,
                    format!(
                        "the document asks for targetVersion {asked:?}, and target {:?} \
                         compiles for: {}",
                        stage.target(),
                        info.target_versions.join(", ")
                    ),
                ));
            }
        }
        // A setting the target does not read would be dropped on the floor,
        // and a plugin built before settings existed reads none.
        if let Some(key) = stage
            .settings()
            .keys()
            .find(|k| !info.settings_keys.contains(k))
        {
            let why = if info.settings_keys.is_empty() {
                format!(
                    "the document sets spec.settings.{key}, and target {:?} takes no settings; \
                     remove spec.settings, or use a plugin that takes them",
                    stage.target()
                )
            } else {
                format!(
                    "the document sets spec.settings.{key}, and target {:?} takes only: {}",
                    stage.target(),
                    info.settings_keys.join(", ")
                )
            };
            return Err(refuse(Reason::CompileRefused, why));
        }
        if let Some(import) = stage.imports().first() {
            if !info.accepts_inputs {
                // Dropping the value without a word would compile into
                // something that means less than the document says.
                return Err(refuse(
                    Reason::CompileRefused,
                    format!(
                        "the document imports {:?}, and target {:?} has no way to hand a value \
                         from another document to its tool; remove spec.imports, or use a \
                         target that takes inputs",
                        import.name,
                        stage.target()
                    ),
                ));
            }
        }

        // Resolve what the stage needs from earlier ones, then build its tree.
        let (graph, host, inputs) = match stage {
            Stage::Infrastructure(doc) => match ResourceGraph::build_infrastructure(doc) {
                Ok(graph) => (graph, String::new(), Data::new()),
                Err(e) => return Err(refuse(Reason::CompileRefused, e.to_string())),
            },
            Stage::ResourceSet(doc) => {
                let (host, inputs) = match self.host_of(doc) {
                    Ok(found) => found,
                    Err(Halt::Failed(why)) => return Err(refuse(Reason::CompileRefused, why)),
                    Err(Halt::Deferred { waiting_on, output }) => {
                        return Err(NotCompiled::Deferred { waiting_on, output })
                    }
                };
                match ResourceGraph::build(doc, &host) {
                    Ok(graph) => (graph, host, inputs),
                    Err(e) => {
                        return Err(NotCompiled::Failed {
                            reason: Reason::CompileRefused,
                            why: e.to_string(),
                            host,
                        })
                    }
                }
            }
        };

        let credential = graph.credential.clone();
        let mut graph = to_sdk(&graph);
        // A secret in a param reaches the plugin by name, so the plugin can
        // write a reference the operator recognises. Only a target that
        // takes secrets is read with any (see `read`).
        name_secrets(&mut graph, declared_secrets);
        let failed =
            |reason: Reason, why: String, host: String| NotCompiled::Failed { reason, why, host };
        match target.emit(&graph, &options.data) {
            Ok(emitted) => Ok(Compiled {
                graph,
                emitted,
                host,
                inputs,
                credential,
            }),
            Err(EmitError::Refused(e)) => Err(failed(Reason::CompileRefused, e.to_string(), host)),
            Err(EmitError::Plugin(e)) => Err(failed(Reason::PluginFailed, e.to_string(), host)),
        }
    }

    /// What a document compiles to: what this run already compiled it to,
    /// or, for a document the run does not include, a compile made now.
    fn artifact_of(&mut self, source: &Document) -> Result<Artifact, NotCompiled> {
        let stage = match source {
            Document::Infrastructure(doc) => Stage::Infrastructure(doc),
            Document::ResourceSet(doc) => Stage::ResourceSet(doc),
        };
        let id = stage.id();
        if let Some(artifact) = self.artifacts.get(&id) {
            return Ok(artifact.clone());
        }
        // Nothing of this document runs here, so nothing of it is read from
        // a provider.
        let (document, secrets) = match self.read(stage, false) {
            Read::Kept {
                document, secrets, ..
            } => (document, secrets),
            Read::LeftOut { condition } => {
                return Err(NotCompiled::Failed {
                    reason: Reason::CompileRefused,
                    why: format!("its when ({condition}) is false, so it compiles to nothing"),
                    host: String::new(),
                })
            }
            Read::Failed(why) => {
                return Err(NotCompiled::Failed {
                    reason: Reason::CompileRefused,
                    why,
                    host: String::new(),
                })
            }
        };
        let compiled = self.compile(Stage::of(&document), &secrets.names())?;
        self.artifacts.insert(id, compiled.emitted.artifact.clone());
        Ok(compiled.emitted.artifact)
    }

    /// The values a document's `spec.imports` hand it, by the names they
    /// arrive under.
    ///
    /// A file another document compiles to is known as soon as that document
    /// compiles. An output is known once its document has been applied, in
    /// this run or before; until then the stage waits, as a ResourceSet
    /// waits on a host.
    pub(crate) fn imports_of(&mut self, document: &Document) -> Result<Data, NotCompiled> {
        let bundle = self.bundle;
        let refuse = |why: String| NotCompiled::Failed {
            reason: Reason::CompileRefused,
            why,
            host: String::new(),
        };
        let mut values = Data::new();
        for import in document.imports() {
            let Some(source) = bundle.import_source(import) else {
                return Err(refuse(format!(
                    "import {:?} names the document {:?}, which is not loaded",
                    import.name, import.r#ref
                )));
            };
            let mut sensitive = import.sensitive;
            let value = if import.from == meridian_ir::IMPORT_FROM_ARTIFACT {
                let artifact = match self.artifact_of(source) {
                    Ok(artifact) => artifact,
                    Err(NotCompiled::Failed { why, .. }) => {
                        return Err(refuse(format!(
                            "import {:?} is a file {} {:?} compiles to, and that document did \
                             not compile: {why}",
                            import.name,
                            source.kind(),
                            source.name()
                        )))
                    }
                    Err(deferred) => return Err(deferred),
                };
                let Some(content) = artifact.files.get(&import.file) else {
                    let have: Vec<&str> = artifact.files.keys().map(String::as_str).collect();
                    return Err(refuse(format!(
                        "import {:?} is the file {:?} of {} {:?}, which compiles to no file of \
                         that name (it compiles to: {})",
                        import.name,
                        import.file,
                        source.kind(),
                        source.name(),
                        have.join(", ")
                    )));
                };
                Value::from(content.as_str())
            } else {
                let from = StageId {
                    kind: StageKind::Infrastructure,
                    name: import.r#ref.clone(),
                };
                if !self.outputs.contains_key(&from.name) {
                    self.read_outputs(&from);
                }
                let Some(outputs) = self.outputs.get(&from.name) else {
                    return Err(NotCompiled::Deferred {
                        waiting_on: from,
                        output: import.output.clone(),
                    });
                };
                let Some(output) = outputs.iter().find(|o| o.name == import.output) else {
                    let have: Vec<&str> = outputs.iter().map(|o| o.name.as_str()).collect();
                    return Err(refuse(format!(
                        "{from} did not produce the output {:?} that import {:?} needs (it \
                         produced: {})",
                        import.output,
                        import.name,
                        if have.is_empty() {
                            "nothing".to_owned()
                        } else {
                            have.join(", ")
                        }
                    )));
                };
                sensitive |= output.sensitive;
                output.value.clone()
            };
            if sensitive {
                if let Value::String(text) = &value {
                    self.keep_secret(text);
                }
            }
            values.insert(import.name.clone(), value);
        }
        Ok(values)
    }

    /// Puts a stage that has compiled through every analyzer of the run.
    fn analyze(
        &self,
        stage: &Stage<'_>,
        report: &StageReport,
        category: Category,
        graph: &meridian_sdk::ResourceGraph,
    ) -> Vec<Analysis> {
        let empty = Artifact::default();
        let subject = analyze::Subject {
            mode: self.mode.as_str(),
            run: &self.run_id,
            kind: report.id.kind.as_str(),
            name: &report.id.name,
            target: &report.target,
            category: category.as_str(),
            host: &report.host,
            labels: stage.labels(),
            graph,
            artifact: report.artifact.as_ref().unwrap_or(&empty),
        };
        let scratch = std::env::temp_dir();
        let mut analyses: Vec<Analysis> = self
            .options
            .analyzers
            .iter()
            .map(|analyzer| analyzer.analyze(&scratch, &subject))
            .collect();
        for analysis in &mut analyses {
            analysis.error = mask(&analysis.error, &self.secrets);
            for finding in &mut analysis.findings {
                finding.message = mask(&finding.message, &self.secrets);
            }
        }
        self.options
            .baseline
            .apply(&mut analyses, &report.id.name, &report.target);
        analyses
    }

    /// Why the analyses fail the stage, if they do: an analyzer that could
    /// not look comes first, then a finding at the run's threshold.
    fn found_wanting(&self, analyses: &[Analysis]) -> Option<(Reason, String)> {
        if let Some(failed) = analyses.iter().find(|a| !a.asked) {
            return Some((Reason::AnalyzerFailed, failed.error.clone()));
        }
        let threshold = self.options.fail_on.threshold()?;
        let failing: Vec<String> = analyses
            .iter()
            .flat_map(|analysis| analysis.failing(Some(threshold)))
            .map(|finding| {
                let place = finding.place();
                let at = if place.is_empty() {
                    String::new()
                } else {
                    format!(" at {place}")
                };
                format!(
                    "{} {} ({}){at}: {}",
                    finding.severity, finding.rule, finding.analyzer, finding.message
                )
            })
            .collect();
        if failing.is_empty() {
            return None;
        }
        Some((
            Reason::AnalysisFailed,
            format!(
                "an analyzer found something at severity {} or above, which this run fails on \
                 (--fail-on {}):\n  - {}",
                threshold,
                self.options.fail_on.as_str(),
                failing.join("\n  - ")
            ),
        ))
    }

    /// Asks the run's policy about a stage that has compiled.
    fn decide(
        &self,
        policy: &Policy,
        stage: &Stage<'_>,
        report: &StageReport,
        category: Category,
        graph: &meridian_sdk::ResourceGraph,
        plan: Option<&Plan>,
    ) -> Decisions {
        let empty = Artifact::default();
        let input = policy::input(&policy::Subject {
            mode: self.mode.as_str(),
            run: &self.run_id,
            kind: report.id.kind.as_str(),
            name: &report.id.name,
            target: &report.target,
            category: category.as_str(),
            host: &report.host,
            labels: stage.labels(),
            graph,
            artifact: report.artifact.as_ref().unwrap_or(&empty),
            plan,
        });
        let mut decisions = policy.check(&input);
        for message in decisions.deny.iter_mut().chain(decisions.warn.iter_mut()) {
            *message = mask(message, &self.secrets);
        }
        decisions
    }

    /// Keeps the artifact a stage was just applied from, and lets go of the
    /// ones a rollback could no longer want: all but this one and the last
    /// good one before it.
    fn snapshot(&mut self, id: &StageId, artifact: &Artifact, previous: &str) {
        let hash = artifact_hash(artifact);
        if hash == previous {
            return;
        }
        let kept = history::save(&self.options.state_dir, id, artifact)
            .and_then(|()| history::prune(&self.options.state_dir, id, &[&hash, previous]));
        if let Err(why) = kept {
            self.warnings.push(format!(
                "the artifact of {id} could not be kept, so this apply cannot be rolled back \
                 to later: {why}"
            ));
        }
    }

    /// The host a ResourceSet configures, and the values it receives from the
    /// Infrastructure document it names.
    pub(crate) fn host_of(&mut self, doc: &ResourceSet) -> Result<(String, Data), Halt> {
        let Some(target_host) = &doc.spec.target_host else {
            return Err(Halt::Failed(format!(
                "document {:?} declares no targetHost",
                doc.metadata.name
            )));
        };
        if target_host.from != TARGET_HOST_FROM_INFRASTRUCTURE {
            return Ok((target_host.host.clone(), Data::new()));
        }

        let source = StageId {
            kind: StageKind::Infrastructure,
            name: target_host.r#ref.clone(),
        };
        let wanted = target_host.output.clone();
        if !self.outputs.contains_key(&source.name) {
            self.read_outputs(&source);
        }
        let Some(outputs) = self.outputs.get(&source.name) else {
            return Err(Halt::Deferred {
                waiting_on: source,
                output: wanted,
            });
        };
        let Some(output) = outputs.iter().find(|o| o.name == wanted) else {
            let have: Vec<&str> = outputs.iter().map(|o| o.name.as_str()).collect();
            return Err(Halt::Failed(format!(
                "{source} did not produce the output {wanted:?} that this document's \
                 targetHost needs (it produced: {})",
                if have.is_empty() {
                    "nothing".to_owned()
                } else {
                    have.join(", ")
                }
            )));
        };
        let Value::String(host) = &output.value else {
            return Err(Halt::Failed(format!(
                "output {wanted:?} of {source} is {}, and a target host has to be a string",
                output.value.describe()
            )));
        };
        // The output is whatever the tool or the cloud returned. It is
        // about to be handed to ssh, an inventory or a manifest, so it is
        // held to what a host name can be before any tool sees it.
        if let Err(why) = meridian_sdk::check_host(host) {
            return Err(Halt::Failed(format!(
                "output {wanted:?} of {source} cannot be this document's target host: {why}"
            )));
        }
        let inputs = outputs
            .iter()
            .map(|o| (o.name.clone(), o.value.clone()))
            .collect();
        Ok((host.clone(), inputs))
    }

    /// Asks an Infrastructure document's tool what its last apply produced.
    /// This is how a plan, or an apply of a later run, sees infrastructure
    /// that already exists. Anything short of an answer leaves the outputs
    /// unknown, which defers whoever needs them.
    fn read_outputs(&mut self, source: &StageId) {
        let Some(doc) = self.bundle.infrastructure(&source.name) else {
            return;
        };
        let Some(target) = self.registry.target(&doc.spec.target) else {
            return;
        };
        if !target.info().operations.outputs {
            return;
        }
        let Ok(context) = self.context(
            source,
            &doc.spec.target,
            "",
            Data::new(),
            meridian_sdk::Secrets::none(),
            None,
        ) else {
            return;
        };
        if let Ok(outputs) = target.outputs(&context) {
            self.learn(source, &outputs);
        }
    }

    /// Remembers a sensitive value, so that it is masked wherever it is
    /// shown. A tool as often prints a value the way JSON writes it, with
    /// its line breaks and quotes escaped, so that form is masked too.
    fn keep_secret(&mut self, text: &str) {
        // Too short to search for without mangling unrelated text.
        if text.len() < 4 {
            return;
        }
        let quoted = Value::from(text).canonical();
        let escaped = quoted
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(text);
        for form in [text, escaped] {
            if !self.secrets.iter().any(|known| known == form) {
                self.secrets.push(form.to_owned());
            }
        }
    }

    pub(crate) fn learn(&mut self, id: &StageId, outputs: &[Output]) {
        for output in outputs.iter().filter(|o| o.sensitive) {
            // Only strings are masked: a sensitive number or flag is too
            // short to search for without mangling unrelated text.
            if let Value::String(s) = &output.value {
                self.keep_secret(s);
            }
        }
        if id.kind == StageKind::Infrastructure {
            self.outputs.insert(id.name.clone(), outputs.to_vec());
        }
    }

    pub(crate) fn context(
        &self,
        id: &StageId,
        target: &str,
        host: &str,
        inputs: Data,
        secrets: meridian_sdk::Secrets,
        credential: Option<meridian_sdk::HeldCredential>,
    ) -> Result<RunContext, String> {
        // The bundle was validated, which refuses a name that is not one
        // path segment; this keeps a caller that skipped it inside the
        // state directory all the same.
        if !meridian_ir::is_document_name(&id.name) {
            return Err(format!(
                "{:?} is not a safe directory name; run `meridian validate`",
                id.name
            ));
        }
        let work_dir = work_dir(&self.options.state_dir, id, target);
        std::fs::create_dir_all(&work_dir)
            .map_err(|e| format!("creating the work directory {}: {e}", work_dir.display()))?;
        // A plugin is a separate process with its own working directory, so
        // it is always given an absolute path, in the form a tool can take.
        let work_dir = meridian_sdk::tool::absolute_path(&work_dir);
        Ok(RunContext {
            document: id.name.clone(),
            host: host.to_owned(),
            work_dir,
            inputs,
            secrets,
            credential,
            save_plan: false,
            saved_plan: None,
            debug_level: 0,
        })
    }

    /// What the saved plan this apply carries out says about a stage, when
    /// it carries one out.
    fn planned(&self, id: &StageId) -> Option<&PlannedStage> {
        self.options
            .from_plan
            .as_ref()
            .and_then(|plan| plan.stage(id))
    }

    /// The data files that changed since the plan was taken, as words to
    /// put after a `plan-differs` failure; empty when none did.
    fn changed_data(&self) -> String {
        let changed = self
            .options
            .from_plan
            .as_ref()
            .map(|plan| plan.changed_data.as_slice())
            .unwrap_or_default();
        match changed {
            [] => String::new(),
            [one] => format!(", and the data file {one} has changed since the plan"),
            many => format!(
                ", and the data files {} have changed since the plan",
                many.join(", ")
            ),
        }
    }

    pub(crate) fn record(&mut self, id: &StageId, target: &str, hash: &str, applied: bool) {
        let last_good = if applied {
            hash.to_owned()
        } else {
            self.state
                .get(id)
                .map(|record| record.last_good.clone())
                .unwrap_or_default()
        };
        self.state.set(
            id,
            StageRecord {
                target: target.to_owned(),
                artifact_hash: hash.to_owned(),
                applied,
                run: self.run_id.clone(),
                last_good,
            },
        );
        // The tool has done its work whether or not this is written, so a
        // failure here is said out loud and does not fail the stage.
        if let Err(why) = self.state.write(self.backend.as_ref()) {
            self.warnings
                .push(format!("the run-state could not be written: {why}"));
        }
    }

    /// Whether a resumed run can leave this stage alone, and if so which run
    /// applied it.
    ///
    /// It can when the run being continued applied the stage, the run-state
    /// agrees, and the document still compiles to the artifact that was
    /// applied. A stage whose outputs later stages may need must also be
    /// able to report them: if its tool cannot, the stage is applied again,
    /// because an apply is idempotent and a guess is not.
    fn already_applied(&mut self, id: &StageId, hash: &str, has_outputs: bool) -> Option<String> {
        let resumed = self.resume.as_ref()?;
        if resumed.done.get(id).map(String::as_str) != Some(hash) {
            return None;
        }
        let record = self.state.get(id)?;
        if !record.applied || record.artifact_hash != hash {
            return None;
        }
        let applied_in = if record.run.is_empty() {
            resumed.run.clone()
        } else {
            record.run.clone()
        };
        if id.kind == StageKind::Infrastructure && has_outputs {
            self.read_outputs(id);
            if !self.outputs.contains_key(&id.name) {
                return None;
            }
        }
        Some(applied_in)
    }

    pub(crate) fn describe(&self, failure: RunFailure) -> String {
        self.masker.mask(&mask(&failure.to_string(), &self.secrets))
    }

    /// Whether the operator has asked for the run to stop.
    fn interrupted(&self) -> bool {
        self.options
            .stop
            .when
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// What a denied stage says.
pub(crate) fn denial(decisions: &Decisions) -> String {
    if !decisions.asked {
        return decisions.deny.join("; ");
    }
    format!(
        "the policy does not allow this change:\n  - {}",
        decisions.deny.join("\n  - ")
    )
}

/// Why a run of a tool produced no result.
pub(crate) fn reason_of(failure: &RunFailure) -> Reason {
    match failure {
        RunFailure::Tool(_) => Reason::ToolFailed,
        RunFailure::Plugin(_) => Reason::PluginFailed,
        RunFailure::Stopped(stopped) => match stopped.cause {
            StopCause::Interrupted => Reason::Interrupted,
            StopCause::TimedOut(_) => Reason::TimedOut,
        },
    }
}

/// Where a stage's tool keeps its state: one directory per document and
/// target, so changing a document's target never points a new tool at the
/// old one's files.
pub fn work_dir(state_dir: &Path, id: &StageId, target: &str) -> PathBuf {
    let kind = match id.kind {
        StageKind::Infrastructure => "infrastructure",
        StageKind::ResourceSet => "resourceset",
    };
    state_dir.join(kind).join(&id.name).join(target)
}
