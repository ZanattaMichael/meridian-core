//! Compiles a Meridian infrastructure document into a Packer build template
//! and runs it with `packer`.
//!
//! # What a document looks like to this target
//!
//! One document is one Packer build. Packer's vocabulary is its plugins', so
//! this target does not hide it behind a smaller one: after the prefix that
//! says what part of a build a resource is, its `type` is Packer's own type
//! and its `params` are that type's arguments, exactly as its plugin documents
//! them.
//!
//! | `type` | Becomes |
//! |---|---|
//! | `source.<builder>`, such as `source.amazon-ebs` | a `source` block, built by the document's build |
//! | `provisioner.<type>` | a `provisioner` of the build |
//! | `post-processor.<type>` | a `post-processor` of the build |
//! | `error-cleanup-provisioner.<type>` | the build's `error-cleanup-provisioner` |
//! | `data.<type>` | a `data` block |
//! | `variable` | a `variable` block |
//! | `local` | an entry of `locals`, from the param `value` |
//! | `plugin` | an entry of `required_plugins` |
//!
//! What Meridian adds is the wiring. Provisioners run in the order the
//! document's `dependsOn` gives them. A post-processor that depends on another
//! takes that one's artifact as its input, which Packer writes as a
//! `post-processors` sequence. `${id}` in a param refers to a variable, a
//! local or a data source of the document by its id and is rewritten to
//! Packer's own address for it.
//!
//! # Outputs
//!
//! Packer has no outputs, but an image is only useful to a later stage that
//! knows its id. Every build therefore ends with Packer's `manifest`
//! post-processor, and a document's outputs read from what it records:
//! `${ubuntu.id}` is the id of the image the source `ubuntu` produced.
//!
//! # Why the output is JSON
//!
//! The template is written in Packer's JSON syntax (`build.pkr.json`), not
//! HCL. In HCL a nested block and a map-valued argument are written
//! differently, and only a plugin's schema says which one a param is. In the
//! JSON syntax Packer makes that decision itself, so a document needs no
//! schema to compile correctly.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod order;
mod outputs;
mod runner;
mod transform;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{
    holds_secret, Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position,
    Resource, ResourceGraph, Value,
};

pub use emit::{BUILD_PATH, CONFIG_PATH, MANIFEST_PATH};
pub use runner::{PackerRunner, PROGRAM_VARIABLE};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "packer";

/// The name of the input variable the secret `name` is handed over as:
/// `meridian_secret_<name>`.
pub fn secret_variable(name: &str) -> String {
    format!("meridian_secret_{name}")
}

/// How a secret is written in the template: a reference to its variable.
pub fn secret_reference(name: &str) -> String {
    format!("${{var.{}}}", secret_variable(name))
}

/// Declares the sensitive input variable each of the graph's secrets is
/// handed over as, so the template is complete on its own and Packer masks
/// the value wherever it would print it.
fn secret_variables(graph: &ResourceGraph, template: &mut Template) {
    for secret in &graph.secrets {
        template.blocks.push(Block {
            kind: Kind::Variable,
            r#type: String::new(),
            name: secret_variable(&secret.name),
            body: BTreeMap::from([
                ("type".to_owned(), Value::from("string")),
                ("sensitive".to_owned(), Value::Bool(true)),
            ]),
            refs: BTreeSet::new(),
            pos: Position::default(),
        });
    }
}

/// What part of a template a resource became.
///
/// The order of the variants is the order Packer deals with them in: what it
/// needs installed, what it is told, what it looks up, what it builds from,
/// what it runs on the machine, and what it does with the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Plugin,
    Variable,
    Data,
    Local,
    Source,
    Provisioner,
    ErrorCleanup,
    PostProcessor,
}

impl Kind {
    /// The stage of a build the kind belongs to. A dependency from an earlier
    /// stage to a later one is already how Packer works.
    pub fn stage(self) -> u8 {
        match self {
            Kind::Plugin => 0,
            Kind::Variable => 1,
            Kind::Data | Kind::Local => 2,
            Kind::Source => 3,
            Kind::Provisioner | Kind::ErrorCleanup => 4,
            Kind::PostProcessor => 5,
        }
    }

    /// The kind as a sentence names it.
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Plugin => "plugin requirement",
            Kind::Variable => "variable",
            Kind::Data => "data source",
            Kind::Local => "local",
            Kind::Source => "source",
            Kind::Provisioner => "provisioner",
            Kind::ErrorCleanup => "error-cleanup provisioner",
            Kind::PostProcessor => "post-processor",
        }
    }
}

/// One block of the template.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Block {
    pub kind: Kind,
    /// Packer's type: `amazon-ebs` for a source, `shell` for a provisioner.
    /// Empty for a variable, a local and a plugin requirement.
    pub r#type: String,
    /// The block's name, which is the resource's id.
    pub name: String,
    pub body: BTreeMap<String, Value>,
    /// The ids of the other resources this block's arguments refer to.
    pub refs: BTreeSet<String>,
    pub pos: Position,
}

impl Block {
    /// How a build names this source: `amazon-ebs.ubuntu`.
    pub fn source_name(&self) -> String {
        format!("{}.{}", self.r#type, self.name)
    }
}

/// A value the document promises, as text that refers to sources by id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputTemplate {
    pub name: String,
    /// In the document's own syntax: `${ubuntu.id}`, with literal text
    /// escaped so it reads back unchanged.
    pub template: String,
    pub pos: Position,
}

/// A whole template: one build.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Template {
    /// The build's name, which is the document's.
    pub name: String,
    /// Every block, in the order the document's dependencies give.
    pub blocks: Vec<Block>,
    /// The post-processor sequences, each a list of indexes into `blocks`.
    /// Filled in by the ordering stage.
    pub chains: Vec<Vec<usize>>,
    pub outputs: Vec<OutputTemplate>,
}

impl Template {
    pub fn block(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.name == id)
    }

    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Block> {
        self.blocks.iter().filter(move |b| b.kind == kind)
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

/// The Packer emitter.
pub struct Packer;

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

impl Emitter for Packer {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        [
            "data.<type>",
            "error-cleanup-provisioner.<type>",
            "local",
            "plugin",
            "post-processor.<type>",
            "provisioner.<type>",
            "source.<builder>",
            "variable",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// A build runs its provisioners once, in order, on a machine it throws
    /// away. Nothing in it reacts to a change, and nothing in it is decided on
    /// the machine at apply time. A secret is taken: it reaches Packer as a
    /// sensitive input variable read from a private file.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: false,
            runtime_condition: false,
            secrets: true,
            credentials: false,
        }
    }

    /// Packer builds machine and container images. The protocol has no
    /// category for that yet. Of the four it has, an image is infrastructure
    /// declared as code: it is built from nothing, like a Terraform resource,
    /// and what it produces is an id that the stage creating machines consumes.
    fn category(&self) -> Category {
        Category::InfrastructureAsCode
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A secret in a param is written as a reference to a sensitive input
        // variable, which the runner gives its value from a private file.
        refuse_secret_in(
            graph,
            &["variable", "plugin"],
            "its declaration is written into the template as it stands",
        )?;
        let graph = &graph.secrets_written_as(secret_reference);
        let mut template = transform::transform(graph)?;
        secret_variables(graph, &mut template);
        order::apply_ordering(graph, &mut template)?;
        validate::validate(&template)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&template),
            },
            warnings: Vec::new(),
        })
    }
}
