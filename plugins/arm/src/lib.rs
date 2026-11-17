//! Compiles a Meridian infrastructure document into an Azure Resource Manager
//! (ARM) JSON template and deploys it with the `az` command line.
//!
//! # What a document looks like to this target
//!
//! A resource's `type` is the ARM resource type
//! (`Microsoft.Network/virtualNetworks`) and its `params` are the resource as
//! ARM writes it: `apiVersion`, `name`, `location`, `properties`, `sku`,
//! `tags` and the rest, exactly as Azure documents them. `name` may be left
//! out, and is then the resource's id.
//!
//! Two types are not resources of the template. One resource may have the
//! type `deployment`: it holds the settings of the deployment itself
//! (`name`, `scope`, `resourceGroup`, `location`, `subscription`, `mode`). A
//! resource of type `parameter` declares a template parameter, named by its
//! id.
//!
//! What Meridian adds is the wiring. `${web_vnet}` is the resource id of
//! another resource of the document; `${web_vnet.name}` is its name;
//! `${web_ip.properties.ipAddress}` reads its state with `reference()`;
//! `${admin_password}` reads a parameter. Any of them inside longer text
//! becomes one `format()`, and inside an ARM expression the author wrote in
//! square brackets it is put in place. Every reference and every `dependsOn`
//! becomes an ARM `dependsOn`, because ARM does not infer one from a resource
//! id. Outputs become the template's `outputs`.
//!
//! A string in square brackets is an ARM expression and is passed through,
//! after a check that it is well formed.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
pub mod expression;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, PlacedEscape, Position,
    Resource, ResourceGraph, Value,
};

pub use emit::{DEPLOYMENT_PATH, TEMPLATE_PATH};
pub use runner::{ArmRunner, ARGS_VARIABLE, PARAMETERS_VARIABLE, PROGRAM_VARIABLE};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "arm";

/// What a deployment is made into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    #[default]
    ResourceGroup,
    Subscription,
}

impl Scope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Scope::ResourceGroup => "resourceGroup",
            Scope::Subscription => "subscription",
        }
    }
}

/// One resource of the template.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// The resource's id in the document.
    pub id: String,
    pub r#type: String,
    pub api_version: String,
    /// The name as the template writes it: literal text, or an expression in
    /// square brackets.
    pub name: String,
    /// The expression, without brackets, that is this resource's id in Azure.
    pub resource_id: String,
    /// Everything else of the resource, with its references rewritten.
    pub body: BTreeMap<String, Value>,
    /// The ids of the other resources this one refers to or must follow.
    pub depends: BTreeSet<String>,
    /// `dependsOn` as the template writes it.
    pub depends_on: Vec<String>,
    pub pos: Position,
}

/// One template parameter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Parameter {
    pub name: String,
    pub definition: BTreeMap<String, Value>,
    pub pos: Position,
}

/// The settings of the deployment, from the document's `deployment` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Settings {
    pub name: String,
    pub scope: Scope,
    pub resource_group: String,
    pub location: String,
    pub subscription: String,
    pub mode: String,
}

/// A whole template and how to deploy it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Template {
    pub document: String,
    pub settings: Settings,
    pub parameters: Vec<Parameter>,
    pub entries: Vec<Entry>,
    /// Output name and its value as the template writes it.
    pub outputs: Vec<(String, String, Position)>,
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

/// The ARM plugin's emitter.
pub struct Arm;

impl Emitter for Arm {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        ["<Namespace>/<type>", "deployment", "parameter"]
            .map(str::to_owned)
            .to_vec()
    }

    /// Resource Manager cannot act on one resource because another changed,
    /// and a template's `condition` is decided before anything is deployed,
    /// not from the state of what it deploys to.
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    /// A template string that starts with `[` and ends with `]` is an
    /// expression Resource Manager evaluates, so a value placed from data
    /// that opens with `[` is handed over opening with `[[`, which this
    /// plugin and Resource Manager both read as a literal `[`. What the
    /// author wrote in square brackets is passed through, as the target's
    /// page says.
    fn placed_text(&self) -> Vec<PlacedEscape> {
        vec![PlacedEscape::at_start("[", "[[")]
    }

    fn category(&self) -> Category {
        Category::InfrastructureAsCode
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut template = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut template)?;
        let warnings = validate::validate(&template)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&template),
            },
            warnings,
        })
    }
}
