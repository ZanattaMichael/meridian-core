//! Runs a workflow.
//!
//! A workflow is a set of documents. Each document is a stage: it is compiled
//! for its target, and then, depending on what was asked, previewed or
//! applied by that target's plugin. Stages run in an order that lets a later
//! one use what an earlier one produced: every Infrastructure document first,
//! then every ResourceSet, each group in the order it was written.
//!
//! Before a document reaches its plugin the orchestrator settles everything
//! that is the core's to settle: whether the document's labels select it for
//! this run, whether its kind is one the target's category takes, whether the
//! target compiles for the version the document asks for, and which resources
//! their `when` conditions leave in.
//!
//! The orchestrator knows nothing about any tool. It routes on what a plugin
//! declares, hands it an artifact and a directory, and reads back changes and
//! outputs. Everything tool-shaped happens behind the plugin boundary.
//!
//! A run stops at the first stage that fails. Stages after it are reported as
//! not run, never attempted against infrastructure in an unknown state.
//!
//! A run that reaches a tool is made safe to repeat and to share:
//!
//! - it holds the state's [lock](lock), so two runs cannot collide;
//! - every change of state is an [event](event), and the events and a
//!   summary are kept as the run's [record](record) in a [backend];
//! - an apply that failed can be [resumed](Resume), skipping the stages that
//!   were already applied from the same artifact;
//! - an apply that failed can instead put back what it changed
//!   ([`OnFailure::Rollback`]), and [`Mode::Rollback`] does the same for the
//!   last apply on request;
//! - a [policy](policy) is asked whether each change may be made, before
//!   the first one is;
//! - [`Mode::Drift`] asks whether the world still matches what was applied.
//!
//! None of it holds a secret value: see `docs/reference/state.md`.

pub mod backend;
pub mod event;
pub mod history;
pub mod lock;
pub mod policy;
pub mod record;
mod rollback;
mod secrets;
mod select;
mod state;
pub mod time;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use meridian_ast::ResourceGraph;
use meridian_ir::{
    Bundle, Document, Infrastructure, Removed, Resolution, ResourceSet, Secrets,
    TARGET_HOST_FROM_INFRASTRUCTURE,
};
use meridian_plugin_host::{to_sdk, EmitError, Registry, RunFailure};
use meridian_sdk::{
    Artifact, Category, Change, Data, LogLine, Output, Plan, RunContext, Value, Warning,
};
use meridian_secret::Masker;

pub use backend::{Backend, ExecBackend, LocalBackend, MemoryBackend};
pub use event::Event;
pub use lock::{LockError, LockInfo};
pub use policy::{Decisions, Policy};
pub use record::{RunRecord, RunStatus, StepRecord, StepStatus};
pub use rollback::{RollbackOutcome, RollbackReport};
pub use secrets::{check_declared, names_declared, places_declared, Declared};
pub use select::Selector;
pub use state::{artifact_hash, stage_key, RunState, StageRecord, STATE_KEY};

/// How far a run goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Compile every document and stop. Nothing is contacted.
    Compile,
    /// Compile, then ask each tool what applying would change.
    Plan,
    /// Compile, then make it so.
    Apply,
    /// Compile, then ask each tool whether what was last applied is still
    /// what is there. Nothing is changed.
    Drift,
    /// Put back what an apply changed: every stage it applied is applied
    /// again from the artifact of its last successful apply before that run,
    /// last stage first. Which apply is [`Options::rollback_of`].
    Rollback,
}

impl Mode {
    /// The name the mode has in a run's record.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Compile => "compile",
            Mode::Plan => "plan",
            Mode::Apply => "apply",
            Mode::Drift => "drift",
            Mode::Rollback => "rollback",
        }
    }
}

/// Which kind of document a stage came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StageKind {
    Infrastructure,
    ResourceSet,
}

impl StageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StageKind::Infrastructure => "Infrastructure",
            StageKind::ResourceSet => "ResourceSet",
        }
    }
}

/// Names a stage: the kind and name of the document it came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StageId {
    pub kind: StageKind,
    pub name: String,
}

impl fmt::Display for StageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:?}", self.kind.as_str(), self.name)
    }
}

/// How a stage ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The document compiled. This is as far as [`Mode::Compile`] goes, and
    /// as far as a plan can go with a target that only compiles.
    Compiled,
    /// The tool previewed the change.
    Planned(Plan),
    /// The tool applied the change.
    Applied {
        changes: Vec<Change>,
        outputs: Vec<Output>,
    },
    /// The stage needs a value that does not exist yet: an output of an
    /// Infrastructure document that has not been applied. It is not a
    /// failure. A plan simply cannot see past infrastructure that is not
    /// there.
    Deferred { waiting_on: StageId, output: String },
    /// A resumed run found the stage already applied, from the same
    /// artifact, by the run named. Nothing was done to it.
    Skipped { applied_in: String },
    /// What a drift check found.
    Drift(Drift),
    /// The document's own `spec.when` was false, so it is not part of this
    /// run. Nothing went wrong, and nothing was compiled.
    LeftOut { condition: String },
    /// The stage failed, and the run stopped here.
    Failed(String),
    /// An earlier stage failed, so this one was not attempted.
    NotRun { because_of: StageId },
}

/// Whether a stage still matches what was last applied to it.
///
/// Drift is a difference nobody asked for: the document compiles to exactly
/// what was applied, and the tool says the world is no longer that. A
/// difference that follows from an edited document is not drift, it is a
/// change waiting to be applied, and the two are kept apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drift {
    /// The stage has never been applied, so there is nothing to compare.
    NeverApplied,
    /// The tool reports nothing to change.
    InSync,
    /// The tool would change these to restore what was applied.
    Drifted(Vec<Change>),
    /// The document no longer compiles to what was last applied, or the
    /// last apply failed part way. The differences are changes waiting to be
    /// applied, and cannot be told from drift.
    Pending {
        changes: Vec<Change>,
        /// Whether the last apply failed, as opposed to the document having
        /// changed since it.
        last_apply_failed: bool,
    },
    /// The tool cannot preview, so it cannot say. This is never read as in
    /// sync.
    Unknown,
}

/// What one stage did.
#[derive(Clone, Debug, PartialEq)]
pub struct StageReport {
    pub id: StageId,
    /// The target the document selected.
    pub target: String,
    /// The host the document configures; empty for an Infrastructure
    /// document, or when the host is not known yet.
    pub host: String,
    /// What the document compiled to, when it got that far.
    pub artifact: Option<Artifact>,
    pub warnings: Vec<Warning>,
    /// What the document's conditions removed before it was compiled.
    pub removed: Vec<Removed>,
    pub outcome: Outcome,
    /// For a failed stage, the fixed word that says what kind of failure it
    /// was, one of the `REASON_` constants. It is what the run's record
    /// keeps; the text in [`Outcome::Failed`] is for the terminal only.
    pub reason: String,
    /// What a failed apply had already done before it failed, as far as the
    /// tool reported it.
    pub done_before_failing: Vec<Change>,
    /// For a stage an apply reached its tool with: the artifact hash of the
    /// stage's last successful apply before this run, which is what a
    /// rollback goes back to. Empty when there was none.
    pub previous_good: String,
    /// What the run's policy said about the stage; `None` when no policy
    /// was asked about it.
    pub policy: Option<Decisions>,
}

/// No plugin provides the document's target.
pub const REASON_NO_PLUGIN: &str = "no-plugin";
/// The document was refused before or while it was compiled.
pub const REASON_COMPILE_REFUSED: &str = "compile-refused";
/// The target cannot do what was asked of it.
pub const REASON_NOT_SUPPORTED: &str = "not-supported";
/// The plugin mechanism failed: the process died, or the call did.
pub const REASON_PLUGIN_FAILED: &str = "plugin-failed";
/// The tool said no.
pub const REASON_TOOL_FAILED: &str = "tool-failed";
/// The stage's working directory could not be prepared.
pub const REASON_STATE_FAILED: &str = "state-failed";
/// The run's policy does not allow the change, or could not be asked.
pub const REASON_POLICY_DENIED: &str = "policy-denied";

/// Why a run did not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Another run holds the state.
    Locked(Box<LockInfo>),
    /// The state could not be read or locked.
    State(String),
    /// There is nothing to resume, or what was named cannot be resumed.
    Resume(String),
    /// There is nothing to roll back.
    Rollback(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Locked(holder) => LockError::Held(holder.clone()).fmt(f),
            Refusal::State(why) | Refusal::Resume(why) | Refusal::Rollback(why) => f.write_str(why),
        }
    }
}

/// What a whole run did, stage by stage, in the order they ran.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunReport {
    /// The run's id, under which its record is kept.
    pub run_id: String,
    pub stages: Vec<StageReport>,
    /// Set when the run did not start at all. No stage was attempted.
    pub refused: Option<Refusal>,
    /// Things that went wrong beside the stages, such as the record not
    /// being written. They do not fail the run: the tools did their work.
    pub warnings: Vec<String>,
    /// What was put back, in the order it was attempted: the last stage
    /// first. Empty unless the run rolled back.
    pub rollbacks: Vec<RollbackReport>,
}

impl RunReport {
    /// Whether every stage did what was asked of it. A deferred stage counts:
    /// nothing went wrong. So does a stage that drifted: the check did its
    /// job, and [`RunReport::drifted`] says what it found. A rollback that
    /// could not put a stage back does not.
    pub fn succeeded(&self) -> bool {
        self.refused.is_none()
            && !self
                .stages
                .iter()
                .any(|s| matches!(s.outcome, Outcome::Failed(_) | Outcome::NotRun { .. }))
            && self.rollbacks.iter().all(|r| r.outcome.is_settled())
    }

    /// Whether a drift check found any stage that no longer matches what was
    /// applied to it.
    pub fn drifted(&self) -> bool {
        self.stages
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Drift(Drift::Drifted(_))))
    }
}

/// Watches a run as it happens.
pub trait Observer {
    /// A stage is about to compile.
    fn stage_started(&mut self, _id: &StageId, _target: &str) {}
    /// The tool printed a line. Sensitive values are already masked.
    fn log(&mut self, _id: &StageId, _line: &LogLine) {}
    /// A stage ended.
    fn stage_finished(&mut self, _report: &StageReport) {}
    /// Something changed state. Every event of the run arrives here, in
    /// order, as it is recorded.
    fn event(&mut self, _event: &Event) {}
    /// A stage was put back, or could not be.
    fn rollback_finished(&mut self, _report: &RollbackReport) {}
}

/// An observer that ignores everything.
pub struct Quiet;

impl Observer for Quiet {}

/// Where a run keeps things, and what it is given.
#[derive(Clone, Debug)]
pub struct Options {
    /// The directory holding each stage's work directory and the run-state
    /// file. It is created if it does not exist.
    pub state_dir: PathBuf,
    /// The data `when` conditions are decided against, also handed to every
    /// target when it compiles.
    pub data: Data,
    /// Fail a stage whose secrets cannot be locked in memory, instead of
    /// warning once and going on.
    pub strict_secrets: bool,
    /// Where the run's record, the run-state and the lock are kept. `None`
    /// is the state directory itself. A tool's working directory is always
    /// under `state_dir`, whatever this is.
    pub backend: Option<Arc<dyn Backend>>,
    /// How long to wait for another run to release the lock before giving
    /// up. Zero refuses at once.
    pub lock_timeout: Duration,
    /// Continue an apply that failed or was interrupted.
    pub resume: Option<Resume>,
    /// What was typed, for the record and for whoever finds the lock held.
    /// It must not hold a secret.
    pub command: String,
    /// How many runs' records are kept; older ones are removed when a run
    /// ends.
    pub keep_runs: usize,
    /// What an apply does when a stage fails.
    pub on_failure: OnFailure,
    /// Which apply [`Mode::Rollback`] puts back.
    pub rollback_of: Resume,
    /// The policy every stage of a plan or an apply is checked against
    /// before it is applied. `None` checks nothing.
    pub policy: Option<Policy>,
}

/// What an apply does when a stage fails.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnFailure {
    /// Stop. The stages after the failed one are not run, and what was
    /// applied stays applied. `--resume` continues from here.
    #[default]
    Stop,
    /// Stop, then put back every stage this run reached a tool with: the
    /// failed one first, then the others, last first.
    Rollback,
}

/// How many runs' records are kept unless a caller says otherwise.
pub const DEFAULT_KEEP_RUNS: usize = 50;

impl Options {
    /// A run that keeps everything under `state_dir`, with no data.
    pub fn new(state_dir: impl Into<PathBuf>) -> Options {
        Options {
            state_dir: state_dir.into(),
            data: Data::new(),
            backend: None,
            lock_timeout: Duration::ZERO,
            resume: None,
            command: String::new(),
            keep_runs: DEFAULT_KEEP_RUNS,
            on_failure: OnFailure::Stop,
            rollback_of: Resume::Last,
            policy: None,
            strict_secrets: false,
        }
    }

    /// Where the record is kept.
    pub fn store(&self) -> Arc<dyn Backend> {
        match &self.backend {
            Some(backend) => backend.clone(),
            None => Arc::new(LocalBackend::new(&self.state_dir)),
        }
    }
}

/// Which run an apply continues, or a rollback puts back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resume {
    /// The last apply.
    Last,
    /// The run with this id.
    Run(String),
}

/// The text shown in place of a sensitive value.
pub const MASK: &str = "(sensitive)";

/// Runs every document in `bundle` through its target.
///
/// The bundle must already be valid: [`Bundle::validate`] is the caller's
/// job, because its errors are about the documents and belong in front of
/// the author before anything starts.
pub fn run(
    bundle: &Bundle,
    registry: &Registry,
    mode: Mode,
    options: &Options,
    observer: &mut dyn Observer,
) -> RunReport {
    run_selected(bundle, registry, mode, options, &Selector::all(), observer)
}

/// [`run`], for only the documents whose labels `selector` matches.
///
/// A document the selector leaves out is not a stage, but it stays loaded: a
/// selected ResourceSet can still name it as the source of its target host,
/// and finds that host from what the document's last apply left behind.
pub fn run_selected(
    bundle: &Bundle,
    registry: &Registry,
    mode: Mode,
    options: &Options,
    selector: &Selector,
    observer: &mut dyn Observer,
) -> RunReport {
    let run_id = time::run_id();
    let backend = options.store();
    // Compiling contacts nothing and changes nothing, so it takes no lock
    // and leaves no record.
    let persist = mode != Mode::Compile;
    let mut report = RunReport {
        run_id: run_id.clone(),
        ..RunReport::default()
    };

    let command = if options.command.is_empty() {
        mode.as_str().to_owned()
    } else {
        options.command.clone()
    };
    let holder = LockInfo::for_this_process(&run_id, &command);
    if persist {
        if let Err(e) = lock::acquire(backend.as_ref(), &holder, options.lock_timeout) {
            report.refused = Some(match e {
                LockError::Held(holder) => Refusal::Locked(holder),
                LockError::Backend(why) => Refusal::State(format!("taking the state lock: {why}")),
            });
            return report;
        }
    }
    // Releases the lock however the run ends, a panic included.
    let mut guard = LockGuard {
        backend: backend.clone(),
        id: run_id.clone(),
        held: persist,
    };

    let state = if persist {
        match RunState::read(backend.as_ref()) {
            Ok(state) => state,
            Err(why) => {
                report.refused = Some(Refusal::State(format!("reading the run-state: {why}")));
                return report;
            }
        }
    } else {
        RunState::default()
    };
    let resume = match (&options.resume, mode) {
        (None, _) => None,
        (Some(_), Mode::Compile | Mode::Plan | Mode::Drift | Mode::Rollback) => {
            report.refused = Some(Refusal::Resume("only an apply can be resumed".to_owned()));
            return report;
        }
        (Some(resume), Mode::Apply) => match Resumed::find(backend.as_ref(), resume) {
            Ok(resumed) => Some(resumed),
            Err(why) => {
                report.refused = Some(Refusal::Resume(why));
                return report;
            }
        },
    };

    let rolling_back = if mode == Mode::Rollback {
        match rollback::find(backend.as_ref(), &options.rollback_of) {
            Ok(record) => Some(record),
            Err(why) => {
                report.refused = Some(Refusal::Rollback(why));
                return report;
            }
        }
    } else {
        None
    };

    let mut journal = Journal {
        backend: backend.clone(),
        persist,
        record: RunRecord {
            id: run_id.clone(),
            mode: mode.as_str().to_owned(),
            command,
            user: holder.user.clone(),
            host: holder.host.clone(),
            selector: selector.to_string(),
            started: time::now(),
            finished: String::new(),
            status: RunStatus::Running,
            resumes: resume.as_ref().map(|r| r.run.clone()).unwrap_or_default(),
            steps: Vec::new(),
        },
        events: Vec::new(),
        warnings: Vec::new(),
    };
    if persist {
        journal.emit(
            observer,
            event::LOCK_ACQUIRED,
            None,
            vec![("id", Value::from(run_id.as_str()))],
        );
    }
    journal.emit(
        observer,
        event::RUN_STARTED,
        None,
        vec![
            ("mode", Value::from(mode.as_str())),
            ("command", Value::from(journal.record.command.as_str())),
            ("resumes", Value::from(journal.record.resumes.as_str())),
            (
                "rollsBack",
                Value::from(rolling_back.as_ref().map_or("", |r| r.id.as_str())),
            ),
        ],
    );
    journal.flush();

    let mut run = Run {
        bundle,
        registry,
        mode,
        options,
        outputs: BTreeMap::new(),
        secrets: Vec::new(),
        state,
        backend: backend.clone(),
        run_id: run_id.clone(),
        resume,
        warnings: Vec::new(),
        touched: Vec::new(),
        checked: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        preflight: false,
        masker: Masker::new(),
        unlocked: Vec::new(),
        warned_unlocked: false,
    };
    let mut failed: Option<StageId> = None;

    // A rollback runs no stage of its own: it puts back another run's.
    let to_run = if mode == Mode::Rollback {
        Vec::new()
    } else {
        stages(bundle, selector, &options.data)
    };

    // With a policy, an apply asks about every stage it can before it
    // applies the first: a change that is not allowed stops the run while
    // there is still nothing to undo.
    let mut to_run = to_run;
    if mode == Mode::Apply && options.policy.is_some() {
        run.preflight = true;
        let checks: Vec<StageReport> = to_run
            .iter()
            .map(|stage| run.stage(*stage, observer))
            .collect();
        run.preflight = false;
        let first = checks
            .iter()
            .find(|check| matches!(check.outcome, Outcome::Failed(_)))
            .map(|check| check.id.clone());
        if let Some(first) = first {
            for check in checks {
                let stopped = if matches!(check.outcome, Outcome::Failed(_)) {
                    observer.stage_started(&check.id, &check.target);
                    journal.started(observer, &check.id, &check.target);
                    check
                } else {
                    StageReport {
                        host: String::new(),
                        artifact: None,
                        warnings: Vec::new(),
                        outcome: Outcome::NotRun {
                            because_of: first.clone(),
                        },
                        policy: None,
                        ..check
                    }
                };
                journal.finished(observer, &stopped);
                observer.stage_finished(&stopped);
                report.stages.push(stopped);
            }
            failed = Some(first);
            to_run = Vec::new();
        }
    }
    for stage in to_run {
        let id = stage.id();
        // The target as the document's interpolations make it, for what
        // is said before the stage has run. The stage itself reads the
        // document again, with its secrets, when its turn comes.
        let target = match meridian_ir::resolve(&stage.document(), &options.data) {
            Ok(Resolution::Kept { document, .. }) => document.target().to_owned(),
            _ => stage.target().to_owned(),
        };
        if let Some(because_of) = &failed {
            let skipped = StageReport {
                id,
                target,
                host: String::new(),
                artifact: None,
                warnings: Vec::new(),
                removed: Vec::new(),
                outcome: Outcome::NotRun {
                    because_of: because_of.clone(),
                },
                reason: String::new(),
                done_before_failing: Vec::new(),
                previous_good: String::new(),
                policy: None,
            };
            journal.finished(observer, &skipped);
            observer.stage_finished(&skipped);
            report.stages.push(skipped);
            continue;
        }
        observer.stage_started(&id, &target);
        journal.started(observer, &id, &target);
        let stage_report = run.stage(stage, observer);
        // A plan goes on past a stage its policy denied, so that one plan
        // shows every denial there is.
        let denied_plan = mode == Mode::Plan && stage_report.reason == REASON_POLICY_DENIED;
        if matches!(stage_report.outcome, Outcome::Failed(_)) && !denied_plan {
            failed = Some(stage_report.id.clone());
        }
        journal.finished(observer, &stage_report);
        observer.stage_finished(&stage_report);
        report.stages.push(stage_report);
    }

    if let Some(record) = &rolling_back {
        let items = rollback::items_of(&mut run, record);
        report.rollbacks = rollback::execute(&mut run, &mut journal, observer, items);
    } else if failed.is_some() && mode == Mode::Apply && options.on_failure == OnFailure::Rollback {
        let items = std::mem::take(&mut run.touched)
            .into_iter()
            .map(rollback::Item::Ready)
            .collect();
        report.rollbacks = rollback::execute(&mut run, &mut journal, observer, items);
    }

    let failures = report
        .stages
        .iter()
        .filter(|s| matches!(s.outcome, Outcome::Failed(_)))
        .count();
    journal.record.status = if report.succeeded() {
        RunStatus::Succeeded
    } else {
        RunStatus::Failed
    };
    journal.record.finished = time::now();
    journal.emit(
        observer,
        if report.succeeded() {
            event::RUN_SUCCEEDED
        } else {
            event::RUN_FAILED
        },
        None,
        vec![
            ("steps", Value::Int(report.stages.len() as i64)),
            ("failed", Value::Int(failures as i64)),
            (
                "rolledBack",
                Value::Int(
                    report
                        .rollbacks
                        .iter()
                        .filter(|r| matches!(r.outcome, RollbackOutcome::RolledBack { .. }))
                        .count() as i64,
                ),
            ),
        ],
    );
    if persist {
        journal.emit(
            observer,
            event::LOCK_RELEASED,
            None,
            vec![("id", Value::from(run_id.as_str()))],
        );
    }
    journal.flush();
    if persist {
        if let Err(why) = record::prune(backend.as_ref(), options.keep_runs.max(1)) {
            journal
                .warnings
                .push(format!("removing old run records: {why}"));
        }
        guard.held = false;
        if let Err(why) = lock::release(backend.as_ref(), &run_id) {
            journal.warnings.push(format!(
                "the state lock {run_id} could not be released ({why}); remove it with \
                 `meridian unlock {run_id}`"
            ));
        }
    }
    report.warnings = run.warnings;
    report.warnings.append(&mut journal.warnings);
    report
}

/// Releases the lock when a run ends without doing so itself.
struct LockGuard {
    backend: Arc<dyn Backend>,
    id: String,
    held: bool,
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
struct Resumed {
    run: String,
    done: BTreeMap<StageId, String>,
}

impl Resumed {
    fn find(backend: &dyn Backend, resume: &Resume) -> Result<Resumed, String> {
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
struct Journal {
    backend: Arc<dyn Backend>,
    /// Whether the record is kept at all. A compile leaves none.
    persist: bool,
    record: RunRecord,
    events: Vec<Event>,
    warnings: Vec<String>,
}

impl Journal {
    fn emit(
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
    fn flush(&mut self) {
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

    fn started(&mut self, observer: &mut dyn Observer, id: &StageId, target: &str) {
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

    fn finished(&mut self, observer: &mut dyn Observer, report: &StageReport) {
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
                step.status = StepStatus::Failed;
                step.reason = report.reason.clone();
                step.changes = report.done_before_failing.len() as u64;
                // The reason is a fixed word. What the tool said stays on
                // the terminal, because a tool can quote a value.
                self.emit(
                    observer,
                    event::STEP_FAILED,
                    at,
                    vec![
                        ("reason", Value::from(report.reason.as_str())),
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

/// One document, as a stage.
#[derive(Clone, Copy)]
enum Stage<'a> {
    Infrastructure(&'a Infrastructure),
    ResourceSet(&'a ResourceSet),
}

impl Stage<'_> {
    fn id(&self) -> StageId {
        match self {
            Stage::Infrastructure(d) => StageId {
                kind: StageKind::Infrastructure,
                name: d.metadata.name.clone(),
            },
            Stage::ResourceSet(d) => StageId {
                kind: StageKind::ResourceSet,
                name: d.metadata.name.clone(),
            },
        }
    }

    fn target(&self) -> &str {
        match self {
            Stage::Infrastructure(d) => &d.spec.target,
            Stage::ResourceSet(d) => &d.spec.target,
        }
    }

    fn document(&self) -> Document {
        match *self {
            Stage::Infrastructure(d) => Document::Infrastructure(d.clone()),
            Stage::ResourceSet(d) => Document::ResourceSet(d.clone()),
        }
    }

    fn target_version(&self) -> &str {
        match self {
            Stage::Infrastructure(d) => &d.spec.target_version,
            Stage::ResourceSet(d) => &d.spec.target_version,
        }
    }

    fn labels(&self) -> &BTreeMap<String, String> {
        match self {
            Stage::Infrastructure(d) => &d.metadata.labels,
            Stage::ResourceSet(d) => &d.metadata.labels,
        }
    }

    fn imports(&self) -> &[meridian_ir::Import] {
        match self {
            Stage::Infrastructure(d) => &d.spec.imports,
            Stage::ResourceSet(d) => &d.spec.imports,
        }
    }

    /// Whether this stage is `document`.
    fn is(&self, document: &Document) -> bool {
        match (self, document) {
            (Stage::Infrastructure(a), Document::Infrastructure(b)) => {
                a.metadata.name == b.metadata.name
            }
            (Stage::ResourceSet(a), Document::ResourceSet(b)) => a.metadata.name == b.metadata.name,
            _ => false,
        }
    }

    /// A document, as a stage.
    fn of(document: &Document) -> Stage<'_> {
        match document {
            Document::Infrastructure(doc) => Stage::Infrastructure(doc),
            Document::ResourceSet(doc) => Stage::ResourceSet(doc),
        }
    }

    fn in_bundle<'b>(&self, bundle: &'b Bundle) -> Option<&'b Document> {
        bundle.documents.iter().find(|document| self.is(document))
    }
}

/// How many documents of `bundle` a selector picks. A run that picks none
/// does nothing, which a caller will usually want to say out loud.
pub fn selected(bundle: &Bundle, selector: &Selector, data: &Data) -> usize {
    stages(bundle, selector, data).len()
}

/// Whether a document of this kind can be aimed at a target of this
/// category, and if not, why.
///
/// The two kinds answer different questions. An Infrastructure document says
/// what should exist; a ResourceSet says how a machine that exists is
/// configured. A tool that provisions has no machine to be pointed at, and a
/// tool that configures one machine has nothing to provision, so the wrong
/// pairing is refused before the plugin sees it instead of compiling into
/// something that quietly means less than the document says.
///
/// Containerization takes both: a container project either stands on its
/// own (Infrastructure) or runs on the daemon of a host the document names
/// (ResourceSet). A plugin that does not state a category was built before
/// categories existed, and is not second-guessed.
pub fn kind_fits(kind: StageKind, category: Category) -> Result<(), String> {
    let wants = match (kind, category) {
        (StageKind::ResourceSet, Category::InfrastructureAsCode)
        | (StageKind::ResourceSet, Category::ContainerOrchestration) => StageKind::Infrastructure,
        (StageKind::Infrastructure, Category::ConfigurationManagement) => StageKind::ResourceSet,
        _ => return Ok(()),
    };
    let why = match wants {
        StageKind::Infrastructure => {
            "it creates what should exist and is not pointed at one machine, so it takes \
             kind: Infrastructure, without a targetHost"
        }
        StageKind::ResourceSet => {
            "it configures a machine that already exists, so it takes kind: ResourceSet, \
             with a targetHost"
        }
    };
    let (document, tool) = match kind {
        StageKind::ResourceSet => ("a ResourceSet", "an infrastructure-as-code"),
        StageKind::Infrastructure => ("an Infrastructure", "a configuration-management"),
    };
    let tool = match category {
        Category::ContainerOrchestration => "a container-orchestration",
        _ => tool,
    };
    Err(format!(
        "this is {document} document, and its target is {tool} tool: {why}"
    ))
}

/// Whether `asked`, a document's `targetVersion`, is one of the versions a
/// target declares or a more specific version under one of them: `3.1` is
/// under `3`, and `1.10` is not under `1.1`.
pub fn version_fits(asked: &str, declared: &[String]) -> bool {
    declared.iter().any(|have| {
        asked == have
            || asked
                .strip_prefix(have.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Every Infrastructure document, then every ResourceSet, each in the order
/// written. A ResourceSet can only depend on an Infrastructure document, so
/// this order satisfies every dependency a workflow can express.
///
/// A selector matches a document's labels as they are once their
/// interpolations are placed. A document whose labels cannot be decided stays
/// a stage, so that its error is reported instead of the document quietly
/// dropping out of the run.
fn stages<'a>(bundle: &'a Bundle, selector: &Selector, data: &Data) -> Vec<Stage<'a>> {
    let selected = |doc: &&Document| {
        meridian_ir::resolved_labels(doc, data).map_or(true, |labels| selector.matches(&labels))
    };
    let infrastructure = bundle
        .documents
        .iter()
        .filter(selected)
        .filter_map(Document::as_infrastructure)
        .map(Stage::Infrastructure);
    let resource_sets = bundle
        .documents
        .iter()
        .filter(selected)
        .filter_map(Document::as_resource_set)
        .map(Stage::ResourceSet);
    let written: Vec<Stage<'a>> = infrastructure.chain(resource_sets).collect();
    by_need(bundle, written)
}

/// Orders stages so that each comes after every stage it needs: the one its
/// host comes from, and every one it imports from.
///
/// The order given is kept wherever need allows, so a workflow with no
/// imports runs exactly as it was written. A stage that needs a document the
/// run does not include waits on nothing: what it needs is read from what
/// that document's last apply left behind. A cycle cannot be ordered and is
/// refused when the bundle is validated; should one arrive here anyway, its
/// stages are kept in the order given.
fn by_need<'a>(bundle: &'a Bundle, mut waiting: Vec<Stage<'a>>) -> Vec<Stage<'a>> {
    let mut ordered: Vec<Stage<'a>> = Vec::with_capacity(waiting.len());
    while !waiting.is_empty() {
        let ready = waiting.iter().position(|stage| {
            let Some(document) = stage.in_bundle(bundle) else {
                return true;
            };
            !bundle
                .needs(document)
                .iter()
                .any(|need| waiting.iter().any(|other| other.is(need)))
        });
        ordered.push(waiting.remove(ready.unwrap_or(0)));
    }
    ordered
}

struct Run<'a> {
    bundle: &'a Bundle,
    registry: &'a Registry,
    mode: Mode,
    options: &'a Options,
    /// The outputs of each Infrastructure document, once known.
    outputs: BTreeMap<String, Vec<Output>>,
    /// Sensitive values seen so far, masked out of everything shown.
    secrets: Vec<String>,
    /// Every stage's last apply, as read when the run started and as this
    /// run has changed it since.
    state: RunState,
    backend: Arc<dyn Backend>,
    run_id: String,
    /// The run this one continues, when it was asked to.
    resume: Option<Resumed>,
    warnings: Vec<String>,
    /// Every stage this run has handed to a tool to apply, in order, with
    /// what is needed to put it back.
    touched: Vec<rollback::Touched>,
    /// What the policy said about each stage the preflight asked about, and
    /// the artifact it was asked about.
    checked: BTreeMap<StageId, (String, Decisions)>,
    /// What each document compiled to in this run, for the stages that
    /// import a file from it.
    artifacts: BTreeMap<StageId, Artifact>,
    /// Whether this is an apply's first pass, which asks the policy about
    /// every stage it can and applies nothing.
    preflight: bool,
    /// The filter for the secrets the stage now running fetched, for what is
    /// said about a failure. Empty between stages.
    masker: Masker,
    /// The secrets of the stage now running that could not be locked in
    /// memory, and whether the run has already said so.
    unlocked: Vec<String>,
    warned_unlocked: bool,
}

/// A stage, compiled.
struct Compiled {
    /// The tree the target was handed.
    graph: meridian_sdk::ResourceGraph,
    emitted: meridian_sdk::Emitted,
    /// The machine the stage configures; empty when there is none.
    host: String,
    /// The values the stage's tool is handed.
    inputs: Data,
}

/// Why a stage did not compile, or cannot yet.
enum Stop {
    Failed {
        /// One of the `REASON_` constants.
        reason: &'static str,
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
enum Read {
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
enum Halt {
    Failed(String),
    Deferred { waiting_on: StageId, output: String },
}

impl Run<'_> {
    fn stage(&mut self, stage: Stage<'_>, observer: &mut dyn Observer) -> StageReport {
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
            artifact: None,
            warnings: Vec::new(),
            removed: Vec::new(),
            outcome: Outcome::Compiled,
            reason: String::new(),
            done_before_failing: Vec::new(),
            previous_good: String::new(),
            policy: None,
        };
        let fail = |mut report: StageReport, reason: &str, why: String| {
            report.outcome = Outcome::Failed(why);
            report.reason = reason.to_owned();
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
                report.outcome = Outcome::LeftOut { condition };
                return report;
            }
            Read::Failed(why) => return fail(report, REASON_COMPILE_REFUSED, why),
        };
        let stage = Stage::of(&document);

        let Compiled {
            graph,
            emitted,
            host,
            mut inputs,
        } = match self.compile(stage) {
            Ok(compiled) => compiled,
            Err(Stop::Failed { reason, why, host }) => {
                report.host = host;
                return fail(report, reason, why);
            }
            Err(Stop::Deferred { waiting_on, output }) => {
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
        report.warnings = emitted.warnings;
        self.artifacts.insert(id.clone(), emitted.artifact.clone());
        report.artifact = Some(emitted.artifact.clone());
        if self.mode == Mode::Compile {
            return report;
        }

        // What the stage is handed from other documents. Compiling needs
        // none of it: the values reach the tool, not the artifact.
        if let Some(document) = stage.in_bundle(self.bundle) {
            match self.imports_of(document) {
                Ok(imports) => inputs.extend(imports),
                Err(Stop::Failed { reason, why, .. }) => return fail(report, reason, why),
                Err(Stop::Deferred { waiting_on, output }) => {
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
                    return fail(report, REASON_POLICY_DENIED, denial(&decisions));
                }
            }
            return report;
        }
        let hash = artifact_hash(&emitted.artifact);
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
                REASON_NOT_SUPPORTED,
                format!(
                    "target {:?} only compiles, so Meridian cannot apply this document; \
                     use `meridian compile` and apply the result with the tool itself",
                    stage.target()
                ),
            );
        }

        let context = match self.context(&id, stage.target(), &report.host, inputs) {
            Ok(context) => context,
            Err(why) => return fail(report, REASON_STATE_FAILED, why),
        };

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
                        match target.plan(&context, &emitted.artifact, &mut log) {
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
                return fail(report, REASON_POLICY_DENIED, denial(&decisions));
            }
            if self.preflight {
                self.checked.insert(id.clone(), (hash.clone(), decisions));
            }
        }
        if self.preflight {
            return report;
        }

        let plan_now = match planned.take() {
            Some(plan) if self.mode == Mode::Plan => Some(Ok(plan)),
            _ => None,
        };
        match self.mode {
            Mode::Plan => match plan_now
                .unwrap_or_else(|| target.plan(&context, &emitted.artifact, &mut log))
            {
                Ok(plan) => report.outcome = Outcome::Planned(plan),
                Err(e) => return fail(report, reason_of(&e), self.describe(e)),
            },
            Mode::Drift => match target.plan(&context, &emitted.artifact, &mut log) {
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
                self.touched.push(rollback::Touched {
                    id: id.clone(),
                    target: stage.target().to_owned(),
                    context: context.clone(),
                    previous: report.previous_good.clone(),
                    attempted: hash.clone(),
                });
                let outcome = target.apply(&context, &emitted.artifact, &mut log);
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
                        if let RunFailure::Tool(tool) = &e {
                            report.done_before_failing = tool.changes.clone();
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
        let mut resolution = meridian_ir::resolve_with(&document, &self.options.data, &secrets);
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
                Ok(()) => meridian_ir::resolve_with(&document, &self.options.data, &secrets),
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

        // A secret in a param. A read that fetched nothing writes what
        // stands for it. `plan` and `apply` hold it, and must hand it to the
        // tool by a channel that is not the compiled files; until a target
        // has one, the stage stops here instead of writing a secret into a
        // file or dropping it silently.
        if let Some((resource, param)) = secrets::first_in_params(&document) {
            if !fetches {
                secrets::describe_in_params(&mut document, secrets.table());
            } else {
                return Read::Failed(format!(
                    "resource {resource:?}: {param} holds a secret, and target {:?} cannot \
                     be handed a secret yet. A secret can decide a when today; handing one \
                     to a tool arrives with secret delivery for each target",
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
    fn compile(&mut self, stage: Stage<'_>) -> Result<Compiled, Stop> {
        let id = stage.id();
        let refuse = |reason: &'static str, why: String| Stop::Failed {
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
                REASON_NO_PLUGIN,
                format!("no plugin provides target {:?} ({known})", stage.target()),
            ));
        };

        let info = target.info();
        if let Err(why) = kind_fits(id.kind, info.category) {
            return Err(refuse(REASON_COMPILE_REFUSED, why));
        }
        let asked = stage.target_version();
        if !asked.is_empty() {
            if info.target_versions.is_empty() {
                return Err(refuse(
                    REASON_COMPILE_REFUSED,
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
                    REASON_COMPILE_REFUSED,
                    format!(
                        "the document asks for targetVersion {asked:?}, and target {:?} \
                         compiles for: {}",
                        stage.target(),
                        info.target_versions.join(", ")
                    ),
                ));
            }
        }
        if let Some(import) = stage.imports().first() {
            if !info.accepts_inputs {
                // Dropping the value without a word would compile into
                // something that means less than the document says.
                return Err(refuse(
                    REASON_COMPILE_REFUSED,
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
                Err(e) => return Err(refuse(REASON_COMPILE_REFUSED, e.to_string())),
            },
            Stage::ResourceSet(doc) => {
                let (host, inputs) = match self.host_of(doc) {
                    Ok(found) => found,
                    Err(Halt::Failed(why)) => return Err(refuse(REASON_COMPILE_REFUSED, why)),
                    Err(Halt::Deferred { waiting_on, output }) => {
                        return Err(Stop::Deferred { waiting_on, output })
                    }
                };
                match ResourceGraph::build(doc, &host) {
                    Ok(graph) => (graph, host, inputs),
                    Err(e) => {
                        return Err(Stop::Failed {
                            reason: REASON_COMPILE_REFUSED,
                            why: e.to_string(),
                            host,
                        })
                    }
                }
            }
        };

        let graph = to_sdk(&graph);
        let failed =
            |reason: &'static str, why: String, host: String| Stop::Failed { reason, why, host };
        match target.emit(&graph, &options.data) {
            Ok(emitted) => Ok(Compiled {
                graph,
                emitted,
                host,
                inputs,
            }),
            Err(EmitError::Refused(e)) => Err(failed(REASON_COMPILE_REFUSED, e.to_string(), host)),
            Err(EmitError::Plugin(e)) => Err(failed(REASON_PLUGIN_FAILED, e.to_string(), host)),
        }
    }

    /// What a document compiles to: what this run already compiled it to,
    /// or, for a document the run does not include, a compile made now.
    fn artifact_of(&mut self, source: &Document) -> Result<Artifact, Stop> {
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
        let (document, _secrets) = match self.read(stage, false) {
            Read::Kept {
                document, secrets, ..
            } => (document, secrets),
            Read::LeftOut { condition } => {
                return Err(Stop::Failed {
                    reason: REASON_COMPILE_REFUSED,
                    why: format!("its when ({condition}) is false, so it compiles to nothing"),
                    host: String::new(),
                })
            }
            Read::Failed(why) => {
                return Err(Stop::Failed {
                    reason: REASON_COMPILE_REFUSED,
                    why,
                    host: String::new(),
                })
            }
        };
        let compiled = self.compile(Stage::of(&document))?;
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
    fn imports_of(&mut self, document: &Document) -> Result<Data, Stop> {
        let bundle = self.bundle;
        let refuse = |why: String| Stop::Failed {
            reason: REASON_COMPILE_REFUSED,
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
                    Err(Stop::Failed { why, .. }) => {
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
                    return Err(Stop::Deferred {
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
    fn host_of(&mut self, doc: &ResourceSet) -> Result<(String, Data), Halt> {
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
        let Ok(context) = self.context(source, &doc.spec.target, "", Data::new()) else {
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

    fn learn(&mut self, id: &StageId, outputs: &[Output]) {
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

    fn context(
        &self,
        id: &StageId,
        target: &str,
        host: &str,
        inputs: Data,
    ) -> Result<RunContext, String> {
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
        })
    }

    fn record(&mut self, id: &StageId, target: &str, hash: &str, applied: bool) {
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

    fn describe(&self, failure: RunFailure) -> String {
        self.masker.mask(&mask(&failure.to_string(), &self.secrets))
    }
}

/// What a denied stage says.
fn denial(decisions: &Decisions) -> String {
    if !decisions.asked {
        return decisions.deny.join("; ");
    }
    format!(
        "the policy does not allow this change:\n  - {}",
        decisions.deny.join("\n  - ")
    )
}

/// The fixed word for why a run of a tool produced no result.
fn reason_of(failure: &RunFailure) -> &'static str {
    match failure {
        RunFailure::Tool(_) => REASON_TOOL_FAILED,
        RunFailure::Plugin(_) => REASON_PLUGIN_FAILED,
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

/// Replaces every sensitive value in `text`.
pub fn mask(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), MASK);
        }
    }
    out
}
