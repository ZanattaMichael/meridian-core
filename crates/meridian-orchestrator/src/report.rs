//! What a run reports: the mode, the stages, each stage's outcome and the
//! fixed words a failed stage is recorded with.

use super::*;

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
    /// Remove what each stage's tool created, last stage first. A stage
    /// whose target cannot destroy is left in place.
    Destroy,
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
            Mode::Destroy => "destroy",
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
    /// The tool removed everything it had created for the stage.
    Destroyed { changes: Vec<Change> },
    /// A destroy, or its preview, left the stage in place. Nothing went
    /// wrong.
    Kept(Kept),
    /// The document's own `spec.when` was false, so it is not part of this
    /// run. Nothing went wrong, and nothing was compiled.
    LeftOut { condition: String },
    /// The stage failed, and the run stopped here.
    Failed(String),
    /// An earlier stage failed, so this one was not attempted.
    NotRun { because_of: StageId },
}

/// Why a destroy left a stage in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The stage's target cannot destroy: its tool keeps no record of what
    /// it made, or the target only compiles.
    CannotDestroy,
    /// The run-state has no apply of the stage, so there is nothing to
    /// remove.
    NeverApplied,
}

impl Kept {
    /// The word the record and the JSON output use.
    pub fn as_str(&self) -> &'static str {
        match self {
            Kept::CannotDestroy => "cannot-destroy",
            Kept::NeverApplied => "never-applied",
        }
    }

    /// What a person is told.
    pub fn why(&self, target: &str) -> String {
        match self {
            Kept::CannotDestroy => format!(
                "target {target:?} cannot destroy; remove what it made with the tool, or \
                 destroy the host it configured"
            ),
            Kept::NeverApplied => {
                "never applied with this state, so there is nothing to destroy".to_owned()
            }
        }
    }
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
    /// The credential the host is reached with, in a few words (`deploy
    /// (ssh, user deploy)`), so that which identity touches a machine can
    /// be read off a plan; empty when the document names none. Never any
    /// part of a secret.
    pub credential: String,
    /// What the document compiled to, when it got that far.
    pub artifact: Option<Artifact>,
    pub warnings: Vec<Warning>,
    /// What the document's conditions removed before it was compiled.
    pub removed: Vec<Removed>,
    pub outcome: Outcome,
    /// For a failed stage, what kind of failure it was. Its word is what
    /// the run's record keeps; the text in [`Outcome::Failed`] is for the
    /// terminal only.
    pub reason: Option<Reason>,
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
    /// What each analyzer said about the stage, in the order the analyzers
    /// were named; empty when none was asked.
    pub analyses: Vec<Analysis>,
}

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
    /// The saved plan does not fit this run.
    Plan(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Locked(holder) => LockError::Held(holder.clone()).fmt(f),
            Refusal::State(why)
            | Refusal::Resume(why)
            | Refusal::Rollback(why)
            | Refusal::Plan(why) => f.write_str(why),
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

    /// Whether the operator stopped the run.
    pub fn interrupted(&self) -> bool {
        self.stages
            .iter()
            .any(|s| s.reason == Some(Reason::Interrupted))
    }

    /// Whether a drift check found any stage that no longer matches what was
    /// applied to it.
    pub fn drifted(&self) -> bool {
        self.stages
            .iter()
            .any(|s| matches!(s.outcome, Outcome::Drift(Drift::Drifted(_))))
    }
}
