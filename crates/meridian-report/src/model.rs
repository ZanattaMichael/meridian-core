//! What a run did, folded from its events.

use meridian_orchestrator::{event, Event, RunRecord};
use meridian_sdk::Value;

use crate::time;

/// How a run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunState {
    /// No event has said how it ended: it is still going, or it died.
    #[default]
    Running,
    Succeeded,
    Failed,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Succeeded => "succeeded",
            RunState::Failed => "failed",
        }
    }
}

/// How a step stands. The words are the run record's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StepState {
    #[default]
    Running,
    Succeeded,
    Failed,
    /// A resumed run found it already applied.
    Skipped,
    /// It waits on a value that does not exist yet.
    Deferred,
    /// An earlier step failed, so it was not attempted.
    NotRun,
    /// It was put back to what was applied before.
    RolledBack,
}

impl StepState {
    pub fn as_str(self) -> &'static str {
        match self {
            StepState::Running => "running",
            StepState::Succeeded => "succeeded",
            StepState::Failed => "failed",
            StepState::Skipped => "skipped",
            StepState::Deferred => "deferred",
            StepState::NotRun => "not-run",
            StepState::RolledBack => "rolled-back",
        }
    }
}

/// Whether a resource's change was previewed or made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Planned,
    Changed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Planned => "planned",
            Phase::Changed => "changed",
        }
    }
}

/// One resource a step would change, or changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The resource's id in its document.
    pub id: String,
    /// What the tool does to it: `create`, `update`, `delete` and so on.
    pub action: String,
    pub phase: Phase,
}

/// An output a step produced. Its name only: a report never has the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepOutput {
    pub name: String,
    pub sensitive: bool,
}

/// One step of the run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    /// `Infrastructure` or `ResourceSet`.
    pub kind: String,
    pub target: String,
    /// The machine the step configures, when there is one.
    pub endpoint: String,
    pub status: StepState,
    /// The fixed word that says why it failed, was skipped or is unknown.
    pub reason: String,
    /// The hash of what the step compiled to.
    pub artifact: String,
    pub started: String,
    pub finished: String,
    /// For a deferred step, the step and the output it waits on.
    pub waiting_on: String,
    pub waiting_for: String,
    /// What a drift check found: `none`, `detected` or `unknown`; empty when
    /// the run was not a drift check.
    pub drift: String,
    /// How putting the step back went: `succeeded`, `failed` or
    /// `unavailable`; empty when it was not rolled back.
    pub rollback: String,
    /// What the run's policy said of the step: `passed` or `denied`; empty
    /// when no policy was asked. With it, how many rules denied and how
    /// many warned. Counts only: a rule's message is not in an event.
    pub policy: String,
    pub policy_denials: u64,
    pub policy_warnings: u64,
    pub resources: Vec<Resource>,
    pub outputs: Vec<StepOutput>,
}

impl Step {
    /// How long the step took, when it both started and ended.
    pub fn duration_ms(&self) -> Option<u64> {
        time::between(&self.started, &self.finished)
    }

    /// The step as a person names it: `Infrastructure network`.
    pub fn title(&self) -> String {
        if self.kind.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.kind, self.name)
        }
    }

    /// Whether the step is a problem: it failed, it could not be put back,
    /// or a drift check found that it drifted.
    pub fn is_problem(&self) -> bool {
        self.status == StepState::Failed || self.rollback == "failed" || self.drift == "detected"
    }
}

/// The counts a report opens with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub steps: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub skipped: u64,
    pub not_run: u64,
    pub deferred: u64,
    pub rolled_back: u64,
    pub running: u64,
    /// Resources previewed as changing.
    pub planned: u64,
    /// Resources changed.
    pub changed: u64,
    /// Steps a drift check found drifted.
    pub drifted: u64,
}

/// What one run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub run: String,
    /// `compile`, `plan`, `apply`, `drift` or `rollback`.
    pub mode: String,
    /// What was typed, as the run recorded it.
    pub command: String,
    pub status: RunState,
    pub started: String,
    pub finished: String,
    /// The run this one continues, and the run it puts back.
    pub resumes: String,
    pub rolls_back: String,
    /// Who ran it and where, when the run's record is at hand. Best effort,
    /// and not an identity.
    pub user: String,
    pub host: String,
    pub selector: String,
    pub steps: Vec<Step>,
    /// How many events the report was folded from.
    pub events: u64,
}

fn text(event: &Event, key: &str) -> String {
    match event.detail.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn count(event: &Event, key: &str) -> u64 {
    match event.detail.get(key) {
        Some(Value::Uint(n)) => *n,
        Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
        _ => 0,
    }
}

impl Report {
    /// The report of a whole stream of events.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Report {
        let mut report = Report::default();
        for event in events {
            report.apply(event);
        }
        report
    }

    /// Adds what the run's record knows and the events do not.
    pub fn with_record(mut self, record: &RunRecord) -> Report {
        self.user = record.user.clone();
        self.host = record.host.clone();
        self.selector = record.selector.clone();
        self
    }

    /// Folds one more event in. An event of a kind this version does not
    /// know is counted and otherwise ignored, so a report can be made from a
    /// newer Meridian's events.
    pub fn apply(&mut self, event: &Event) {
        self.events += 1;
        if self.run.is_empty() {
            self.run = event.run.clone();
        }
        match event.kind.as_str() {
            event::RUN_STARTED => {
                self.started = event.time.clone();
                self.mode = text(event, "mode");
                self.command = text(event, "command");
                self.resumes = text(event, "resumes");
                self.rolls_back = text(event, "rollsBack");
            }
            event::RUN_SUCCEEDED => {
                self.status = RunState::Succeeded;
                self.finished = event.time.clone();
            }
            event::RUN_FAILED => {
                self.status = RunState::Failed;
                self.finished = event.time.clone();
            }
            _ if event.step.is_empty() => {}
            kind => self.apply_to_step(kind, event),
        }
    }

    fn apply_to_step(&mut self, kind: &str, event: &Event) {
        let step_kind = text(event, "kind");
        let at = self
            .steps
            .iter()
            .position(|s| s.name == event.step && s.kind == step_kind)
            .unwrap_or_else(|| {
                self.steps.push(Step {
                    name: event.step.clone(),
                    kind: step_kind,
                    ..Step::default()
                });
                self.steps.len() - 1
            });
        let step = &mut self.steps[at];
        if !event.target.is_empty() {
            step.target = event.target.clone();
        }
        if !event.endpoint.is_empty() {
            step.endpoint = event.endpoint.clone();
        }
        let artifact = text(event, "artifact");
        if !artifact.is_empty() {
            step.artifact = artifact;
        }
        let end = |step: &mut Step, status: StepState| {
            step.status = status;
            step.finished = event.time.clone();
        };
        let resource = |step: &mut Step, phase: Phase| {
            step.resources.push(Resource {
                id: text(event, "resource"),
                action: text(event, "action"),
                phase,
            });
        };
        match kind {
            event::STEP_STARTED => {
                step.started = event.time.clone();
                step.status = StepState::Running;
            }
            event::STEP_SUCCEEDED => end(step, StepState::Succeeded),
            event::STEP_FAILED => {
                step.reason = text(event, "reason");
                end(step, StepState::Failed);
            }
            event::STEP_SKIPPED => {
                step.reason = text(event, "reason");
                if step.reason == "earlier-step-failed" {
                    step.waiting_on = text(event, "becauseOf");
                    end(step, StepState::NotRun);
                } else {
                    end(step, StepState::Skipped);
                }
            }
            event::STEP_DEFERRED => {
                step.waiting_on = text(event, "waitingOn");
                step.waiting_for = text(event, "output");
                end(step, StepState::Deferred);
            }
            event::RESOURCE_PLANNED => resource(step, Phase::Planned),
            event::RESOURCE_CHANGED => resource(step, Phase::Changed),
            event::OUTPUT_PRODUCED => step.outputs.push(StepOutput {
                name: text(event, "name"),
                sensitive: matches!(event.detail.get("sensitive"), Some(Value::Bool(true))),
            }),
            event::DRIFT_NONE => step.drift = "none".to_owned(),
            event::DRIFT_DETECTED => step.drift = "detected".to_owned(),
            event::DRIFT_UNKNOWN => {
                step.drift = "unknown".to_owned();
                step.reason = text(event, "reason");
            }
            event::POLICY_PASSED | event::POLICY_DENIED => {
                step.policy = if kind == event::POLICY_PASSED {
                    "passed"
                } else {
                    "denied"
                }
                .to_owned();
                step.policy_denials = count(event, "denials");
                step.policy_warnings = count(event, "warnings");
            }
            event::ROLLBACK_STARTED => {
                if step.started.is_empty() {
                    step.started = event.time.clone();
                }
            }
            event::ROLLBACK_SUCCEEDED => {
                step.rollback = "succeeded".to_owned();
                end(step, StepState::RolledBack);
            }
            event::ROLLBACK_FAILED => {
                step.rollback = "failed".to_owned();
                step.reason = text(event, "reason");
                step.finished = event.time.clone();
            }
            event::ROLLBACK_UNAVAILABLE => {
                step.rollback = "unavailable".to_owned();
                step.reason = text(event, "reason");
            }
            _ => {}
        }
    }

    /// How long the run took, when it both started and ended.
    pub fn duration_ms(&self) -> Option<u64> {
        time::between(&self.started, &self.finished)
    }

    /// The counts of the run.
    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            steps: self.steps.len() as u64,
            ..Summary::default()
        };
        for step in &self.steps {
            match step.status {
                StepState::Running => summary.running += 1,
                StepState::Succeeded => summary.succeeded += 1,
                StepState::Failed => summary.failed += 1,
                StepState::Skipped => summary.skipped += 1,
                StepState::Deferred => summary.deferred += 1,
                StepState::NotRun => summary.not_run += 1,
                StepState::RolledBack => summary.rolled_back += 1,
            }
            for resource in &step.resources {
                match resource.phase {
                    Phase::Planned => summary.planned += 1,
                    Phase::Changed => summary.changed += 1,
                }
            }
            summary.drifted += u64::from(step.drift == "detected");
        }
        summary
    }
}

/// What a fixed reason word means, for a person. A word this version does
/// not know is shown as it is.
pub fn explain(reason: &str) -> &str {
    match reason {
        "no-plugin" => "no plugin provides the document's target",
        "compile-refused" => "the document was refused before or while it was compiled",
        "not-supported" => "the target cannot do what was asked of it",
        "plugin-failed" => "the plugin failed: its process died, or a call to it did",
        "tool-failed" => "the tool reported a failure",
        "policy-denied" => "the run's policy does not allow the change, or could not be asked",
        "state-failed" => "the step's working directory could not be prepared",
        "already-applied" => "already applied from the same compiled content",
        "when-false" => "left out: its `when` condition is false",
        "earlier-step-failed" => "an earlier step failed",
        "never-applied" => "never applied, so there is nothing to compare",
        "no-preview" => "the tool cannot preview, so it cannot say",
        "last-apply-failed" => "the last apply failed part way; the differences are pending",
        "document-changed" => {
            "the document changed since the last apply; the differences are pending"
        }
        other => other,
    }
}
