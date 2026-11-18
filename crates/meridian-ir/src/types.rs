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

/// `endpoint.type` for an account, subscription or project of a cloud,
/// reached as an `aws`, `azure` or `gcp` credential.
pub const ENDPOINT_TYPE_CLOUD: &str = "cloud";
/// `endpoint.type` for a Kubernetes API, reached as a `kubeconfig` credential.
pub const ENDPOINT_TYPE_CLUSTER: &str = "cluster";

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
    /// The provider, such as `env`, `file`, `exec` or `keyring`.
    pub from: String,
    /// `from: env`: the environment variable. `from: keyring`: the key's
    /// description. `from: systemd-creds`: the credential's name.
    pub key: String,
    /// `from: file`: the file.
    pub path: String,
    /// `from: exec`: the program and its arguments. No shell reads them.
    pub command: Vec<String>,
    /// `from: keychain`: the item's service.
    pub service: String,
    /// `from: keychain`: the item's account.
    pub account: String,
    /// `from: secret-service`: the attributes the item is found by.
    pub attributes: BTreeMap<String, String>,
    /// A secret that cannot be found is null instead of an error.
    pub optional: bool,
    /// `from: file`: read a file that others can read as well.
    pub allow_wide_mode: bool,
    /// `from: file`, `from: systemd-creds`: keep the value's last line
    /// ending.
    pub keep_trailing_newline: bool,
    /// The places to look next, in order, when the first has nothing or
    /// cannot be used on this machine.
    pub fallback: Vec<meridian_secret::Source>,
    pub pos: Position,
}

impl Secret {
    /// Where the secret comes from first, as the secrets crate reads it.
    pub fn source(&self) -> meridian_secret::Source {
        meridian_secret::Source {
            from: self.from.clone(),
            key: self.key.clone(),
            path: self.path.clone(),
            command: self.command.clone(),
            service: self.service.clone(),
            account: self.account.clone(),
            attributes: self.attributes.clone(),
            allow_wide_mode: self.allow_wide_mode,
            keep_trailing_newline: self.keep_trailing_newline,
        }
    }

    /// Every place the secret is looked for, in order: the first, then
    /// each `fallback`.
    pub fn sources(&self) -> Vec<meridian_secret::Source> {
        std::iter::once(self.source())
            .chain(self.fallback.iter().cloned())
            .collect()
    }
}

/// `spec.credentials[].from`: the tool finds the credential for itself, in
/// its own environment, profile or agent, as it does when no credential is
/// named.
pub const CREDENTIAL_FROM_AMBIENT: &str = "ambient";

/// One entry of `spec.credentials`: an identity, declared by name and type,
/// for use wherever one is asked for. A field that holds secret material is
/// a reference to a declared secret, as `${{ secret.<name> }}`, and never a
/// value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Credential {
    /// The name it is used by.
    pub name: String,
    /// Its type: one of [`meridian_secret::credential_types`].
    pub r#type: String,
    /// Empty, or [`CREDENTIAL_FROM_AMBIENT`].
    pub from: String,
    /// The type's fields, as written: text for a plain field, a yes or a no
    /// for a flag, and for a secret field the reference as written, which
    /// resolving turns into the secret itself.
    pub fields: BTreeMap<String, Value>,
    pub pos: Position,
}

impl Credential {
    /// The type, when it is one the language has.
    pub fn kind(&self) -> Option<&'static meridian_secret::CredentialType> {
        meridian_secret::credential_type(&self.r#type)
    }

    /// Whether the tool finds the credential for itself.
    pub fn is_ambient(&self) -> bool {
        self.from == CREDENTIAL_FROM_AMBIENT
    }

    /// A field that is text, when it is there.
    pub fn text(&self, field: &str) -> Option<&str> {
        match self.fields.get(field) {
            Some(Value::String(text)) => Some(text),
            _ => None,
        }
    }

    /// A flag, which is no when it is left out.
    pub fn flag(&self, field: &str) -> bool {
        matches!(self.fields.get(field), Some(Value::Bool(true)))
    }

    /// The fields that hold secret material, with the slot each stands for
    /// once the document is resolved.
    pub fn secrets(&self) -> Vec<(&str, meridian_value::SecretRef)> {
        self.fields
            .iter()
            .filter_map(|(name, value)| match value {
                Value::Secret(at) => Some((name.as_str(), *at)),
                _ => None,
            })
            .collect()
    }

    /// The credential in a few words, for a plan: `deploy (ssh, user
    /// deploy)`, never any part of a secret.
    pub fn summary(&self) -> String {
        let mut what = self.r#type.clone();
        for field in SUMMARISED_FIELDS {
            if let Some(value) = self.text(field) {
                what.push_str(&format!(", {field} {value}"));
            }
        }
        if self.is_ambient() {
            what.push_str(", ambient");
        }
        format!("{} ({what})", self.name)
    }
}

/// The plain fields a credential's summary names, in this order: who it is
/// and where it points, and never anything secret.
pub const SUMMARISED_FIELDS: &[&str] = &["user", "profile", "region", "project", "subscriptionId"];

/// Binds a ResourceSet to the node it configures. `from: infrastructure`
/// resolves `ref`/`output` against a sibling Infrastructure document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetHost {
    pub from: String,
    pub r#ref: String,
    pub output: String,
    pub host: String,
    /// The credential the host is reached with, by the name
    /// `spec.credentials` declares it under; empty for the tool's own.
    pub credential: String,
    pub pos: Position,
}

/// `spec.endpoint`: the place a document is applied to that is not a host,
/// and the credential it is reached as. A host is `targetHost`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Endpoint {
    /// [`ENDPOINT_TYPE_CLOUD`] or [`ENDPOINT_TYPE_CLUSTER`].
    pub r#type: String,
    /// The credential the endpoint is reached with, by the name
    /// `spec.credentials` declares it under.
    pub credential: String,
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
    /// `spec.credentials`: the identities the document names.
    pub credentials: Vec<Credential>,
    /// `spec.endpoint`: the cloud the document provisions in, and the
    /// credential it is reached as.
    pub endpoint: Option<Endpoint>,
    /// `spec.settings`: the tool's own settings, which the target checks as
    /// it checks a resource's params.
    pub settings: BTreeMap<String, Value>,
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
    /// `spec.credentials`: the identities the document names.
    pub credentials: Vec<Credential>,
    pub target_host: Option<TargetHost>,
    /// `spec.endpoint`: the cloud or cluster the document is applied to,
    /// and the credential it is reached as, for a target that reaches no
    /// host.
    pub endpoint: Option<Endpoint>,
    /// `spec.settings`: the tool's own settings, which the target checks as
    /// it checks a resource's params.
    pub settings: BTreeMap<String, Value>,
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

    /// `spec.settings`: the tool's own settings; empty when the document
    /// sets none.
    pub fn settings(&self) -> &BTreeMap<String, Value> {
        match self {
            Document::Infrastructure(d) => &d.spec.settings,
            Document::ResourceSet(d) => &d.spec.settings,
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

    /// `spec.credentials`.
    pub fn credentials(&self) -> &[Credential] {
        match self {
            Document::Infrastructure(d) => &d.spec.credentials,
            Document::ResourceSet(d) => &d.spec.credentials,
        }
    }

    /// `spec.endpoint`, when the document names one.
    pub fn endpoint(&self) -> Option<&Endpoint> {
        match self {
            Document::Infrastructure(d) => d.spec.endpoint.as_ref(),
            Document::ResourceSet(d) => d.spec.endpoint.as_ref(),
        }
    }

    /// The credential the document is applied as, when it names one: the
    /// host's, through `targetHost.credential`, or the endpoint's, through
    /// `endpoint.credential`. Validation refuses both at once.
    pub fn named_credential(&self) -> Option<&Credential> {
        let name = self
            .as_resource_set()
            .and_then(|d| d.spec.target_host.as_ref())
            .map(|th| th.credential.as_str())
            .filter(|name| !name.is_empty())
            .or_else(|| self.endpoint().map(|e| e.credential.as_str()))?;
        self.credentials().iter().find(|c| c.name == name)
    }

    /// The credential declared as `name`, if there is one.
    pub fn credential(&self, name: &str) -> Option<&Credential> {
        self.credentials().iter().find(|c| c.name == name)
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
