//! The run's journal: the lock it holds, the record it writes as it goes,
//! and what a resumed run reads back from the one it continues.

use super::*;

/// Releases the lock when a run ends without doing so itself.
pub(crate) struct LockGuard {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) id: String,
    pub(crate) held: bool,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.held {
            let _ = lock::release(self.backend.as_ref(), &self.id);
        }
    }
}

/// The run an apply continues: its id, and the artifact each of its stages
/// was applied from.
pub(crate) struct Resumed {
    pub(crate) run: String,
    pub(crate) done: BTreeMap<StageId, String>,
}

impl Resumed {
    pub(crate) fn find(backend: &dyn Backend, resume: &Resume) -> Result<Resumed, String> {
        let record = match resume {
            Resume::Last => record::last_run(backend, Some(Mode::Apply.as_str()))?
                .ok_or("there is no apply to resume: none has been recorded in this state")?,
            Resume::Run(id) => record::load_run(backend, id)?
                .ok_or_else(|| format!("there is no run {id} in this state"))?,
        };
        if record.mode != Mode::Apply.as_str() {
            return Err(format!(
                "run {} was a {}, and only an apply can be resumed",
                record.id, record.mode
            ));
        }
        if record.status == RunStatus::Succeeded {
            return Err(format!(
                "run {} succeeded, so there is nothing to resume; run `meridian apply` to \
                 apply again",
                record.id
            ));
        }
        let done = record
            .steps
            .iter()
            .filter(|step| {
                matches!(step.status, StepStatus::Succeeded | StepStatus::Skipped)
                    && !step.artifact.is_empty()
            })
            .map(|step| (step.id.clone(), step.artifact.clone()))
            .collect();
        Ok(Resumed {
            run: record.id,
            done,
        })
    }
}

/// Collects a run's events and keeps its record.
pub(crate) struct Journal {
    pub(crate) backend: Arc<dyn Backend>,
    /// Whether the record is kept at all. A compile leaves none.
    pub(crate) persist: bool,
    pub(crate) record: RunRecord,
    pub(crate) events: Vec<Event>,
    pub(crate) warnings: Vec<String>,
    /// At what severity a finding fails a stage, for what the analysis
    /// events say.
    pub(crate) fail_on: FailOn,
}

impl Journal {
    pub(crate) fn emit(
        &mut self,
        observer: &mut dyn Observer,
        kind: &str,
        step: Option<(&StageId, &str, &str)>,
        detail: Vec<(&str, Value)>,
    ) {
        let (name, target, endpoint) = match step {
            Some((id, target, endpoint)) => (id.name.as_str(), target, endpoint),
            None => ("", "", ""),
        };
        let mut detail: BTreeMap<String, Value> = detail
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        if let Some((id, _, _)) = step {
            detail.insert("kind".to_owned(), Value::from(id.kind.as_str()));
        }
        let event = Event {
            run: self.record.id.clone(),
            seq: self.events.len() as u64 + 1,
            time: time::now(),
            kind: kind.to_owned(),
            workflow: String::new(),
            step: name.to_owned(),
            instance: name.to_owned(),
            endpoint: endpoint.to_owned(),
            target: target.to_owned(),
            attempt: u32::from(step.is_some()),
            detail,
        };
        observer.event(&event);
        self.events.push(event);
    }

    /// Writes the record as it now stands. A record that cannot be written
    /// is a warning and not a failed stage: the tool has done its work
    /// either way.
    pub(crate) fn flush(&mut self) {
        if !self.persist {
            return;
        }
        let id = self.record.id.clone();
        let wrote = self
            .backend
            .put(&record::run_key(&id), self.record.to_json().as_bytes())
            .and_then(|()| {
                self.backend.put(
                    &record::events_key(&id),
                    event::to_jsonl(&self.events).as_bytes(),
                )
            });
        if let Err(why) = wrote {
            let warning = format!("the record of run {id} could not be written: {why}");
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
    }

    pub(crate) fn started(&mut self, observer: &mut dyn Observer, id: &StageId, target: &str) {
        self.emit(
            observer,
            event::STEP_STARTED,
            Some((id, target, "")),
            Vec::new(),
        );
        self.record.set_step(StepRecord {
            id: id.clone(),
            target: target.to_owned(),
            endpoint: String::new(),
            status: StepStatus::Running,
            artifact: String::new(),
            changes: 0,
            reason: String::new(),
            previous: String::new(),
        });
        self.flush();
    }

    pub(crate) fn finished(&mut self, observer: &mut dyn Observer, report: &StageReport) {
        let at = Some((&report.id, report.target.as_str(), report.host.as_str()));
        let artifact = report
            .artifact
            .as_ref()
            .map(artifact_hash)
            .unwrap_or_default();
        let mut step = StepRecord {
            id: report.id.clone(),
            target: report.target.clone(),
            endpoint: report.host.clone(),
            status: StepStatus::Succeeded,
            artifact: artifact.clone(),
            changes: 0,
            reason: String::new(),
            previous: report.previous_good.clone(),
        };
        let resource = |change: &Change| {
            vec![
                ("resource", Value::from(change.resource.as_str())),
                ("action", Value::from(change.action.as_str())),
            ]
        };
        let hash = ("artifact", Value::from(artifact.as_str()));
        if let Some(policy) = &report.policy {
            // How many, and never what they said: a message can quote the
            // document it is about.
            self.emit(
                observer,
                if policy.allowed() {
                    event::POLICY_PASSED
                } else {
                    event::POLICY_DENIED
                },
                at,
                vec![
                    ("denials", Value::Int(policy.deny.len() as i64)),
                    ("warnings", Value::Int(policy.warn.len() as i64)),
                    ("asked", Value::Bool(policy.asked)),
                ],
            );
        }
        for analysis in &report.analyses {
            // Every finding is an event of its own, so a report or a sink
            // can say which rule fired where; the message is the analyzer's
            // description of its rule, masked of every value the run knows
            // to be sensitive.
            for finding in &analysis.findings {
                self.emit(
                    observer,
                    event::FINDING_REPORTED,
                    at,
                    vec![
                        ("analyzer", Value::from(analysis.analyzer.as_str())),
                        ("rule", Value::from(finding.rule.as_str())),
                        ("severity", Value::from(finding.severity.as_str())),
                        ("message", Value::from(finding.message.as_str())),
                        ("file", Value::from(finding.file.as_str())),
                        ("line", Value::Uint(finding.line)),
                        ("resource", Value::from(finding.resource.as_str())),
                        ("document", Value::from(finding.document.as_str())),
                        ("documentLine", Value::Uint(finding.document_line)),
                        ("url", Value::from(finding.url.as_str())),
                        ("accepted", Value::Bool(finding.accepted)),
                    ],
                );
            }
            let counts = analysis.counts();
            let threshold = self.fail_on.threshold();
            let mut detail = vec![
                ("analyzer", Value::from(analysis.analyzer.as_str())),
                ("asked", Value::Bool(analysis.asked)),
                ("findings", Value::Uint(counts.total())),
                ("critical", Value::Uint(counts.critical)),
                ("high", Value::Uint(counts.high)),
                ("medium", Value::Uint(counts.medium)),
                ("low", Value::Uint(counts.low)),
                ("info", Value::Uint(counts.info)),
                ("accepted", Value::Uint(counts.accepted)),
                (
                    "failing",
                    Value::Uint(analysis.failing(threshold).len() as u64),
                ),
            ];
            if !analysis.measures.is_empty() {
                detail.push((
                    "measures",
                    Value::List(
                        analysis
                            .measures
                            .iter()
                            .map(|m| {
                                Value::Map(
                                    [
                                        ("name".to_owned(), Value::from(m.name.as_str())),
                                        ("value".to_owned(), Value::Float(m.value)),
                                        ("unit".to_owned(), Value::from(m.unit.as_str())),
                                    ]
                                    .into(),
                                )
                            })
                            .collect(),
                    ),
                ));
            }
            self.emit(
                observer,
                if analysis.passed(threshold) {
                    event::ANALYSIS_PASSED
                } else {
                    event::ANALYSIS_FAILED
                },
                at,
                detail,
            );
        }
        match &report.outcome {
            Outcome::Compiled => {
                self.emit(observer, event::STEP_SUCCEEDED, at, vec![hash]);
            }
            Outcome::Planned(plan) => {
                for change in &plan.changes {
                    self.emit(observer, event::RESOURCE_PLANNED, at, resource(change));
                }
                step.changes = plan.changes.len() as u64;
                self.emit(
                    observer,
                    event::STEP_SUCCEEDED,
                    at,
                    vec![
                        hash,
                        ("changes", Value::Int(step.changes as i64)),
                        ("unknown", Value::Bool(plan.unknown)),
                    ],
                );
            }
            Outcome::Applied { changes, outputs } => {
                for change in changes {
                    self.emit(observer, event::RESOURCE_CHANGED, at, resource(change));
                }
                for output in outputs {
                    // The name, and that it is sensitive. Never the value:
                    // a tool does not always know which of its values are
                    // secret, and the record must not find out the hard way.
                    self.emit(
                        observer,
                        event::OUTPUT_PRODUCED,
                        at,
                        vec![
                            ("name", Value::from(output.name.as_str())),
                            ("sensitive", Value::Bool(output.sensitive)),
                        ],
                    );
                }
                step.changes = changes.len() as u64;
                self.emit(
                    observer,
                    event::STEP_SUCCEEDED,
                    at,
                    vec![hash, ("changes", Value::Int(step.changes as i64))],
                );
            }
            Outcome::Drift(drift) => {
                let (kind, reason, changes): (&str, &str, &[Change]) = match drift {
                    Drift::InSync => (event::DRIFT_NONE, "", &[]),
                    Drift::Drifted(changes) => (event::DRIFT_DETECTED, "", changes),
                    Drift::NeverApplied => (event::DRIFT_UNKNOWN, "never-applied", &[]),
                    Drift::Unknown => (event::DRIFT_UNKNOWN, "no-preview", &[]),
                    Drift::Pending {
                        changes,
                        last_apply_failed,
                    } => (
                        event::DRIFT_UNKNOWN,
                        if *last_apply_failed {
                            "last-apply-failed"
                        } else {
                            "document-changed"
                        },
                        changes,
                    ),
                };
                for change in changes {
                    self.emit(observer, event::RESOURCE_PLANNED, at, resource(change));
                }
                step.changes = changes.len() as u64;
                let mut detail = vec![hash.clone(), ("changes", Value::Int(step.changes as i64))];
                if !reason.is_empty() {
                    detail.push(("reason", Value::from(reason)));
                }
                self.emit(observer, kind, at, detail);
                self.emit(
                    observer,
                    event::STEP_SUCCEEDED,
                    at,
                    vec![hash, ("changes", Value::Int(step.changes as i64))],
                );
            }
            Outcome::Skipped { applied_in } => {
                step.status = StepStatus::Skipped;
                self.emit(
                    observer,
                    event::STEP_SKIPPED,
                    at,
                    vec![
                        ("reason", Value::from("already-applied")),
                        hash,
                        ("appliedIn", Value::from(applied_in.as_str())),
                    ],
                );
            }
            Outcome::LeftOut { .. } => {
                step.status = StepStatus::Skipped;
                self.emit(
                    observer,
                    event::STEP_SKIPPED,
                    at,
                    vec![("reason", Value::from("when-false"))],
                );
            }
            Outcome::Deferred { waiting_on, output } => {
                step.status = StepStatus::Deferred;
                self.emit(
                    observer,
                    event::STEP_DEFERRED,
                    at,
                    vec![
                        ("waitingOn", Value::from(waiting_on.name.as_str())),
                        ("output", Value::from(output.as_str())),
                    ],
                );
            }
            Outcome::Failed(_) => {
                for change in &report.done_before_failing {
                    self.emit(observer, event::RESOURCE_CHANGED, at, resource(change));
                }
                // Which resource and why, as fixed words: the tool's own
                // text about it is kept on this machine only.
                for failed in &report.failures {
                    self.emit(
                        observer,
                        event::RESOURCE_FAILED,
                        at,
                        vec![
                            ("resource", Value::from(failed.failure.resource.as_str())),
                            ("cause", Value::from(failed.failure.cause.as_str())),
                        ],
                    );
                }
                step.status = StepStatus::Failed;
                step.reason = report.reason.map_or_else(String::new, |r| r.to_string());
                step.changes = report.done_before_failing.len() as u64;
                // The reason is a fixed word. What the tool said stays on
                // the terminal, because a tool can quote a value.
                self.emit(
                    observer,
                    event::STEP_FAILED,
                    at,
                    vec![
                        (
                            "reason",
                            Value::from(report.reason.map_or("", Reason::as_str)),
                        ),
                        hash,
                        ("changes", Value::Int(step.changes as i64)),
                    ],
                );
            }
            Outcome::NotRun { because_of } => {
                step.status = StepStatus::NotRun;
                self.emit(
                    observer,
                    event::STEP_SKIPPED,
                    at,
                    vec![
                        ("reason", Value::from("earlier-step-failed")),
                        ("becauseOf", Value::from(because_of.name.as_str())),
                    ],
                );
            }
        }
        self.record.set_step(step);
        self.flush();
    }
}
