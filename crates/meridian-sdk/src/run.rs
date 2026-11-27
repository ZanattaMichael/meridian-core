//! What a plugin is, beyond a compiler: its category, the operations it
//! offers, and the [`Runner`] half that hands an artifact to the real tool.

use std::collections::BTreeMap;
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
    /// Asks `plan` to hand back the tool's own saved plan in [`Plan::saved`],
    /// for `meridian plan --out`. A runner that cannot, or whose saved plan
    /// would carry a secret value, leaves it `None`; that is never an error.
    pub save_plan: bool,
    /// The plan `apply` must carry out exactly: what `plan` saved for this
    /// very artifact in an earlier run. A runner handed one applies that
    /// plan and nothing else, and fails when its tool finds the plan stale.
    /// Sent only to a runner whose [`Runner::saves_plans`] is true.
    pub saved_plan: Option<SavedPlan>,
    /// How much of its own debug output the tool is asked for: 0 for none,
    /// up to [`crate::debug::MAX_LEVEL`]. A runner reads it through
    /// [`crate::debug::pick`] with its [`Runner::debug_levels`].
    pub debug_level: u32,
}

/// A tool's own saved plan, as the plugin chose to keep it: a Terraform plan
/// file, a Pulumi plan, the name of a CloudFormation change set.
///
/// The host never reads `data`. It keeps it in the plan file between the
/// plan and the apply, so `data` must hold no secret value: a runner whose
/// tool would write one into its saved plan saves nothing instead.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SavedPlan {
    pub data: Vec<u8>,
    /// What the plan would do, as the plan reported it. Filled in by the
    /// host when it hands the plan back to `apply`, so a runner whose tool
    /// gives no second account of what it applied can still report it.
    pub changes: Vec<Change>,
}

impl SavedPlan {
    /// A saved plan holding `data`, as a runner returns it from `plan`.
    pub fn new(data: impl Into<Vec<u8>>) -> SavedPlan {
        SavedPlan {
            data: data.into(),
            changes: Vec::new(),
        }
    }
}

/// The bytes are the tool's, and can be long and binary, so only their
/// length is shown.
impl fmt::Debug for SavedPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedPlan")
            .field("data", &format_args!("{} bytes", self.data.len()))
            .field("changes", &self.changes)
            .finish()
    }
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
    /// Taken under management as it is, with nothing else changed: what a
    /// resource's `adopt` does on its first plan. An adoption that also
    /// changes the thing is reported as the change.
    Adopt,
}

impl ChangeAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeAction::Create => "create",
            ChangeAction::Update => "update",
            ChangeAction::Delete => "delete",
            ChangeAction::Replace => "replace",
            ChangeAction::Adopt => "adopt",
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
    /// The tool's own saved plan, when [`RunContext::save_plan`] asked for
    /// one and the runner could save it. `None` otherwise; an apply from the
    /// plan file then checks the artifact's hash and plans again.
    pub saved: Option<SavedPlan>,
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

/// One resource as the tool reads it now: what [`Runner::inspect`] answers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inspection {
    /// False when the tool has no such resource: never made, or removed.
    pub found: bool,
    /// Its properties now, keyed by the document's param names where the
    /// plugin can map them and by the tool's own names otherwise.
    pub actual: Data,
    /// Whether the tool says the resource is as declared; `None` when it
    /// cannot say.
    pub in_desired_state: Option<bool>,
    /// The native command run, as a person would type it, with no secret in
    /// it.
    pub command: String,
}

/// What [`Runner::act`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Acted {
    /// The native command run, as a person would type it, with no secret in
    /// it.
    pub command: String,
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
    /// What an apply or a destroy had already done before it failed, as far
    /// as the tool reported it. Always empty for a plan.
    pub changes: Vec<Change>,
    /// Each resource the tool failed on, as its report says. See
    /// [`crate::failure`].
    pub failures: Vec<crate::failure::Failure>,
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

    /// Whether the host has asked for this run to stop: the operator
    /// interrupted it, or it ran past its time limit. [`crate::Tool::run`]
    /// checks this while the tool runs and stops the tool's process when it
    /// turns true. A runner that does its own work in a loop should check it
    /// between steps and return a [`RunError`] carrying what it had done.
    fn cancelled(&self) -> bool {
        false
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

    /// Whether this runner hands back its tool's own saved plan from
    /// [`Runner::plan`] when [`RunContext::save_plan`] asks for one, and
    /// carries out exactly that plan in [`Runner::apply`] when handed it in
    /// [`RunContext::saved_plan`].
    ///
    /// Say true only when both halves hold: a saved plan is what a reviewer
    /// approved, and an apply handed one must do that and nothing else, or
    /// fail. The default is false: `meridian apply --plan` then checks that
    /// the document still compiles to the planned artifact and plans again,
    /// and the plan file says so for the stage.
    fn saves_plans(&self) -> bool {
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

    /// What each debug level does to the tool, first level first: the
    /// arguments and environment [`RunContext::debug_level`] adds. The host
    /// lists them with `meridian plugins` and on the capability matrix. The
    /// default is none: `--tool-debug` then changes nothing for this target.
    fn debug_levels(&self) -> &'static [crate::debug::DebugLevel] {
        &[]
    }

    /// The debug levels of the other tools this runner may run instead of
    /// its main one, each named: Bolt beside `puppet apply`. Listed after
    /// the main table by `meridian plugins`.
    fn more_debug_levels(&self) -> &'static [(&'static str, &'static [crate::debug::DebugLevel])] {
        &[]
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

    /// Whether [`Runner::identifiers`] is answered. Say true only when the
    /// identifiers are what the tool's import takes back, so that `meridian
    /// retarget` can write them as `adopt` for another target.
    fn reads_identifiers(&self) -> bool {
        false
    }

    /// The tool's own identifier of each thing it manages, by the id of the
    /// resource it was declared as, read from the tool's state without
    /// changing anything. A resource the tool has no record of is left out.
    fn identifiers(&self, context: &RunContext) -> Result<BTreeMap<String, String>, RunError> {
        let _ = context;
        Err(RunError::new("this plugin does not read identifiers"))
    }

    /// Whether [`Runner::destroy`] and [`Runner::plan_destroy`] are
    /// answered. Say true only when `destroy` removes everything the tool
    /// created for the artifact, as the tool's own destroy does. A target
    /// whose tool keeps no record of what it made, as most configuration
    /// management tools, says false, and `meridian destroy` leaves its
    /// stages in place.
    fn destroys(&self) -> bool {
        false
    }

    /// Previews what `destroy` would remove. It must change nothing.
    fn plan_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let _ = (context, artifact, log);
        Err(RunError::new("this plugin does not destroy"))
    }

    /// Removes everything the tool created for the artifact, and reports
    /// what it removed. A failure carries what was removed before it in
    /// [`RunError::changes`].
    fn destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let _ = (context, artifact, log);
        Err(RunError::new("this plugin does not destroy"))
    }

    /// Whether [`Runner::inspect`] is answered.
    fn inspects(&self) -> bool {
        false
    }

    /// Reads the resource `resource` of the artifact as it is now on
    /// `endpoint`, the context's host unless the operator named another.
    /// It must change nothing: no apply, no write to the tool's state.
    fn inspect(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        endpoint: &str,
    ) -> Result<Inspection, RunError> {
        let _ = (context, artifact, resource, endpoint);
        Err(RunError::new("this plugin does not inspect"))
    }

    /// The notify actions this runner performs itself, by resource type,
    /// for a notify its tool cannot express. List only what the emitter
    /// cannot compile natively or synthetically: the host takes a covered
    /// notify out of the tree before `emit`. The default is none.
    fn acts(&self) -> Vec<crate::ActSchema> {
        Vec::new()
    }

    /// Performs `action` on the resource `resource` of the artifact, after
    /// the stage applied. Asked only for a type and action [`Runner::acts`]
    /// lists.
    fn act(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        action: &str,
        log: &dyn Log,
    ) -> Result<Acted, RunError> {
        let _ = (context, artifact, resource, action, log);
        Err(RunError::new("this plugin does not act"))
    }

    /// Whether [`Runner::facts`] is answered. Say true only when the plugin
    /// reads a fixed set of facts about an endpoint over the endpoint's own
    /// connection, changing nothing.
    fn gathers_facts(&self) -> bool {
        false
    }

    /// Reads a fixed set of facts about `endpoint` over the endpoint's own
    /// connection, changing nothing. The host maps them to the language's
    /// fixed names (`facts.os.family`, `facts.kernel`, ...) and evaluates
    /// `when` over `facts.*` per endpoint before compiling its artifact.
    fn facts(&self, context: &RunContext, endpoint: &str) -> Result<Facts, RunError> {
        let _ = (context, endpoint);
        Err(RunError::new("this plugin does not gather facts"))
    }
}

/// A fixed set of facts about one endpoint, as [`Runner::facts`] reads them:
/// the language's own names (`facts.os.family`, `facts.kernel`, ...) mapped to
/// their values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// Named facts, keyed by the fixed names the language uses under `facts.*`.
    pub values: Data,
}
