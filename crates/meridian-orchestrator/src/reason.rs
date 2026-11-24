//! The fixed word a failed stage is recorded with.

use std::fmt;
use std::str::FromStr;

/// Why a stage failed, as one fixed word. It is what the run's record and
/// its events keep, and what `meridian rollback` and `meridian resume` read
/// back; the text in [`Outcome::Failed`](crate::Outcome::Failed) is for the
/// terminal only, because a tool can quote a value in it.
///
/// A reason's word never changes and is never reused for another meaning,
/// so a record written by one version reads the same in the next. A new kind
/// of failure gets a new variant and a new word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// No plugin provides the document's target.
    NoPlugin,
    /// The document was refused before or while it was compiled.
    CompileRefused,
    /// The target cannot do what was asked of it.
    NotSupported,
    /// The plugin mechanism failed: the process died, or the call did.
    PluginFailed,
    /// The tool said no.
    ToolFailed,
    /// The stage's working directory could not be prepared.
    StateFailed,
    /// The run's policy does not allow the change, or could not be asked.
    PolicyDenied,
    /// The operator stopped the run while the stage was going, or before it
    /// reached its tool.
    Interrupted,
    /// The stage's tool ran past the run's time limit and was stopped.
    TimedOut,
    /// An analyzer found something at or above the severity the run fails
    /// on.
    AnalysisFailed,
    /// An analyzer could not be run, or did not answer, so the stage was
    /// not analyzed.
    AnalyzerFailed,
    /// An apply from a saved plan found that the document no longer compiles
    /// to what was planned, or that its condition no longer keeps it, so the
    /// plan does not say what would happen.
    PlanDiffers,
    /// An apply from a saved plan reached a stage whose tool saved no plan
    /// and cannot say what it would change, so it cannot be checked against
    /// the plan; `--accept-unknown` applies it anyway (decision 0059).
    PlanUnchecked,
}

impl Reason {
    /// Every reason, in the order the reference lists them.
    pub const ALL: [Reason; 13] = [
        Reason::NoPlugin,
        Reason::CompileRefused,
        Reason::NotSupported,
        Reason::PluginFailed,
        Reason::ToolFailed,
        Reason::StateFailed,
        Reason::PolicyDenied,
        Reason::Interrupted,
        Reason::TimedOut,
        Reason::AnalysisFailed,
        Reason::AnalyzerFailed,
        Reason::PlanDiffers,
        Reason::PlanUnchecked,
    ];

    /// The word the record keeps.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NoPlugin => "no-plugin",
            Reason::CompileRefused => "compile-refused",
            Reason::NotSupported => "not-supported",
            Reason::PluginFailed => "plugin-failed",
            Reason::ToolFailed => "tool-failed",
            Reason::StateFailed => "state-failed",
            Reason::PolicyDenied => "policy-denied",
            Reason::Interrupted => "interrupted",
            Reason::TimedOut => "timed-out",
            Reason::AnalysisFailed => "analysis-failed",
            Reason::AnalyzerFailed => "analyzer-failed",
            Reason::PlanDiffers => "plan-differs",
            Reason::PlanUnchecked => "plan-unchecked",
        }
    }

    /// Whether the stage got as far as handing its artifact to the tool, so
    /// the tool may have changed something. A stopped stage is not counted:
    /// `meridian rollback` leaves what an interrupted or timed-out tool did
    /// alone, because the plugin's account of it may be incomplete.
    pub fn reached_the_tool(self) -> bool {
        matches!(self, Reason::ToolFailed | Reason::PluginFailed)
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The word is not one of a stage's reasons. A record also holds words that
/// are a step's and not a failure's, such as `earlier-step-failed`, which
/// read as this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownReason(pub String);

impl fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a reason a stage fails with", self.0)
    }
}

impl std::error::Error for UnknownReason {}

impl FromStr for Reason {
    type Err = UnknownReason;

    fn from_str(word: &str) -> Result<Reason, UnknownReason> {
        Reason::ALL
            .into_iter()
            .find(|reason| reason.as_str() == word)
            .ok_or_else(|| UnknownReason(word.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_reads_back_from_its_word() {
        for reason in Reason::ALL {
            assert_eq!(reason.as_str().parse::<Reason>(), Ok(reason));
            assert_eq!(reason.to_string(), reason.as_str());
        }
        assert!("earlier-step-failed".parse::<Reason>().is_err());
    }

    #[test]
    fn the_words_are_distinct_and_fixed() {
        let mut words: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), Reason::ALL.len());
        for word in words {
            assert!(
                word.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{word}"
            );
        }
    }
}
