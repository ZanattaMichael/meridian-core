//! Putting back what an apply changed.
//!
//! A core that knows no tool has one honest way to undo a stage: apply the
//! artifact of its last successful apply again. That is what happens here,
//! to every stage a run reached a tool with, last first: the stage that
//! failed, then the ones before it.
//!
//! What it cannot do is said out loud, never skipped over in silence:
//!
//! - A stage applied for the first time has nothing to go back to. Removing
//!   what it created would need a `destroy` operation the plugin protocol
//!   does not have, so it is left as it is and reported.
//! - The artifact to go back to must still be on this machine
//!   ([`crate::history`]). If it is not, the stage is reported with what to
//!   do by hand.
//! - Applying an old artifact restores what the tool manages. It does not
//!   bring back data a tool destroyed on the way.
//!
//! A rollback that fails stops there. Nothing further is put back, because
//! the stages before it were built on the one that would not go back.

use meridian_ir::Document;

use crate::Stop;
use meridian_sdk::{Change, Data, LogLine, RunContext, Value};
use meridian_secret::Masker;

use crate::backend::Backend;
use crate::record::{self, RunRecord, StepRecord, StepStatus};
use crate::{
    event, history, mask, reason_of, Halt, Journal, Mode, Observer, Resume, Run, StageId, StageKind,
};

/// A stage a run handed to a tool to apply, and what is needed to put it
/// back.
pub(crate) struct Touched {
    pub id: StageId,
    pub target: String,
    /// Where and on what the stage was applied.
    pub context: RunContext,
    /// The artifact hash of the stage's last successful apply before the
    /// run; empty when there was none.
    pub previous: String,
    /// The artifact hash the run applied, or tried to.
    pub attempted: String,
    /// A filter for what the tool prints, holding the secrets handed to it.
    pub masker: Masker,
}

/// One stage of a rollback: ready to be put back, or already known not to
/// be possible.
pub(crate) enum Item {
    Ready(Touched),
    Unavailable(RollbackReport, &'static str),
}

/// What became of one stage of a rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RollbackReport {
    pub id: StageId,
    pub target: String,
    /// The machine the stage configures, when there is one.
    pub host: String,
    pub outcome: RollbackOutcome,
}

/// How putting a stage back ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// The stage was applied again from the artifact with the hash `to`.
    RolledBack { to: String, changes: Vec<Change> },
    /// The run applied the artifact that was already the stage's last good
    /// one, so there is nothing to put back.
    Unchanged,
    /// The stage could not be put back, and was left as it is. The text
    /// says why, and what to do about it.
    Unavailable(String),
    /// Applying the earlier artifact failed. The rollback stopped here.
    Failed(String),
    /// An earlier stage of the rollback failed, so this one was not
    /// attempted.
    NotAttempted { because_of: StageId },
}

impl RollbackOutcome {
    /// Whether the stage is where a rollback means it to be.
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            RollbackOutcome::RolledBack { .. } | RollbackOutcome::Unchanged
        )
    }
}

/// The apply a rollback puts back.
pub(crate) fn find(backend: &dyn Backend, which: &Resume) -> Result<RunRecord, String> {
    let record = match which {
        Resume::Last => record::last_run(backend, Some(Mode::Apply.as_str()))?
            .ok_or("there is no apply to roll back: none has been recorded in this state")?,
        Resume::Run(id) => record::load_run(backend, id)?
            .ok_or_else(|| format!("there is no run {id} in this state"))?,
    };
    if record.mode != Mode::Apply.as_str() {
        return Err(format!(
            "run {} was a {}, and only an apply can be rolled back",
            record.id, record.mode
        ));
    }
    if !record.steps.iter().any(reached_a_tool) {
        return Err(format!(
            "run {} has nothing to roll back: no stage it applied is left to put back",
            record.id
        ));
    }
    Ok(record)
}

/// Whether a recorded step is one its run handed to a tool to apply.
fn reached_a_tool(step: &StepRecord) -> bool {
    match step.status {
        StepStatus::Succeeded => true,
        // A step can fail before it reaches its tool, when the document is
        // refused. One that got as far as the tool has an artifact, and a
        // reason that says the tool or its plugin failed.
        StepStatus::Failed => {
            !step.artifact.is_empty()
                && (step.reason == crate::REASON_TOOL_FAILED
                    || step.reason == crate::REASON_PLUGIN_FAILED)
        }
        // Already put back, never applied, or left alone by a resume.
        _ => false,
    }
}

/// What an earlier run changed, as stages to put back, in the order that
/// run applied them.
pub(crate) fn items_of(run: &mut Run<'_>, of: &RunRecord) -> Vec<Item> {
    let mut items = Vec::new();
    for step in of.steps.iter().filter(|step| reached_a_tool(step)) {
        let unavailable = |why: String, reason: &'static str| {
            Item::Unavailable(
                RollbackReport {
                    id: step.id.clone(),
                    target: step.target.clone(),
                    host: step.endpoint.clone(),
                    outcome: RollbackOutcome::Unavailable(why),
                },
                reason,
            )
        };
        // The stage must still be as that run left it. If a later run has
        // applied it, that run's work is what is there now, and undoing an
        // older one over the top of it would undo the wrong thing.
        let state = run.state.get(&step.id);
        if state.map(|s| s.run.as_str()) != Some(of.id.as_str()) {
            let since = state.map(|s| s.run.clone()).unwrap_or_default();
            items.push(unavailable(
                format!(
                    "it has been applied again since run {}{}, so that run is no longer what \
                     there is to put back",
                    of.id,
                    if since.is_empty() {
                        String::new()
                    } else {
                        format!(", by run {since}")
                    }
                ),
                "applied-since",
            ));
            continue;
        }
        let document = run
            .bundle
            .documents
            .iter()
            .find(|doc| match (doc, step.id.kind) {
                (Document::Infrastructure(d), StageKind::Infrastructure) => {
                    d.metadata.name == step.id.name
                }
                (Document::ResourceSet(d), StageKind::ResourceSet) => {
                    d.metadata.name == step.id.name
                }
                _ => false,
            });
        let Some(document) = document else {
            items.push(unavailable(
                format!(
                    "its document is not among the files given; give the files run {} was \
                     given",
                    of.id
                ),
                "document-missing",
            ));
            continue;
        };
        // Where the stage runs is worked out as an apply would: a
        // ResourceSet's host from the stage that provides it, which has not
        // been put back yet, because stages go back last first.
        let (host, inputs) = match document {
            Document::ResourceSet(doc) => match run.host_of(doc) {
                Ok(found) => found,
                Err(Halt::Failed(why)) => {
                    items.push(unavailable(why, "host-unknown"));
                    continue;
                }
                Err(Halt::Deferred { waiting_on, output }) => {
                    items.push(unavailable(
                        format!(
                            "its host is the output {output:?} of {waiting_on}, whose tool can \
                             no longer report it"
                        ),
                        "host-unknown",
                    ));
                    continue;
                }
            },
            _ => (String::new(), Data::new()),
        };
        // And what it is handed from other documents, likewise.
        let mut inputs = inputs;
        match run.imports_of(document) {
            Ok(imports) => inputs.extend(imports),
            Err(Stop::Failed { why, .. }) => {
                items.push(unavailable(why, "imports-unknown"));
                continue;
            }
            Err(Stop::Deferred { waiting_on, output }) => {
                items.push(unavailable(
                    format!(
                        "it imports the output {output:?} of {waiting_on}, whose tool can no \
                         longer report it"
                    ),
                    "imports-unknown",
                ));
                continue;
            }
        }
        let context = match run.context(
            &step.id,
            &step.target,
            &host,
            inputs,
            meridian_sdk::Secrets::none(),
        ) {
            Ok(context) => context,
            Err(why) => {
                items.push(unavailable(why, "state-failed"));
                continue;
            }
        };
        items.push(Item::Ready(Touched {
            id: step.id.clone(),
            target: step.target.clone(),
            context,
            previous: step.previous.clone(),
            attempted: step.artifact.clone(),
            masker: Masker::new(),
        }));
    }
    items
}

/// Puts stages back, the last first, and says what became of each.
pub(crate) fn execute(
    run: &mut Run<'_>,
    journal: &mut Journal,
    observer: &mut dyn Observer,
    items: Vec<Item>,
) -> Vec<RollbackReport> {
    let mut reports = Vec::new();
    let mut stopped: Option<StageId> = None;
    for item in items.into_iter().rev() {
        let (report, reason) = match item {
            Item::Unavailable(report, reason) => (report, reason),
            Item::Ready(touched) => match &stopped {
                Some(because_of) => (
                    RollbackReport {
                        id: touched.id,
                        target: touched.target,
                        host: touched.context.host,
                        outcome: RollbackOutcome::NotAttempted {
                            because_of: because_of.clone(),
                        },
                    },
                    "earlier-rollback-failed",
                ),
                None => one(run, journal, observer, touched),
            },
        };
        let at = Some((&report.id, report.target.as_str(), report.host.as_str()));
        match &report.outcome {
            RollbackOutcome::Failed(_) => stopped = Some(report.id.clone()),
            RollbackOutcome::Unavailable(_) | RollbackOutcome::NotAttempted { .. } => {
                journal.emit(
                    observer,
                    event::ROLLBACK_UNAVAILABLE,
                    at,
                    vec![("reason", Value::from(reason))],
                );
            }
            RollbackOutcome::RolledBack { .. } | RollbackOutcome::Unchanged => {}
        }
        if let RollbackOutcome::RolledBack { to, changes } = &report.outcome {
            let step = StepRecord {
                id: report.id.clone(),
                target: report.target.clone(),
                endpoint: report.host.clone(),
                status: StepStatus::RolledBack,
                artifact: to.clone(),
                changes: changes.len() as u64,
                reason: String::new(),
                previous: String::new(),
            };
            journal.record.set_step(step);
        }
        journal.flush();
        observer.rollback_finished(&report);
        reports.push(report);
    }
    reports
}

/// The secrets the document of stage `id` declares, fetched now, for the
/// tool putting the stage back: the artifact refers to them by name and
/// needs their values. Every declared secret is handed over, as the artifact
/// may refer to any; one an expression made from them is not among them,
/// and a tool that needs it says so. With them comes a filter for what the
/// tool prints. A document without secrets gives none.
fn secrets_of(run: &Run<'_>, id: &StageId) -> Result<(meridian_sdk::Secrets, Masker), String> {
    let document = run
        .bundle
        .documents
        .iter()
        .find(|doc| match (doc, id.kind) {
            (Document::Infrastructure(d), StageKind::Infrastructure) => d.metadata.name == id.name,
            (Document::ResourceSet(d), StageKind::ResourceSet) => d.metadata.name == id.name,
            _ => false,
        });
    let Some(document) = document.filter(|doc| !doc.secrets().is_empty()) else {
        return Ok((meridian_sdk::Secrets::none(), Masker::new()));
    };
    let mut secrets = meridian_ir::Secrets::declared(document);
    crate::secrets::fetch(document, &mut secrets)?;
    let handed = crate::secrets::handed_by_name(&secrets)?;
    Ok((handed, secrets.masker()))
}

/// Puts one stage back.
fn one(
    run: &mut Run<'_>,
    journal: &mut Journal,
    observer: &mut dyn Observer,
    touched: Touched,
) -> (RollbackReport, &'static str) {
    let Touched {
        id,
        target: target_name,
        mut context,
        previous,
        attempted,
        mut masker,
    } = touched;
    let report = |outcome| RollbackReport {
        id: id.clone(),
        target: target_name.clone(),
        host: context.host.clone(),
        outcome,
    };
    let unavailable = |why: String| report(RollbackOutcome::Unavailable(why));

    if previous.is_empty() {
        return (
            unavailable(
                "it had never been applied successfully before, so there is nothing to go \
                 back to; what was created is left as it is, and removing it is the tool's \
                 own job"
                    .to_owned(),
            ),
            "first-apply",
        );
    }
    if previous == attempted {
        return (report(RollbackOutcome::Unchanged), "");
    }
    let by_hand = "check out the revision of the document that was applied before, and apply it";
    let artifact = match history::load(&run.options.state_dir, &id, &previous) {
        Ok(Some(artifact)) => artifact,
        Ok(None) => {
            return (
                unavailable(format!(
                    "the artifact it was last applied from successfully is not kept on this \
                     machine; {by_hand}"
                )),
                "no-snapshot",
            )
        }
        Err(why) => return (unavailable(format!("{why}; {by_hand}")), "no-snapshot"),
    };
    let Some(target) = run.registry.target(&target_name) else {
        return (
            unavailable(format!("no plugin provides target {target_name:?}")),
            "no-plugin",
        );
    };

    // The secrets are fetched at the stage's turn and held for it alone,
    // as an apply holds them.
    if context.secrets.is_empty() {
        match secrets_of(run, &id) {
            Ok((handed, filter)) => {
                context.secrets = handed;
                masker = filter;
            }
            Err(why) => return (unavailable(why), "secrets-unavailable"),
        }
    }

    let at = Some((&id, target_name.as_str(), context.host.as_str()));
    journal.emit(
        observer,
        event::ROLLBACK_STARTED,
        at,
        vec![("to", Value::from(previous.as_str()))],
    );
    let secrets = run.secrets.clone();
    let mut log = |mut line: LogLine| {
        line.text = masker.mask(&mask(&line.text, &secrets));
        observer.log(&id, &line);
    };
    let outcome = target.apply(&context, &artifact, &mut log);
    let resource = |change: &Change| {
        vec![
            ("resource", Value::from(change.resource.as_str())),
            ("action", Value::from(change.action.as_str())),
        ]
    };
    match outcome {
        Ok(applied) => {
            run.record(&id, &target_name, &previous, true);
            run.learn(&id, &applied.outputs);
            for change in &applied.changes {
                journal.emit(observer, event::RESOURCE_CHANGED, at, resource(change));
            }
            journal.emit(
                observer,
                event::ROLLBACK_SUCCEEDED,
                at,
                vec![
                    ("to", Value::from(previous.as_str())),
                    ("changes", Value::Int(applied.changes.len() as i64)),
                ],
            );
            (
                report(RollbackOutcome::RolledBack {
                    to: previous,
                    changes: applied.changes,
                }),
                "",
            )
        }
        Err(failure) => {
            // The stage is now part way between two artifacts. The
            // run-state says so: the last attempt failed, and the last good
            // artifact is still the one a rollback would go to.
            run.record(&id, &target_name, &previous, false);
            let reason = reason_of(&failure);
            journal.emit(
                observer,
                event::ROLLBACK_FAILED,
                at,
                vec![
                    ("to", Value::from(previous.as_str())),
                    ("reason", Value::from(reason)),
                ],
            );
            (
                report(RollbackOutcome::Failed(run.describe(failure))),
                reason,
            )
        }
    }
}
