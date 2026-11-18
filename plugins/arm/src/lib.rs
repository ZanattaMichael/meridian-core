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

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{
    holds_secret, Adoption, Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter,
    PlacedEscape, Position, Resource, ResourceGraph, Value,
};

pub use emit::{DEPLOYMENT_PATH, TEMPLATE_PATH};
pub use runner::{ArmRunner, ARGS_VARIABLE, PARAMETERS_VARIABLE, PROGRAM_VARIABLE};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "arm";

/// The template parameter the secret `name` is handed over as:
/// `meridian_secret_<name>`.
pub fn secret_parameter(name: &str) -> String {
    format!("meridian_secret_{name}")
}

/// How a secret is written in a param: a reference to its parameter, which
/// the transform writes as `[parameters('meridian_secret_<name>')]`.
pub fn secret_reference(name: &str) -> String {
    format!("${{{}}}", secret_parameter(name))
}

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
    /// The document's parameters, then one `securestring` parameter per
    /// secret.
    pub parameters: Vec<Parameter>,
    pub entries: Vec<Entry>,
    /// Output name and its value as the template writes it.
    pub outputs: Vec<(String, String, Position)>,
}

/// The refusal rules only this target has. Everything else it refuses carries
/// a general rule from `meridian_sdk::rules`.
pub(crate) mod own {
    use meridian_sdk::rules::Rule;

    pub(crate) const INVALID_EXPRESSION: Rule = Rule::new(
        "invalid-expression",
        "a value written as an ARM template expression, `[...]`, that Resource Manager could not parse",
        "write a well-formed expression, or escape a literal bracket as `[[`",
    );

    pub(crate) const TEMPLATE_TOO_LARGE: Rule = Rule::new(
        "template-too-large",
        "the compiled template is larger than the 4 MB Resource Manager accepts",
        "split the document into several deployments",
    );

    /// Every rule here, for `Emitter::rules` and the reference page.
    pub(crate) const ALL: &[Rule] = &[INVALID_EXPRESSION, TEMPLATE_TOO_LARGE];
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &Rule, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

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

/// The ARM plugin's emitter.
pub struct Arm;

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

impl Emitter for Arm {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        [
            "<Namespace>/<type>",
            "address",
            "deployment",
            "firewall",
            "network",
            "parameter",
            "subnet",
            "vm",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Resource Manager cannot act on one resource because another changed,
    /// and a template's `condition` is decided before anything is deployed,
    /// not from the state of what it deploys to.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // A secret reaches Resource Manager as a securestring parameter
            // whose value is read from a private file.
            secrets: true,
            adopt: Adoption::Converge,
            ..Capabilities::default()
        }
    }
    fn rules(&self) -> Vec<Rule> {
        own::ALL.to_vec()
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

    /// The common infrastructure types, described for editors. Azure's own
    /// types come from `meridian schema sync`.
    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        meridian_target_bicep::common_schemas()
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // The common types become Azure resources first, written the way a
        // template writes them, so every later stage sees Azure types only.
        let graph =
            &meridian_target_bicep::lower_common(graph, meridian_target_bicep::Dialect::Arm, NAME)?;
        // A secret in a param is written as a reference to a securestring
        // parameter, which the runner gives its value from a private file.
        refuse_secret_in(graph, &["deployment", "parameter"], "the deployment's settings are the command line `az` is given and a parameter's declaration is written into the template")?;
        let graph = &graph.secrets_written_as(secret_reference);
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
