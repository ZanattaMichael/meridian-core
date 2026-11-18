//! Compiles a Meridian infrastructure document into an AWS CloudFormation
//! template and deploys it as a stack with the `aws` command line.
//!
//! # What a document looks like to this target
//!
//! A resource's `type` is the CloudFormation resource type
//! (`AWS::EC2::Instance`) and its `params` are that type's properties,
//! exactly as AWS documents them. A few keys are resource attributes and not
//! properties, and are written beside `Properties` instead of inside it:
//! `DeletionPolicy`, `UpdateReplacePolicy`, `CreationPolicy`, `UpdatePolicy`,
//! `Condition` and `Metadata`.
//!
//! One resource may have the type `stack`. It is not part of the template: it
//! holds the settings of the deployment itself (`name`, `region`,
//! `capabilities`).
//!
//! What Meridian adds is the wiring. `${web_vm}` refers to another resource
//! of the document and becomes a `Ref`; `${web_vm.PublicIp}` becomes
//! `Fn::GetAtt`; either inside longer text becomes `Fn::Sub`. `dependsOn`
//! becomes `DependsOn` unless a reference already implies the order. Outputs
//! become the template's `Outputs`.
//!
//! CloudFormation's logical ids hold only letters and digits, so a resource
//! id such as `web_vm` is written `WebVm`. The mapping travels with the
//! artifact, and everything Meridian reports uses the id the author wrote.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{
    holds_secret, Artifact, Capabilities, Category, CloudTakes, CompileError, Data, Emitted,
    Emitter, Position, Resource, ResourceGraph, Value,
};
use meridian_yaml::Yaml;

pub use emit::{STACK_PATH, TEMPLATE_PATH};
pub use runner::CloudFormationRunner;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "cloudformation";

/// The id a secret is referred to by inside a param, before it is resolved:
/// `meridian_secret_<name>`.
pub fn secret_key(name: &str) -> String {
    format!("meridian_secret_{name}")
}

/// The template parameter the secret `name` is handed over as:
/// `MeridianSecretDbPassword` for `db_password`, since a parameter's name is
/// letters and digits only. `None` when the name has neither.
pub fn secret_parameter(name: &str) -> Option<String> {
    logical_id(&secret_key(name))
}

/// How a secret is written in a param: a reference the transform resolves to
/// `Ref` of its parameter, or `${X}` inside an `Fn::Sub`.
pub fn secret_reference(name: &str) -> String {
    format!("${{{}}}", secret_key(name))
}

/// One resource of the template.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// The resource's id in the document.
    pub id: String,
    /// Its logical id in the template.
    pub logical: String,
    pub r#type: String,
    pub properties: Vec<(String, Yaml)>,
    /// Resource attributes, written beside `Properties`.
    pub attributes: Vec<(String, Yaml)>,
    pub depends_on: Vec<String>,
    /// The ids of the other resources the properties refer to.
    pub refs: BTreeSet<String>,
    pub pos: Position,
}

/// The settings of the deployment, from the document's `stack` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Settings {
    pub name: String,
    pub region: String,
    pub capabilities: Vec<String>,
}

/// A whole template and how to deploy it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Template {
    pub document: String,
    pub settings: Settings,
    /// The parameters the secrets are handed over as, by logical id.
    pub parameters: Vec<String>,
    pub entries: Vec<Entry>,
    /// Output name as written, its logical id, and its value.
    pub outputs: Vec<(String, String, Yaml, Position)>,
}

/// A refusal of one resource, stamped with this target.
/// How `aws cloudformation` takes a cloud credential: the AWS CLI reads its
/// login from the environment, as every AWS SDK does. CloudFormation is AWS
/// only, so an `azure` or `gcp` credential is refused.
pub(crate) const CLOUD_TAKES: CloudTakes = CloudTakes {
    tool: "aws cloudformation",
    providers: &["aws"],
    azure_certificate: Err("unused"),
    gcp_impersonation: Err("unused"),
};

/// Refuses a credential the tool cannot take, at compile, so that nothing
/// is fetched for it.
fn check_credential(graph: &ResourceGraph) -> Result<(), CompileError> {
    let Some(credential) = &graph.credential else {
        return Ok(());
    };
    meridian_sdk::check_cloud_credential(credential, &CLOUD_TAKES).map_err(|msg| CompileError {
        target: NAME.to_owned(),
        resource: String::new(),
        rule: "unsupported-credential".to_owned(),
        msg: format!("the credential {:?}: {msg}", credential.name),
        pos: Position::default(),
    })
}

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

/// Turns a document id into a CloudFormation logical id: `web_vm` is `WebVm`.
/// Returns `None` when nothing alphanumeric is left to name it by.
pub(crate) fn logical_id(id: &str) -> Option<String> {
    let mut out = String::with_capacity(id.len());
    for word in id.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A param value with no references in it, as YAML.
pub(crate) fn plain(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(i) => Yaml::Int(*i),
        Value::Uint(u) => Yaml::Uint(*u),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::str(s),
        // A plugin is never sent a secret as a value; one would be written as its mask.
        Value::Secret(_) => Yaml::str(meridian_sdk::SENSITIVE),
        Value::List(items) => Yaml::Seq(items.iter().map(plain).collect()),
        Value::Map(map) => Yaml::Map(map.iter().map(|(k, v)| (k.clone(), plain(v))).collect()),
    }
}

/// The names of the two files, kept in one place for the runner.
pub(crate) type Files = BTreeMap<String, String>;

/// The CloudFormation emitter.
pub struct CloudFormation;

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
                "unsupported-secret-place",
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

impl Emitter for CloudFormation {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        ["<Vendor>::<Service>::<Type>", "stack"]
            .map(str::to_owned)
            .to_vec()
    }

    /// CloudFormation cannot act on one resource because another changed,
    /// natively or otherwise, and it has no apply-time conditional that reads
    /// the state of the world. A secret is taken: it reaches CloudFormation
    /// as a `NoEcho` parameter whose value is read from a private file.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            secrets: true,
            cloud_credentials: true,
            ..Capabilities::default()
        }
    }

    fn category(&self) -> Category {
        Category::InfrastructureAsCode
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        check_credential(graph)?;
        // A secret in a param is written as a reference to a NoEcho
        // parameter, which the runner gives its value from a private file.
        refuse_secret_in(
            graph,
            &["stack"],
            "the stack's settings are the command line `aws` is given",
        )?;
        let graph = &graph.secrets_written_as(secret_reference);
        let mut template = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut template)?;
        validate::validate(&template)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&template),
            },
            warnings: Vec::new(),
        })
    }
}
