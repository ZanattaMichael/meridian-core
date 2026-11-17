//! Compiles a Meridian resource tree into an Ansible playbook and inventory,
//! and runs it with `ansible-playbook`.
//!
//! The library depends on `meridian-sdk` and on the two crates plugins share
//! for writing YAML and importing, and on nothing of the compiler's. If this
//! target can be written against the public SDK, so can one Meridian's
//! authors never anticipated. The binary that serves it over the
//! plugin protocol is `main.rs`; the library is ordinary code, so the emitter
//! can be tested without a process boundary.
//!
//! The four compile stages run in the order the design plan mandates:
//! transform, sort, validate, emit. Each is a separate module, and emit does
//! no mapping or ordering work of its own.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod import;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};

pub use emit::{INVENTORY_PATH, PLAYBOOK_PATH};
pub use import::AnsibleImporter;
pub use runner::AnsibleRunner;
pub use transform::supported_resource_types;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "ansible";

/// One native Ansible task or handler: the module to run, its arguments, and
/// the structural keys around it. It is the native tree the transform stage
/// produces and the emit stage serialises.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Task {
    /// The task's `name:` key, which is also what handlers are notified by,
    /// so it must be unique within the play.
    pub name: String,
    /// The fully-qualified module name, for example `ansible.builtin.package`.
    pub module: String,
    /// The module's arguments, keyed by native argument name.
    pub args: BTreeMap<String, Value>,
    /// The handler names this task triggers on change.
    pub notify: Vec<String>,
    /// A target-native runtime conditional. It is only ever populated from a
    /// resource's `runtimeWhen`: a compile-time `when` is pruned long before
    /// this stage and must never reach the output.
    pub when: String,
    /// Provenance, kept so a validation failure can name the resource the
    /// author wrote and not the task it became.
    pub resource_id: String,
    pub pos: Position,
}

/// A whole Ansible play.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Playbook {
    pub name: String,
    pub hosts: String,
    pub tasks: Vec<Task>,
    pub handlers: Vec<Task>,
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// The Ansible emitter.
pub struct Ansible;

impl Emitter for Ansible {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        supported_resource_types()
    }

    /// `native_notify`: notify and handlers are Ansible's own mechanism, so
    /// nothing is synthesised.
    ///
    /// `runtime_condition`: a task's `when:` is evaluated on the node at apply
    /// time, which is what `runtimeWhen` needs. It is deliberately not used
    /// for ordinary `when`, which Meridian evaluates and prunes long before
    /// this stage.
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

    /// Runs the four compile stages in order.
    ///
    /// The resolved hierarchy data is not consumed: Meridian bakes resolved
    /// values into the tree as literals before a plugin sees it.
    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A resource here has nothing to read at apply time, so a reference
        // is settled now, from what the document declares, or refused.
        let graph = &meridian_sdk::expr::resolve_declared(graph, NAME)?;
        let playbook = transform::transform(graph)?;
        let warnings = sort::check_order(graph)?;
        validate::validate(&playbook)?;
        // The tasks arrive in the tree's apply order, which the sort stage
        // just confirmed is a valid linear sequence. Leaving them as they are
        // keeps one ordering decision in the codebase instead of two.
        let files = emit::serialise(&playbook);
        Ok(Emitted {
            artifact: Artifact { files },
            warnings,
        })
    }
}
