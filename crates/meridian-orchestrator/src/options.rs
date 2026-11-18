//! What a caller hands a run: its options, what to do on failure, how to
//! resume, and the observer that hears it.

use super::*;

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
    /// When a stage's tool is stopped before it finishes: a flag an
    /// interrupt handler sets, a time limit, or neither. A stopped stage is
    /// recorded as [`Reason::Interrupted`] or [`Reason::TimedOut`], the
    /// stages after it as not run, and the lock is released as usual.
    pub stop: Stop,
    /// Ask each target to hand back its tool's own saved plan, for a plan
    /// that will be written with [`PlanFile`](crate::PlanFile).
    pub save_plan: bool,
    /// Apply exactly this saved plan: refuse when any stage no longer
    /// compiles to what was planned, or was applied since, and hand each
    /// tool the plan it saved. Only an apply takes one.
    pub from_plan: Option<Arc<crate::PlanFile>>,
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
            stop: Stop::default(),
            save_plan: false,
            from_plan: None,
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
