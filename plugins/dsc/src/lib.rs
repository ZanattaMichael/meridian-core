//! Compiles a Meridian document into a DscPipelineRunner configuration and
//! runs it with `Invoke-DscRunner`.
//!
//! DscPipelineRunner drives both generations of DSC through one
//! configuration format: DSC v2 through `Invoke-DscResource` and DSC v3
//! through the `dsc` command line. So one plugin covers both, and which
//! engine evaluates the resources is a setting, not a different target.
//!
//! # What a document looks like to this target
//!
//! A resource's `type` is the DSC resource as the runner names it,
//! `Module/Resource` (`PSDscResources/Service`, `Microsoft.Windows/Registry`),
//! and its `params` are the resource's properties, exactly as the resource
//! documents them. Desired state is one of those properties (`Ensure`,
//! `_exist`), so a resource takes no `state`.
//!
//! One resource may have the type `runner`. It is not a DSC resource: it
//! chooses the engine and says how the runner reaches the document's host
//! (`engine`, `target`, `credential`).
//!
//! A document's `targetVersion` chooses the engine too: `"2"` is `DscV2` and
//! `"3"` is `DscV3`. A `runner` that names a different engine is refused.
//!
//! # References
//!
//! `${id.Property}` reads a property of another resource as DSC reports it,
//! once that resource has run. It becomes the runner's own `reference()`
//! accessor, and the resource that reads is ordered after the one it reads.
//!
//! # Ordering, notification and conditions
//!
//! `dependsOn` is the runner's own `dependsOn`. `notifies` with the action
//! `refresh` is its own `notify`: when the notifier really changed, the
//! notified resource's `Set` runs even if its `Test` says there is nothing to
//! do. `runtimeWhen` is a `preCondition`, a PowerShell predicate the runner
//! evaluates just before the resource.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod import;
mod powershell;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};

pub use emit::{node_path, DATUM_PATH};
pub use import::DscImporter;
pub use runner::DscRunner;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "dsc";

/// One DSC resource instance.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Instance {
    /// The resource's id in the document, and its name in the configuration.
    pub id: String,
    /// `Module/Resource`.
    pub r#type: String,
    pub properties: BTreeMap<String, Value>,
    /// The PowerShell predicate of a `preCondition`, or empty.
    pub pre_condition: String,
    /// The ids of the resources this one reads with `reference()`.
    pub reads: Vec<String>,
    /// The ids this one runs after.
    pub depends_on: Vec<String>,
    /// The ids whose `Set` this one forces when it changes.
    pub notify: Vec<String>,
    pub pos: Position,
}

/// How the configuration is run, from the document's `runner` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Settings {
    /// `DscV2`, `DscV3` or `Auto`.
    pub engine: String,
    /// `Local`, `WinRM` or `SSH`.
    pub target: String,
    /// The name of a credential the runner resolves itself, or empty.
    pub credential: String,
}

/// A whole compiled document: one node of a Datum project.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Configuration {
    /// The document's name, which names the node.
    pub node: String,
    pub host: String,
    pub settings: Settings,
    pub instances: Vec<Instance>,
}

impl Configuration {
    /// A resource as `dependsOn` and `notify` name it: `Module/Resource/name`.
    pub fn identity(&self, id: &str) -> String {
        let r#type = self
            .instances
            .iter()
            .find(|i| i.id == id)
            .map_or("", |i| i.r#type.as_str());
        format!("{type}/{id}")
    }
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

/// The DSC emitter.
pub struct Dsc;

impl Emitter for Dsc {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        ["<Module>/<Resource>", "runner"]
            .map(str::to_owned)
            .to_vec()
    }

    /// The two generations of DSC the runner drives. A document that asks
    /// for one with `targetVersion` gets that engine.
    fn target_versions(&self) -> Vec<String> {
        vec!["2".to_owned(), "3".to_owned()]
    }

    /// Both are the runner's own: `notify` forces a `Set`, and a
    /// `preCondition` is evaluated at the moment of apply.
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
        let mut configuration = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut configuration)?;
        validate::validate(&configuration)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&configuration),
            },
            warnings: Vec::new(),
        })
    }
}
