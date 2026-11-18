//! Compiles a Meridian infrastructure document into a Pulumi YAML program and
//! runs it with `pulumi`.
//!
//! # What a document looks like to this target
//!
//! Pulumi's vocabulary is its providers', and there are thousands of resource
//! types. This target does not hide them behind a smaller one: a resource's
//! `type` is the Pulumi type token and its `params` are that type's
//! properties, exactly as the provider documents them.
//!
//! | `type` | Becomes |
//! |---|---|
//! | `aws:ec2:Instance`, `random:index:RandomPet`, any `<package>:<module>:<Type>` | an entry under `resources`, named by the resource's id |
//! | `pulumi:providers:<package>` | the same: an explicit provider is a resource in Pulumi |
//! | `invoke.<function>` | an entry under `variables` that calls the function with `fn::invoke` |
//! | `config` | an entry under `config`: a value the program takes, or a provider setting |
//! | `stack` | nothing in the program: the name of the stack and where its state lives |
//!
//! Two params are not properties. `resourceOptions` on a resource holds
//! Pulumi's resource options (`protect`, `provider`, `ignoreChanges`, ...) and
//! `invokeOptions` on an `invoke.<function>` holds the options of the call.
//!
//! What Meridian adds is the wiring. `${web_vm.publicIp}` in a param or an
//! output refers to another resource of the document by its id. Pulumi YAML
//! spells a reference the same way, so it is checked and handed over: a
//! reference to something the document does not declare is refused when the
//! document is compiled, not when Pulumi runs. `dependsOn` becomes the
//! `dependsOn` option unless a reference already implies the order. A
//! document's outputs become the program's `outputs`, which is how a later
//! stage learns the address of the machine this one creates.
//!
//! # Why the output is Pulumi YAML
//!
//! Pulumi programs are usually written in a general-purpose language. A
//! compiler that wrote TypeScript or Python would have to choose a language
//! runtime for the operator and install its packages. Pulumi YAML is a
//! declarative program the `pulumi` binary runs by itself, with every
//! provider available to it.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod runner;
mod schemas;
mod sort;
mod transform;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{
    holds_secret, Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position,
    Resource, ResourceGraph, Value,
};

pub use emit::{PROGRAM_PATH, STACK_PATH};
pub use runner::{PulumiRunner, PROGRAM_VARIABLE};
pub use schemas::PulumiSchemas;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "pulumi";

/// The stack a document deploys to when it does not name one.
pub const DEFAULT_STACK: &str = "dev";

/// The param of a resource that holds Pulumi's resource options.
pub(crate) const RESOURCE_OPTIONS: &str = "resourceOptions";
/// The param of an `invoke.<function>` that holds the options of the call.
pub(crate) const INVOKE_OPTIONS: &str = "invokeOptions";

/// What a resource of the document became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A value the program takes, or a provider setting.
    Config,
    /// A function call, written under `variables`.
    Invoke,
    /// A resource, including an explicit provider.
    Managed,
}

impl Kind {
    /// What a message calls an entry of this kind.
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Config => "configuration value",
            Kind::Invoke => "function call",
            Kind::Managed => "resource",
        }
    }
}

/// One named thing in the program.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub kind: Kind,
    /// The resource's id in the document.
    pub id: String,
    /// Its name in the program. It is the id, except for a configuration
    /// value given another name with `name`.
    pub name: String,
    /// The type token of a resource or the function token of a call. Empty
    /// for a configuration value.
    pub token: String,
    /// A resource's properties, a call's arguments, or a configuration
    /// value's declaration.
    pub body: BTreeMap<String, Value>,
    /// Resource options or the options of a call.
    pub options: BTreeMap<String, Value>,
    /// The ids of the other resources this entry refers to.
    pub refs: BTreeSet<String>,
    pub pos: Position,
}

impl Entry {
    /// Whether the rest of the program can read this entry as `${name}`. A
    /// provider setting such as `aws:region` is read by the provider alone.
    pub fn addressable(&self) -> bool {
        !(self.kind == Kind::Config && self.name.contains(':'))
    }
}

/// How the program is deployed. None of it is part of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The stack's name.
    pub stack: String,
    /// The backend that keeps the stack's state, as a URL. Empty leaves the
    /// choice to the runner.
    pub backend: String,
    /// The secrets provider a new stack is created with. Empty is Pulumi's
    /// default for the backend.
    pub encryption: String,
    /// The id and position of the `stack` resource, when there is one.
    pub declared: Option<(String, Position)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            stack: DEFAULT_STACK.to_owned(),
            backend: String::new(),
            encryption: String::new(),
            declared: None,
        }
    }
}

/// A whole program and how to deploy it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Program {
    /// The project's name, which is the document's.
    pub project: String,
    pub settings: Settings,
    pub entries: Vec<Entry>,
    /// Output name to its value.
    pub outputs: Vec<(String, Value, Position)>,
}

impl Program {
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &Rule, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// A refusal that is about the document and not one resource.
pub(crate) fn refuse_at(resource: &str, pos: &Position, rule: &Rule, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        msg,
        pos: pos.clone(),
    }
}

/// The configuration key the secret `name` is handed over as:
/// `meridian_secret_<name>`.
pub fn secret_key(name: &str) -> String {
    format!("meridian_secret_{name}")
}

/// How a secret is written in a param: a reference to its configuration
/// value, which Pulumi YAML reads by name.
pub fn secret_reference(name: &str) -> String {
    format!("${{{}}}", secret_key(name))
}

/// The Pulumi emitter.
pub struct Pulumi;

/// Refuses a secret in a resource of one of `types`: their params are not
/// handed to the tool by a private channel but become `what`, so a secret
/// there would be written in the open.
fn refuse_secret_in(graph: &ResourceGraph, types: &[&str], what: &str) -> Result<(), CompileError> {
    for r in graph
        .resources
        .iter()
        .filter(|r| types.contains(&r.r#type.as_str()))
    {
        if let Some((name, _)) = r.params.iter().find(|(_, value)| holds_secret(value)) {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_SECRET_PLACE,
                format!(
                    "param {name:?} of the {} holds a secret, and {what}, where a secret cannot \
                     go. A secret goes in a resource's params, which the tool is handed by a \
                     private channel",
                    r.r#type
                ),
            ));
        }
    }
    Ok(())
}

impl Emitter for Pulumi {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        [
            "<package>:<module>:<Type>",
            "config",
            "invoke.<function>",
            "stack",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Pulumi has no handler to run when something changes, and nothing in a
    /// Pulumi YAML program makes one resource react to a change in another.
    /// `replaceOnChanges` watches a resource's own properties only. So no
    /// notify is claimed, and a document that uses one is refused.
    ///
    /// There is no apply-time conditional either: Pulumi YAML has no way to
    /// leave a resource out depending on what it finds.
    ///
    /// A secret is taken: it reaches the program as a secret configuration
    /// value, set from the tool's standard input.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            secrets: true,
            ..Capabilities::default()
        }
    }

    fn category(&self) -> Category {
        Category::InfrastructureAsCode
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A secret in a param is written as a reference to a secret
        // configuration value, which the runner sets from standard input.
        refuse_secret_in(graph, &["stack", "config"], "the stack's settings are the command line `pulumi` is given and a configuration value's declaration is written into the program")?;
        let graph = &graph.secrets_written_as(secret_reference);
        let mut program = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut program)?;
        validate::validate(&program)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&program),
            },
            warnings: Vec::new(),
        })
    }
}
