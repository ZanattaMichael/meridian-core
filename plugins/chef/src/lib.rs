//! Compiles a Meridian resource tree into a Chef cookbook and converges it.
//!
//! The library depends on `meridian-sdk` and nothing else of Meridian's. The
//! four compile stages run in the order the design plan mandates: transform,
//! sort, validate, emit.
//!
//! # How this target differs from the others
//!
//! A recipe is Ruby that Chef runs from top to bottom, so the order of the
//! file is the order of the run. The resources are written in the tree's
//! apply order and no dependency has to be declared, as in Ansible.
//!
//! Notification is Chef's own `notifies`, and it names the action to take, so
//! a reload stays a reload. It is delayed: the notified resource acts once,
//! at the end of the run, however many resources notified it.
//!
//! `runtimeWhen` is a guard. Chef evaluates `only_if` on the node at the
//! moment it reaches the resource, so the condition is written as the Ruby
//! the guard holds, for example `node['platform_family'] == 'debian'`.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};

pub use emit::{metadata_path, recipe_path};
pub use runner::{ChefRunner, Transport};
pub use transform::supported_resource_types;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "chef";

/// One `notifies` line: the action, and the resource that takes it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Notify {
    /// The notified resource as Chef writes it: `service[nginx_svc]`.
    pub target: String,
    pub action: String,
}

/// One Chef resource block.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Block {
    /// The Chef resource: `package`, `execute`, `directory`.
    pub resource: &'static str,
    /// The block's name, unique for its resource across the recipe.
    pub name: String,
    /// The properties, keyed by Chef's own name for each.
    pub properties: BTreeMap<String, Value>,
    /// The actions, in the order Chef takes them.
    pub actions: Vec<&'static str>,
    /// The Ruby an `only_if` guard holds, or empty.
    pub only_if: String,
    pub notifies: Vec<Notify>,
    /// Provenance, so a failure names the resource the author wrote.
    pub resource_id: String,
    pub pos: Position,
}

/// A whole compiled document: one cookbook whose default recipe holds every
/// resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Cookbook {
    pub name: String,
    /// The document's name, for the cookbook's description.
    pub document: String,
    pub blocks: Vec<Block>,
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

pub(crate) fn refuse_at(resource: &str, pos: &Position, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        rule: rule.to_owned(),
        msg,
        pos: pos.clone(),
    }
}

/// The Chef emitter.
pub struct Chef;

impl Emitter for Chef {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        supported_resource_types()
    }

    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        transform::resource_schemas()
    }

    /// Chef has both: `notifies` is its own mechanism, and a guard is
    /// evaluated on the node during the run.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: true,
        }
    }

    fn category(&self) -> Category {
        Category::ConfigurationManagement
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A resource here has nothing to read at apply time, so a reference
        // is settled now, from what the document declares, or refused.
        let graph = &meridian_sdk::expr::resolve_declared(graph, NAME)?;
        let mut cookbook = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut cookbook)?;
        validate::validate(&cookbook)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&cookbook),
            },
            warnings: Vec::new(),
        })
    }
}
