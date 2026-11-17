use std::collections::BTreeMap;
use std::fmt;

use crate::error::Position;
use meridian_value::Value;

/// The first language version. Which versions a build reads, and in what
/// state, is decided by [`crate::VersionTable`]; any value it does not hold is
/// an explicit error, never a silent partial parse.
pub const API_VERSION_V1: &str = "meridian/v1";

/// `targetHost.from` value that resolves against another document's outputs.
pub const TARGET_HOST_FROM_INFRASTRUCTURE: &str = "infrastructure";
/// `targetHost.from` value that names an already-existing node directly.
pub const TARGET_HOST_FROM_INLINE: &str = "inline";

/// The only event a notify can fire on: the notifying resource really
/// changed. It is also what a notify with no `on` means.
pub const NOTIFY_ON_CHANGED: &str = "changed";

/// The two tiers of the IR: provisioning and host configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Infrastructure,
    ResourceSet,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Infrastructure => "Infrastructure",
            Kind::ResourceSet => "ResourceSet",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The policy applied when an edge points at a resource that is not in the
/// compiled graph, typically because `when` pruned it.
///
/// Parsing keeps whatever the document said; an unrecognised policy is a
/// validation error, so it is reported alongside a document's other problems
/// instead of stopping the parse.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum IfMissing {
    /// Not stated. Treated as [`IfMissing::Error`].
    #[default]
    Unset,
    /// A dangling edge is an authoring mistake.
    Error,
    Skip,
    Unknown(String),
}

impl IfMissing {
    pub(crate) fn from_text(s: &str) -> Self {
        match s {
            "" => IfMissing::Unset,
            "error" => IfMissing::Error,
            "skip" => IfMissing::Skip,
            other => IfMissing::Unknown(other.to_owned()),
        }
    }

    /// Whether a dangling edge with this policy is dropped instead of refused.
    pub fn skips(&self) -> bool {
        matches!(self, IfMissing::Skip)
    }
}

/// Document identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

/// A hard ordering constraint: the referenced resource must be applied before
/// the resource declaring it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dependency {
    pub resource: String,
    pub if_missing: IfMissing,
    /// A condition on the edge itself; empty means always. A false one drops
    /// the entry and leaves both resources.
    pub when: String,
    pub pos: Position,
}

/// A conditional, deferred trigger. Deliberately a separate type from
/// [`Dependency`], because it is not an ordering constraint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notification {
    pub resource: String,
    /// The event that fires the notify. Empty means [`NOTIFY_ON_CHANGED`],
    /// which is also the only value validation accepts: every target's
    /// notification mechanism fires on a change and on nothing else.
    pub on: String,
    pub action: String,
    pub if_missing: IfMissing,
    /// A condition on the notify itself; empty means always.
    pub when: String,
    pub pos: Position,
}

/// One declared unit of desired state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resource {
    pub id: String,
    pub r#type: String,
    pub state: String,
    pub params: BTreeMap<String, Value>,
    pub depends_on: Vec<Dependency>,
    pub notifies: Vec<Notification>,
    pub when: String,
    pub runtime_when: String,
    /// Position in the document's declaration order. It is the sole tiebreak
    /// for topological sort, so downstream ordering never depends on anything
    /// but what the author wrote.
    pub index: usize,
    pub pos: Position,
}

/// A value an Infrastructure document exposes to ResourceSet documents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub name: String,
    pub value: String,
    /// A condition on the output; empty means always. A false one means the
    /// output is not declared.
    pub when: String,
    pub pos: Position,
}

/// One entry of `spec.secrets`: a secret's name and where it comes from. Its
/// value is never written in a document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Secret {
    /// The name it is read by, as `secret.<name>`.
    pub name: String,
    /// The provider: `env`, `file` or `exec`.
    pub from: String,
    /// `from: env`: the environment variable.
    pub key: String,
    /// `from: file`: the file.
    pub path: String,
    /// `from: exec`: the program and its arguments. No shell reads them.
    pub command: Vec<String>,
    /// A secret that cannot be found is null instead of an error.
    pub optional: bool,
    /// `from: file`: read a file that others can read as well.
    pub allow_wide_mode: bool,
    /// `from: file` and `from: exec`: keep the value's last line ending.
    pub keep_trailing_newline: bool,
    pub pos: Position,
}

impl Secret {
    /// Where the secret comes from, as the secrets crate reads it.
    pub fn source(&self) -> meridian_secret::Source {
        meridian_secret::Source {
            from: self.from.clone(),
            key: self.key.clone(),
            path: self.path.clone(),
            command: self.command.clone(),
            allow_wide_mode: self.allow_wide_mode,
            keep_trailing_newline: self.keep_trailing_newline,
        }
    }
}

/// Binds a ResourceSet to the node it configures. `from: infrastructure`
/// resolves `ref`/`output` against a sibling Infrastructure document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetHost {
    pub from: String,
    pub r#ref: String,
    pub output: String,
    pub host: String,
    pub pos: Position,
}

/// `spec.imports[].from`: a file another document compiles to.
pub const IMPORT_FROM_ARTIFACT: &str = "artifact";
/// `spec.imports[].from`: a value another document's apply produced.
pub const IMPORT_FROM_OUTPUT: &str = "output";

/// A value a document is handed from another document of the workflow. It
/// reaches the document's tool as the tool's own variable or parameter of
/// this name; the core never splices it into the document's text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Import {
    /// The name the value arrives under.
    pub name: String,
    /// [`IMPORT_FROM_ARTIFACT`] or [`IMPORT_FROM_OUTPUT`].
    pub from: String,
    /// The `metadata.name` of the document the value comes from.
    pub r#ref: String,
    /// For `from: artifact`, the compiled file whose text is the value.
    pub file: String,
    /// For `from: output`, the output whose value it is.
    pub output: String,
    /// Whether the value is masked wherever Meridian prints.
    pub sensitive: bool,
    pub pos: Position,
}

/// The body of a `kind: Infrastructure` document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InfrastructureSpec {
    pub target: String,
    pub target_version: String,
    /// `spec.when`: a condition on the whole document; empty means always.
    pub when: String,
    /// `spec.variables`: named values, worked out once per document.
    pub variables: BTreeMap<String, Value>,
    /// `spec.conditions`: named conditions, read as `cond.<name>`.
    pub conditions: BTreeMap<String, String>,
    /// `spec.secrets`: the secrets the document reads, as `secret.<name>`.
    pub secrets: Vec<Secret>,
    pub imports: Vec<Import>,
    pub resources: Vec<Resource>,
    pub outputs: Vec<Output>,
}

/// The provisioning tier: VMs, networks, and anything else that compiles to an
/// infrastructure-as-code tool.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Infrastructure {
    pub api_version: String,
    pub metadata: Metadata,
    pub spec: InfrastructureSpec,
    pub pos: Position,
}

impl Infrastructure {
    /// The named output, if it was declared.
    pub fn output(&self, name: &str) -> Option<&Output> {
        self.spec.outputs.iter().find(|o| o.name == name)
    }
}

/// The body of a `kind: ResourceSet` document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceSetSpec {
    pub target: String,
    pub target_version: String,
    /// `spec.when`: a condition on the whole document; empty means always.
    pub when: String,
    /// `spec.variables`: named values, worked out once per document.
    pub variables: BTreeMap<String, Value>,
    /// `spec.conditions`: named conditions, read as `cond.<name>`.
    pub conditions: BTreeMap<String, String>,
    /// `spec.secrets`: the secrets the document reads, as `secret.<name>`.
    pub secrets: Vec<Secret>,
    pub target_host: Option<TargetHost>,
    pub imports: Vec<Import>,
    pub resources: Vec<Resource>,
}

/// The host-configuration tier: packages, files, services.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceSet {
    pub api_version: String,
    pub metadata: Metadata,
    pub spec: ResourceSetSpec,
    pub pos: Position,
}

/// A parsed document of either kind.
#[derive(Clone, Debug, PartialEq)]
pub enum Document {
    Infrastructure(Infrastructure),
    ResourceSet(ResourceSet),
}

impl Document {
    pub fn kind(&self) -> Kind {
        match self {
            Document::Infrastructure(_) => Kind::Infrastructure,
            Document::ResourceSet(_) => Kind::ResourceSet,
        }
    }

    pub fn api_version(&self) -> &str {
        match self {
            Document::Infrastructure(d) => &d.api_version,
            Document::ResourceSet(d) => &d.api_version,
        }
    }

    /// `metadata.name`.
    pub fn name(&self) -> &str {
        match self {
            Document::Infrastructure(d) => &d.metadata.name,
            Document::ResourceSet(d) => &d.metadata.name,
        }
    }

    /// `metadata.labels`.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        match self {
            Document::Infrastructure(d) => &d.metadata.labels,
            Document::ResourceSet(d) => &d.metadata.labels,
        }
    }

    /// `spec.targetVersion`; empty when the document does not ask for one.
    pub fn target_version(&self) -> &str {
        match self {
            Document::Infrastructure(d) => &d.spec.target_version,
            Document::ResourceSet(d) => &d.spec.target_version,
        }
    }

    /// `spec.target`.
    pub fn target(&self) -> &str {
        match self {
            Document::Infrastructure(d) => &d.spec.target,
            Document::ResourceSet(d) => &d.spec.target,
        }
    }

    /// `spec.imports`: the values the document is handed from others.
    pub fn imports(&self) -> &[Import] {
        match self {
            Document::Infrastructure(d) => &d.spec.imports,
            Document::ResourceSet(d) => &d.spec.imports,
        }
    }

    /// `spec.when`; empty when the document is unconditional.
    pub fn when(&self) -> &str {
        match self {
            Document::Infrastructure(d) => &d.spec.when,
            Document::ResourceSet(d) => &d.spec.when,
        }
    }

    /// `spec.variables`.
    pub fn variables(&self) -> &BTreeMap<String, Value> {
        match self {
            Document::Infrastructure(d) => &d.spec.variables,
            Document::ResourceSet(d) => &d.spec.variables,
        }
    }

    /// `spec.conditions`.
    pub fn conditions(&self) -> &BTreeMap<String, String> {
        match self {
            Document::Infrastructure(d) => &d.spec.conditions,
            Document::ResourceSet(d) => &d.spec.conditions,
        }
    }

    /// `spec.secrets`.
    pub fn secrets(&self) -> &[Secret] {
        match self {
            Document::Infrastructure(d) => &d.spec.secrets,
            Document::ResourceSet(d) => &d.spec.secrets,
        }
    }

    /// `spec.outputs`; a ResourceSet declares none.
    pub fn outputs(&self) -> &[Output] {
        match self {
            Document::Infrastructure(d) => &d.spec.outputs,
            Document::ResourceSet(_) => &[],
        }
    }

    pub fn resources(&self) -> &[Resource] {
        match self {
            Document::Infrastructure(d) => &d.spec.resources,
            Document::ResourceSet(d) => &d.spec.resources,
        }
    }

    pub fn position(&self) -> &Position {
        match self {
            Document::Infrastructure(d) => &d.pos,
            Document::ResourceSet(d) => &d.pos,
        }
    }

    pub fn as_infrastructure(&self) -> Option<&Infrastructure> {
        match self {
            Document::Infrastructure(d) => Some(d),
            Document::ResourceSet(_) => None,
        }
    }

    pub fn as_resource_set(&self) -> Option<&ResourceSet> {
        match self {
            Document::ResourceSet(d) => Some(d),
            Document::Infrastructure(_) => None,
        }
    }
}

impl From<Infrastructure> for Document {
    fn from(d: Infrastructure) -> Self {
        Document::Infrastructure(d)
    }
}

impl From<ResourceSet> for Document {
    fn from(d: ResourceSet) -> Self {
        Document::ResourceSet(d)
    }
}
