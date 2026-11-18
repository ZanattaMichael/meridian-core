//! What a plugin is, beyond a compiler: its category, the operations it
//! offers, and the [`Runner`] half that hands an artifact to the real tool.

use std::fmt;
use std::path::PathBuf;

use crate::credential::HeldCredential;
use crate::settings::Setting;
use crate::{Artifact, Data, Secrets, Value};

/// The kind of tool a plugin drives. The core routes on it, to decide which
/// workflows a plugin can join and where in one it sits. It is a declared
/// fact, never a reason for the core to special-case a product.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// The plugin did not say, which is what one built before categories
    /// existed reports.
    #[default]
    Unspecified,
    /// Provisions infrastructure: Terraform, Bicep, CloudFormation.
    InfrastructureAsCode,
    /// Configures a node that already exists: Ansible, Puppet, Chef, DSC.
    ConfigurationManagement,
    /// Builds and runs containers on one host: Docker.
    Containerization,
    /// Schedules containers across a cluster: Kubernetes.
    ContainerOrchestration,
}

impl Category {
    /// The name used in documents, command output and documentation.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Unspecified => "unspecified",
            Category::InfrastructureAsCode => "infrastructure-as-code",
            Category::ConfigurationManagement => "configuration-management",
            Category::Containerization => "containerization",
            Category::ContainerOrchestration => "container-orchestration",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A plugin's type: what it can do for the core beyond compiling. Every plugin
/// compiles. A host never calls a [`Runner`] method whose field is false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Operations {
    /// Preview what applying an artifact would change, changing nothing.
    pub plan: bool,
    /// Make the artifact's desired state real.
    pub apply: bool,
    /// Report named values the tool produced, for later stages to use.
    pub outputs: bool,
}

impl Operations {
    /// A plugin that only compiles.
    pub const COMPILE_ONLY: Operations = Operations {
        plan: false,
        apply: false,
        outputs: false,
    };

    /// A plugin that plans, applies and reports outputs.
    pub const ALL: Operations = Operations {
        plan: true,
        apply: true,
        outputs: true,
    };

    /// Whether the plugin does anything beyond compiling.
    pub fn any(self) -> bool {
        self.plan || self.apply || self.outputs
    }
}

/// What every run needs to know about where it runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunContext {
    /// The name of the document the artifact was compiled from.
    pub document: String,
    /// The node the artifact configures; empty for a document that provisions
    /// infrastructure instead of configuring a host.
    pub host: String,
    /// A directory the host created for this document and target and keeps
    /// between runs. The plugin writes the artifact there and may keep the
    /// tool's own state in it. The host never interprets its contents.
    pub work_dir: PathBuf,
    /// Values handed to this stage from the outputs of earlier ones, keyed by
    /// the name the document gave them.
    pub inputs: Data,
    /// The value of every secret the graph's params referred to, for the
    /// runner to hand to the tool. Sent only to a target whose
    /// [`Capabilities::secrets`](crate::Capabilities::secrets) is true, and
    /// only for `plan` and `apply`.
    pub secrets: Secrets,
    /// The credential the host is reached with, with the value of each of
    /// its secret fields, when the document names one. Sent only to a target
    /// whose [`Capabilities::credentials`](crate::Capabilities::credentials)
    /// is true, and only for `plan` and `apply`.
    pub credential: Option<HeldCredential>,
}

/// What the tool would do, or did, to one thing it manages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChangeAction {
    Create,
    Update,
    Delete,
    /// Destroyed and created again. It is kept apart from an update because it
    /// is the one that loses data.
    Replace,
}

impl ChangeAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeAction::Create => "create",
            ChangeAction::Update => "update",
            ChangeAction::Delete => "delete",
            ChangeAction::Replace => "replace",
        }
    }
}

impl fmt::Display for ChangeAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One difference between desired and actual state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The IR resource id when the tool's report can be traced back to one,
    /// and otherwise the tool's own name for what changes.
    pub resource: String,
    pub action: ChangeAction,
    /// One line a person can read, in the tool's own terms.
    pub summary: String,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.action, self.resource)?;
        if !self.summary.is_empty() {
            write!(f, ": {}", self.summary)?;
        }
        Ok(())
    }
}

/// A named value the tool produced.
#[derive(Clone, PartialEq)]
pub struct Output {
    pub name: String,
    pub value: Value,
    /// Marks a value that must be masked wherever it is shown or logged.
    pub sensitive: bool,
}

/// A sensitive output never prints its value, so a stray `{:?}` in a log line
/// cannot leak one.
impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("Output");
        s.field("name", &self.name);
        if self.sensitive {
            s.field("value", &"(sensitive)");
        } else {
            s.field("value", &self.value);
        }
        s.field("sensitive", &self.sensitive).finish()
    }
}

/// The preview a plan produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Everything an apply would do. Empty means nothing would change, unless
    /// `unknown` is set.
    pub changes: Vec<Change>,
    /// The tool cannot preview: it says only that something might change, or
    /// cannot say at all. An empty `changes` must not be read as "in sync"
    /// when this is set.
    pub unknown: bool,
}

impl Plan {
    /// Whether the tool reported that nothing would change.
    pub fn in_sync(&self) -> bool {
        self.changes.is_empty() && !self.unknown
    }
}

/// What an apply did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    pub changes: Vec<Change>,
    pub outputs: Vec<Output>,
}

/// The tool failing: a non-zero exit, a rejected configuration, an unreachable
/// node.
///
/// Like a [`crate::CompileError`] it is an answer and not a fault. A failed
/// apply says something about the infrastructure, and a host has to be able to
/// tell "the tool said no" from "the plugin crashed".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunError {
    pub msg: String,
    /// What the plugin ran, with any secret arguments removed.
    pub command: String,
    /// The tool's exit status, when it ran far enough to have one.
    pub exit_code: Option<i32>,
    /// The end of what the tool printed, enough to explain the failure.
    pub output: String,
    /// What an apply had already done before it failed, as far as the tool
    /// reported it. Always empty for a plan.
    pub changes: Vec<Change>,
}

impl RunError {
    /// A failure with nothing to report but a message.
    pub fn new(msg: impl Into<String>) -> Self {
        RunError {
            msg: msg.into(),
            ..RunError::default()
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)?;
        if !self.command.is_empty() {
            write!(f, " (ran `{}`", self.command)?;
            if let Some(code) = self.exit_code {
                write!(f, ", exit status {code}")?;
            }
            f.write_str(")")?;
        } else if let Some(code) = self.exit_code {
            write!(f, " (exit status {code})")?;
        }
        if !self.output.is_empty() {
            write!(f, "\n{}", self.output.trim_end())?;
        }
        Ok(())
    }
}

impl std::error::Error for RunError {}

/// Which of the tool's streams a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// One line of the tool's own output, relayed while it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub stream: LogStream,
    pub text: String,
}

/// Where a runner sends the tool's output as it appears. The host shows it to
/// the operator while the run is still going.
pub trait Log: Send + Sync {
    fn line(&self, line: LogLine);

    /// A line the tool wrote to stdout.
    fn out(&self, text: &str) {
        self.line(LogLine {
            stream: LogStream::Stdout,
            text: text.to_owned(),
        });
    }

    /// A line the tool wrote to stderr.
    fn err(&self, text: &str) {
        self.line(LogLine {
            stream: LogStream::Stderr,
            text: text.to_owned(),
        });
    }
}

/// A log that keeps nothing, for a caller with nowhere to show the output.
pub struct NoLog;

impl Log for NoLog {
    fn line(&self, _line: LogLine) {}
}

/// Hands an artifact to the real tool.
///
/// Unlike [`crate::Emitter`] it has side effects, takes as long as the tool
/// takes, and can fail for reasons that have nothing to do with the document.
/// It is a separate trait so compiling stays pure, and a plugin that only
/// compiles never implements it.
///
/// A plugin implements the methods its [`Operations`] declare. The host does
/// not call the others, so their defaults only ever answer a host that
/// ignored the declaration.
pub trait Runner: Send + Sync {
    /// Which of the methods below this plugin offers.
    fn operations(&self) -> Operations;

    /// Whether this plugin hands [`RunContext::inputs`] to its tool, as the
    /// tool's own variables or parameters.
    ///
    /// A document that declares `spec.imports` is refused for a target that
    /// does not, because its values would otherwise be dropped without a
    /// word. The default is false: say true only when every input reaches
    /// the tool.
    fn accepts_inputs(&self) -> bool {
        false
    }

    /// The settings an operator can configure this runner with: each one's
    /// name, environment variable, default and what it does. The host
    /// lists them with `meridian plugins --settings`, generates the
    /// reference page from them, and refuses a project's `meridian.yaml`
    /// that names a setting the runner does not declare. Read each one
    /// through [`crate::settings`], so the project's settings are
    /// honoured.
    ///
    /// The default declares none, which is what a plugin built before
    /// settings were declared reports.
    fn settings(&self) -> Vec<Setting> {
        Vec::new()
    }

    /// Previews what `apply` would change. It must change nothing.
    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let _ = (context, artifact, log);
        Err(RunError::new("this plugin does not plan"))
    }

    /// Makes the artifact's desired state real.
    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let _ = (context, artifact, log);
        Err(RunError::new("this plugin does not apply"))
    }

    /// Reports the values the last apply produced, read from the tool's own
    /// state, without applying anything.
    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let _ = context;
        Err(RunError::new("this plugin does not report outputs"))
    }
}
