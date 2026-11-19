//! Which resource a tool failed on, where, and why, as the tool reported it.
//!
//! A plugin reads its tool's structured output (Terraform's `-json`
//! diagnostics, Ansible's `fatal:` results) and adds one [`Failure`] per
//! resource to the [`RunError`](crate::RunError) it returns:
//!
//! ```
//! use meridian_sdk::failure::{Cause, Failure};
//!
//! let failure = Failure::new(Cause::ResourceFailed, "logs", "bucket name is taken")
//!     .code("BucketAlreadyExists")
//!     .detail("creating S3 Bucket (shop-logs): BucketAlreadyExists");
//! assert_eq!(failure.cause.as_str(), "resource-failed");
//! ```

use std::fmt;
use std::str::FromStr;

/// Why a resource failed, as one fixed word. A word never changes and is
/// never reused for another meaning; a new kind of failure gets a new word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cause {
    /// The machine or the service could not be reached.
    Unreachable,
    /// It was reached, and refused the identity.
    AuthFailed,
    /// The tool refused the configuration before changing anything.
    InvalidConfig,
    /// Something the resource needs is not there: a provider, a module, a
    /// package source, an image.
    DependencyMissing,
    /// The tool tried to change the resource and could not.
    ResourceFailed,
    /// Not tried, because a resource it depends on failed.
    DependencyFailed,
    /// The resource did not finish in the time the tool allows it.
    TimedOut,
    /// The change was made and needs a restart to take effect.
    RebootRequired,
    /// The tool itself stopped unexpectedly.
    ToolCrashed,
    /// The tool's report says it failed and not why.
    #[default]
    Unknown,
}

impl Cause {
    /// Every cause, in the order the reference lists them.
    pub const ALL: [Cause; 10] = [
        Cause::Unreachable,
        Cause::AuthFailed,
        Cause::InvalidConfig,
        Cause::DependencyMissing,
        Cause::ResourceFailed,
        Cause::DependencyFailed,
        Cause::TimedOut,
        Cause::RebootRequired,
        Cause::ToolCrashed,
        Cause::Unknown,
    ];

    /// The word the record keeps.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Unreachable => "unreachable",
            Cause::AuthFailed => "auth-failed",
            Cause::InvalidConfig => "invalid-config",
            Cause::DependencyMissing => "dependency-missing",
            Cause::ResourceFailed => "resource-failed",
            Cause::DependencyFailed => "dependency-failed",
            Cause::TimedOut => "timed-out",
            Cause::RebootRequired => "reboot-required",
            Cause::ToolCrashed => "tool-crashed",
            Cause::Unknown => "unknown",
        }
    }

    /// What the word means, for the reference.
    pub fn meaning(self) -> &'static str {
        match self {
            Cause::Unreachable => "the machine or the service could not be reached",
            Cause::AuthFailed => "it was reached, and refused the identity",
            Cause::InvalidConfig => "the tool refused the configuration before changing anything",
            Cause::DependencyMissing => {
                "something the resource needs is not there: a provider, a module, a package source, an image"
            }
            Cause::ResourceFailed => "the tool tried to change the resource and could not",
            Cause::DependencyFailed => "not tried, because a resource it depends on failed",
            Cause::TimedOut => "the resource did not finish in the time the tool allows it",
            Cause::RebootRequired => "the change was made and needs a restart to take effect",
            Cause::ToolCrashed => "the tool itself stopped unexpectedly",
            Cause::Unknown => "the tool's report says it failed and not why",
        }
    }
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Cause {
    type Err = ();

    fn from_str(word: &str) -> Result<Cause, ()> {
        Cause::ALL
            .into_iter()
            .find(|cause| cause.as_str() == word)
            .ok_or(())
    }
}

impl Cause {
    /// The cause a word names; a word from a newer plugin reads as
    /// [`Cause::Unknown`].
    pub fn read(word: &str) -> Cause {
        word.parse().unwrap_or(Cause::Unknown)
    }
}

/// One resource the tool failed on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The resource id, when the tool's report traces back to one, and
    /// otherwise the tool's own name for it; empty when the failure is the
    /// document's as a whole.
    pub resource: String,
    /// The machine it failed on, when the tool names one.
    pub endpoint: String,
    pub cause: Cause,
    /// The tool's own code or status: `CREATE_FAILED`, `rc=2`.
    pub tool_code: String,
    /// One line a person can read. It can quote a value, so the host keeps
    /// it only on the machine that ran, masked.
    pub summary: String,
    /// The rest of what the tool said about it.
    pub detail: String,
}

impl Failure {
    pub fn new(cause: Cause, resource: impl Into<String>, summary: impl Into<String>) -> Failure {
        Failure {
            resource: resource.into(),
            cause,
            summary: summary.into(),
            ..Failure::default()
        }
    }

    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Failure {
        self.endpoint = endpoint.into();
        self
    }

    pub fn code(mut self, code: impl Into<String>) -> Failure {
        self.tool_code = code.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Failure {
        self.detail = detail.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_reads_back_and_an_unknown_one_is_unknown() {
        for cause in Cause::ALL {
            assert_eq!(Cause::read(cause.as_str()), cause);
            assert!(!cause.meaning().is_empty());
        }
        assert_eq!(Cause::read("melted"), Cause::Unknown);
        let words: std::collections::BTreeSet<&str> =
            Cause::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(words.len(), Cause::ALL.len());
    }
}
