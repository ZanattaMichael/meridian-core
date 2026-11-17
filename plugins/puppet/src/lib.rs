//! Compiles a Meridian resource tree into a Puppet manifest and the node
//! statement that binds it to a host, and applies it.
//!
//! The library depends on `meridian-sdk` and nothing else of Meridian's. The
//! four compile stages run in the order the design plan mandates: transform,
//! sort, validate, emit.
//!
//! # How this target differs from Ansible
//!
//! Puppet compiles a catalog instead of running a task list, and the
//! difference shows up in three places worth reading before the mapping table.
//!
//! Ordering is declared, not implied. A manifest's file order means nothing,
//! so every dependency edge becomes an explicit `require` metaparameter and
//! the dependency graph survives into the output intact. The Ansible target
//! has to flatten that graph into one sequence and warn that it discarded
//! parallelism; this target has nothing to warn about.
//!
//! Notification is a refresh, not a named handler. Puppet's `notify` tells a
//! resource that something it depends on changed, and the resource decides
//! what to do about it. There is no way to say which of several actions to
//! take, so this target accepts the actions Puppet can perform and refuses the
//! rest instead of quietly upgrading a reload into a restart.
//!
//! There is no apply-time conditional at all. A catalog is compiled before it
//! reaches the node, so `runtimeWhen` has nowhere to go and the capabilities
//! say so. A document that needs one fails here with an explanation instead of
//! compiling to a manifest that silently ignores it.

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

pub use emit::{MANIFEST_PATH, SITE_PATH};
pub use runner::{PuppetRunner, Transport};
pub use transform::supported_resource_types;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "puppet";

/// One attribute value. `Bare` is a value Puppet expects unquoted: an ensure
/// value, or a provider name. Quoting one would still parse, but it is not
/// what a Puppet author would have written.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Attr {
    Bare(&'static str),
    Value(Value),
}

/// A Puppet resource reference, written `Type['title']` with the type
/// capitalised. It is a type and not a preformatted string so the emit stage
/// stays the only place that decides how one is written.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Reference {
    pub r#type: String,
    pub title: String,
}

impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut chars = self.r#type.chars();
        if let Some(first) = chars.next() {
            write!(f, "{}{}", first.to_ascii_uppercase(), chars.as_str())?;
        }
        write!(f, "['{}']", self.title)
    }
}

/// One native Puppet resource declaration: a type, a title, its attributes,
/// and the metaparameters that place it in the catalog's graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Declaration {
    /// The native type, lower-case as it is written in a declaration.
    pub r#type: String,
    /// The title. Puppet requires it to be unique for its type across the
    /// whole catalog, a stricter rule than any other target has.
    pub title: String,
    /// The attributes, keyed by native name. `ensure` lives here too.
    pub attrs: BTreeMap<String, Attr>,
    /// References this resource is applied after.
    pub require: Vec<Reference>,
    /// References this resource refreshes when it changes.
    pub notify: Vec<Reference>,
    /// Provenance, so a validation failure names the resource the author
    /// wrote.
    pub resource_id: String,
    pub pos: Position,
}

/// A whole compiled document: one class holding every resource, and the host
/// the node statement assigns it to.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Manifest {
    pub class: String,
    pub host: String,
    pub resources: Vec<Declaration>,
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// A refusal raised against a declaration.
pub(crate) fn refuse_declaration(d: &Declaration, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: d.resource_id.clone(),
        rule: rule.to_owned(),
        msg,
        pos: d.pos.clone(),
    }
}

/// The Puppet emitter.
pub struct Puppet;

impl Emitter for Puppet {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        supported_resource_types()
    }

    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        transform::resource_schemas()
    }

    /// `native_notify`: the `notify` metaparameter is Puppet's own mechanism.
    /// That it is narrower than Ansible's is a matter for the mapping table.
    ///
    /// `runtime_condition` is false. A catalog is compiled and then shipped
    /// to the node, so by the time resources are applied there is no
    /// expression left to evaluate.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: false,
        }
    }

    fn category(&self) -> Category {
        Category::ConfigurationManagement
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A resource here has nothing to read at apply time, so a reference
        // is settled now, from what the document declares, or refused.
        let graph = &meridian_sdk::expr::resolve_declared(graph, NAME)?;
        let mut manifest = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut manifest)?;
        validate::validate(&manifest)?;
        let files = emit::serialise(&manifest)?;
        Ok(Emitted {
            artifact: Artifact { files },
            warnings: Vec::new(),
        })
    }
}
