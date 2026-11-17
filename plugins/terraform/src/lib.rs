//! Compiles a Meridian infrastructure document into a Terraform configuration
//! and runs it with `terraform`.
//!
//! # What a document looks like to this target
//!
//! Terraform's vocabulary is its providers', and there are thousands of
//! resource types. This target does not hide them behind a smaller one: a
//! resource's `type` is the Terraform type and its `params` are that type's
//! arguments, exactly as the provider documents them.
//!
//! | `type` | Becomes |
//! |---|---|
//! | `aws_instance`, `azurerm_resource_group`, any `<provider>_<type>` | a `resource` block named by the resource's id |
//! | `data.<type>` | a `data` block |
//! | `module` | a `module` block |
//! | `variable` | a `variable` block; a stage's inputs arrive through these |
//! | `local` | one entry of `locals`, from the param `value` |
//! | `provider` | a `provider` block; `source` and `version` go to `required_providers` |
//! | `backend` | the `backend` block, whose type is the resource's id |
//!
//! What Meridian adds is the wiring. `${web_vm.public_ip}` in a param or an
//! output refers to another resource of the document by its id, and is
//! rewritten to Terraform's own address for it. `dependsOn` becomes
//! `depends_on` unless such a reference already implies the order. A document's
//! outputs become `output` blocks, which is how a later stage learns the
//! address of the machine this one creates.
//!
//! # Why the output is JSON
//!
//! The configuration is written in Terraform's JSON syntax (`main.tf.json`),
//! not HCL. In HCL a nested block and a map-valued argument are written
//! differently, and only the provider's schema says which one a param is. In
//! the JSON syntax Terraform makes that decision itself, so a document needs
//! no schema to compile correctly.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
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
pub use import::TerraformImporter;
pub use runner::TerraformRunner;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "terraform";

/// What kind of block a resource became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Provider,
    Backend,
    Variable,
    Local,
    Data,
    Module,
    Managed,
}

impl Kind {
    /// What a person calls a block of this kind, as it reads after "is".
    pub fn described(self) -> &'static str {
        match self {
            Kind::Provider => "a provider block",
            Kind::Backend => "a backend block",
            Kind::Variable => "a variable",
            Kind::Local => "a local value",
            Kind::Data => "a data source",
            Kind::Module => "a module",
            Kind::Managed => "a resource",
        }
    }

    /// Whether `depends_on` may name a block of this kind, or be written on
    /// one. Settings, variables and local values have no place in an order:
    /// Terraform works them out before, or as part of, everything else.
    pub fn orderable(self) -> bool {
        matches!(self, Kind::Managed | Kind::Data | Kind::Module)
    }
}

/// One block of the configuration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Block {
    pub kind: Kind,
    /// The Terraform type: `aws_instance` for a resource or a data source,
    /// the provider's local name for a provider, the backend type for a
    /// backend. Empty for a module.
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
        match self.kind {
            Kind::Managed => Some(format!("{}.{}", self.r#type, self.name)),
            Kind::Data => Some(format!("data.{}.{}", self.r#type, self.name)),
            Kind::Module => Some(format!("module.{}", self.name)),
            Kind::Variable => Some(format!("var.{}", self.name)),
            Kind::Local => Some(format!("local.{}", self.name)),
            Kind::Provider | Kind::Backend => None,
        }
    }
}

/// A whole configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Config {
    pub blocks: Vec<Block>,
    /// `required_providers`, by local name.
    pub required_providers: BTreeMap<String, Value>,
    /// Output name to its value expression.
    pub outputs: Vec<(String, Value, Position)>,
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

/// The Terraform emitter.
pub struct Terraform;

impl Emitter for Terraform {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        [
            "<provider>_<type>",
            "backend",
            "data.<type>",
            "local",
            "module",
            "provider",
            "variable",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Terraform has no handler to run when something changes. The nearest it
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
        let warnings = sort::apply_ordering(graph, &mut config)?;
        validate::validate(&config)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&config),
            },
            warnings,
        })
    }
}
