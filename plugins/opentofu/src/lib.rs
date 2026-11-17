//! Compiles a Meridian infrastructure document into an OpenTofu configuration
//! and runs it with `tofu`.
//!
//! # What a document looks like to this target
//!
//! OpenTofu's vocabulary is its providers', and there are thousands of
//! resource types. This target does not hide them behind a smaller one: a
//! resource's `type` is the OpenTofu type and its `params` are that type's
//! arguments, exactly as the provider documents them.
//!
//! | `type` | Becomes |
//! |---|---|
//! | `aws_instance`, `terraform_data`, any `<provider>_<type>` | a `resource` block named by the resource's id |
//! | `data.<type>` | a `data` block |
//! | `ephemeral.<type>` | an `ephemeral` block, whose values are never stored in state or plan |
//! | `module` | a `module` block |
//! | `variable` | a `variable` block; a stage's inputs arrive through these |
//! | `local` | one entry of `locals`, from the param `value` |
//! | `provider` | a `provider` block; `source` and `version` go to `required_providers` |
//! | `backend` | the `backend` block, whose type is the resource's id |
//! | `encryption` | the `encryption` block: state and plan encryption |
//!
//! What Meridian adds is the wiring. `${web_vm.public_ip}` in a param or an
//! output refers to another resource of the document by its id, and is
//! rewritten to OpenTofu's own address for it. `dependsOn` becomes
//! `depends_on` unless such a reference already implies the order. A document's
//! outputs become `output` blocks, which is how a later stage learns the
//! address of the machine this one creates.
//!
//! # How this differs from the Terraform target
//!
//! The two tools read the same language, and a document without OpenTofu's own
//! features compiles to the same blocks for either. This target exists for
//! what OpenTofu has and Terraform does not, and so that neither tool ever
//! picks up the other's file:
//!
//! - the configuration is written to `main.tofu.json`, an extension only
//!   OpenTofu reads;
//! - `encryption` compiles to OpenTofu's state and plan encryption, and its
//!   key providers and methods are checked against each other at compile time;
//! - `ephemeral.<type>` resources are understood as blocks of their own;
//! - the program run is `tofu`.
//!
//! # Why the output is JSON
//!
//! The configuration is written in the JSON syntax, not HCL. In HCL a nested
//! block and a map-valued argument are written differently, and only the
//! provider's schema says which one a param is. In the JSON syntax OpenTofu
//! makes that decision itself, so a document needs no schema to compile
//! correctly.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod encryption;
mod expression;
mod import;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};

pub use emit::CONFIG_PATH;
pub use import::OpenTofuImporter;
pub use runner::{OpenTofuRunner, PROGRAM_VARIABLE};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "opentofu";

/// What kind of block a resource became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Provider,
    Backend,
    Encryption,
    Variable,
    Local,
    Data,
    Ephemeral,
    Module,
    Managed,
}

impl Kind {
    /// What a person calls a block of this kind, as it reads after "is".
    pub fn described(self) -> &'static str {
        match self {
            Kind::Provider => "a provider block",
            Kind::Backend => "a backend block",
            Kind::Encryption => "the encryption block",
            Kind::Variable => "a variable",
            Kind::Local => "a local value",
            Kind::Data => "a data source",
            Kind::Ephemeral => "an ephemeral resource",
            Kind::Module => "a module",
            Kind::Managed => "a resource",
        }
    }

    /// Whether `depends_on` may name a block of this kind, or be written on
    /// one. Settings, variables and local values have no place in an order:
    /// OpenTofu works them out before, or as part of, everything else.
    pub fn orderable(self) -> bool {
        matches!(
            self,
            Kind::Managed | Kind::Data | Kind::Ephemeral | Kind::Module
        )
    }
}

/// One block of the configuration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Block {
    pub kind: Kind,
    /// The OpenTofu type: `aws_instance` for a resource, a data source or an
    /// ephemeral resource, the provider's local name for a provider, the
    /// backend type for a backend. Empty for the rest.
    pub r#type: String,
    /// The block's name, which is the resource's id.
    pub name: String,
    pub body: BTreeMap<String, Value>,
    /// The ids of the other resources this block's arguments refer to.
    pub refs: BTreeSet<String>,
    pub pos: Position,
}

impl Block {
    /// How the rest of the configuration refers to this block, when it can.
    pub fn address(&self) -> Option<String> {
        address(self.kind, &self.r#type, &self.name)
    }
}

/// OpenTofu's address for a block of a kind, when it has one.
pub(crate) fn address(kind: Kind, tofu_type: &str, name: &str) -> Option<String> {
    match kind {
        Kind::Managed => Some(format!("{tofu_type}.{name}")),
        Kind::Data => Some(format!("data.{tofu_type}.{name}")),
        Kind::Ephemeral => Some(format!("ephemeral.{tofu_type}.{name}")),
        Kind::Module => Some(format!("module.{name}")),
        Kind::Variable => Some(format!("var.{name}")),
        Kind::Local => Some(format!("local.{name}")),
        Kind::Provider | Kind::Backend | Kind::Encryption => None,
    }
}

/// One declared output.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutputBlock {
    pub name: String,
    pub value: Value,
    /// Set when the value reads something the document marks sensitive, since
    /// OpenTofu refuses to plan an output that could show a secret unless the
    /// output says it knows.
    pub sensitive: bool,
    pub pos: Position,
}

/// A whole configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Config {
    pub blocks: Vec<Block>,
    /// `required_providers`, by local name.
    pub required_providers: BTreeMap<String, Value>,
    pub outputs: Vec<OutputBlock>,
}

impl Config {
    pub fn block(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.name == id)
    }
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// A refusal that is about the document and not one resource.
pub(crate) fn refuse_at(resource: &str, pos: &Position, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        rule: rule.to_owned(),
        msg,
        pos: pos.clone(),
    }
}

/// The OpenTofu emitter.
pub struct OpenTofu;

impl Emitter for OpenTofu {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        [
            "<provider>_<type>",
            "backend",
            "data.<type>",
            "encryption",
            "ephemeral.<type>",
            "local",
            "module",
            "provider",
            "variable",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// OpenTofu has no handler to run when something changes. The nearest it
    /// has is `replace_triggered_by`, which destroys and recreates the
    /// notified resource. That is a synthetic notify with a real cost, so it
    /// is declared as one and every use of it is warned about.
    ///
    /// There is no apply-time conditional: a plan is computed before anything
    /// is touched.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
        }
    }

    fn category(&self) -> Category {
        Category::InfrastructureAsCode
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut config = transform::transform(graph)?;
        let mut warnings = sort::apply_ordering(graph, &mut config)?;
        validate::validate(&config)?;
        warnings.extend(encryption::check(&config)?);
        encryption::normalise(&mut config);
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&config),
            },
            warnings,
        })
    }
}
