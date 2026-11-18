//! Compiles a Meridian document of Kubernetes objects into a Helm chart, with
//! an optional Kustomize overlay, and installs it as a release with `helm`.
//!
//! # What a document looks like to this target
//!
//! A resource's `type` is the object's `apiVersion` and `kind` joined by a
//! slash (`apps/v1/Deployment`, `v1/Service`) and its `params` are the rest of
//! the manifest, exactly as Kubernetes documents it. Each one becomes a
//! template of the chart. An object with no `metadata.name` is named after
//! its id.
//!
//! Four more types hold settings and are not objects:
//!
//! - `values` holds the chart's default values, written to `values.yaml`.
//! - `chart` names and versions the chart (`name`, `version`, `appVersion`,
//!   `description`).
//! - `release` says how it is installed (`name`, `namespace`,
//!   `createNamespace`, `wait`, `atomic`, `timeout`).
//! - `kustomization` is a Kustomize overlay applied to what Helm renders,
//!   before it is installed. Its params are the fields of a
//!   `kustomization.yaml`.
//!
//! What Meridian adds is the wiring. `${values.image.tag}` reads a value and
//! becomes `{{ .Values.image.tag }}`; `${release.name}` becomes
//! `{{ .Release.Name }}`; `${web_config}` is the name of another object of the
//! document. `notifies` with the action `restart` rolls a workload's pods
//! when the object it watches changes. An output may read anything from an
//! object once it is applied, such as
//! `${web_svc.status.loadBalancer.ingress[0].ip}`.
//!
//! Text the author wrote is never read as a template: a literal `{{` reaches
//! the cluster as `{{`.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod manifest;
mod order;
mod path;
mod runner;
mod template;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::{
    holds_secret, Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position,
    Resource, ResourceGraph, SecretName, Value,
};
use meridian_yaml::Yaml;

/// What a secret in the chart's values is written as in `values.yaml`: a
/// token inside a JSON string. The runner replaces it with the value in
/// protected memory and hands the whole values file to Helm on standard
/// input (`--values -`), so the file on disk never holds the value and the
/// templates read it as any value, `${values.<path>}`.
pub fn secret_token(name: &str) -> String {
    format!("${{meridian_secret_{name}}}")
}

/// A secret may be a value, and nothing else: a setting is the command line
/// `helm` is given, an object is written into a template file on disk, and
/// a Kustomize overlay has the rendered manifests written to disk too.
fn refuse_secrets_outside_values(graph: &ResourceGraph) -> Result<(), CompileError> {
    let kustomized = graph.resources.iter().any(|r| r.r#type == "kustomization");
    for r in &graph.resources {
        let Some((key, _)) = r.params.iter().find(|(_, v)| holds_secret(v)) else {
            continue;
        };
        let msg = match r.r#type.as_str() {
            "values" if kustomized => format!(
                "value {key:?} holds a secret, and this document has a kustomization, for which \
                 the rendered manifests are written to a file in the work directory, where a \
                 secret cannot go; a document with a secret in its values has no kustomization"
            ),
            "values" => continue,
            "chart" | "release" | "kustomization" => format!(
                "param {key:?} of the {} holds a secret, and the settings are the command line \
                 helm is given, where a secret cannot go. A secret goes in the values, which the \
                 templates read with ${{values.<path>}} and Helm is handed on standard input",
                r.r#type
            ),
            _ => format!(
                "param {key:?} of {:?} holds a secret, and an object is written into a template \
                 file as it stands, where a secret cannot go. Put the secret in the values and \
                 read it here with ${{values.<path>}}, in the stringData of a v1/Secret",
                r.r#type
            ),
        };
        return Err(refuse(r, "unsupported-secret-place", msg));
    }
    Ok(())
}

/// Text the document writes that would read as a secret's token is refused,
/// so the runner cannot be made to fill a value in where the document put
/// the token itself.
fn refuse_token_text(graph: &ResourceGraph) -> Result<(), CompileError> {
    fn has_token(value: &Value) -> bool {
        match value {
            Value::String(s) => s.contains("${meridian_secret_"),
            Value::List(items) => items.iter().any(has_token),
            Value::Map(map) => {
                map.keys().any(|k| k.contains("${meridian_secret_")) || map.values().any(has_token)
            }
            _ => false,
        }
    }
    for r in graph.resources.iter().filter(|r| r.r#type == "values") {
        if let Some((key, _)) = r.params.iter().find(|(_, v)| has_token(v)) {
            return Err(refuse(
                r,
                "reserved-text",
                format!(
                    "value {key:?} holds the text \"${{meridian_secret_\", which is how this \
                     target writes a secret's place in the values; write the secret as \
                     ${{{{ secret.<name> }}}} or choose other text"
                ),
            ));
        }
    }
    Ok(())
}

/// The chart's values as `values.yaml`, each secret as its token.
pub(crate) fn values_yaml(value: &Value, secrets: &[SecretName]) -> Yaml {
    match value {
        Value::Secret(slot) => match secrets.iter().find(|s| s.secret == *slot) {
            Some(named) => Yaml::Raw(format!("\"{}\"", secret_token(&named.name))),
            None => Yaml::str(meridian_sdk::SENSITIVE),
        },
        Value::List(items) => Yaml::Seq(items.iter().map(|v| values_yaml(v, secrets)).collect()),
        Value::Map(map) => Yaml::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), values_yaml(v, secrets)))
                .collect(),
        ),
        other => plain(other),
    }
}

pub use emit::{CHART_DIR, KUSTOMIZE_DIR, RELEASE_PATH};
pub use runner::{
    HelmRunner, HELM_ARGS_VARIABLE, HELM_VARIABLE, KUBECTL_ARGS_VARIABLE, KUBECTL_VARIABLE,
    KUSTOMIZE_VARIABLE,
};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "helm";

/// A node of a template: YAML whose string scalars may be template actions.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    /// A scalar that is not a string.
    Plain(Yaml),
    /// Text the author wrote, to reach the cluster unchanged.
    Text(String),
    /// A template action that writes the whole value.
    Action(String),
    Seq(Vec<Node>),
    Map(Vec<(String, Node)>),
}

/// One Kubernetes object of the chart.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Manifest {
    /// The resource's id in the document, and the name of its template.
    pub id: String,
    pub api_version: String,
    pub kind: String,
    /// The object's name with the document's own values.
    pub name: String,
    /// Its namespace, when the manifest sets one itself.
    pub namespace: String,
    /// Everything below `apiVersion` and `kind`.
    pub body: Vec<(String, Node)>,
    pub pos: Position,
}

/// The chart's own metadata, from the document's `chart` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChartSettings {
    pub name: String,
    pub version: String,
    pub app_version: String,
    pub description: String,
}

/// How the chart is installed, from the document's `release` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ReleaseSettings {
    pub name: String,
    pub namespace: String,
    pub create_namespace: bool,
    pub wait: bool,
    pub atomic: bool,
    pub timeout: String,
}

/// The overlay Kustomize applies to what Helm renders.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Kustomization {
    /// The fields of `kustomization.yaml`, apart from the ones Meridian writes.
    pub fields: Vec<(String, Yaml)>,
    /// The three transformations that move an object, kept so that what the
    /// tools report can still be traced to the resource that declared it.
    pub name_prefix: String,
    pub name_suffix: String,
    pub namespace: String,
}

/// One piece of an output's value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutputPart {
    /// Known when the document is compiled.
    Text(String),
    /// Read from the object in the cluster after an apply.
    Lookup {
        resource: String,
        path: Vec<path::Step>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutputSpec {
    pub name: String,
    pub parts: Vec<OutputPart>,
    /// The value is read from a Secret.
    pub sensitive: bool,
}

/// A whole chart and how to install it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Chart {
    pub document: String,
    pub chart: ChartSettings,
    pub release: ReleaseSettings,
    pub values: BTreeMap<String, Value>,
    pub manifests: Vec<Manifest>,
    pub kustomization: Option<Kustomization>,
    pub outputs: Vec<OutputSpec>,
    /// The name of each secret a value stands for.
    pub secrets: Vec<SecretName>,
}

impl Chart {
    pub(crate) fn manifest(&self, id: &str) -> Option<&Manifest> {
        self.manifests.iter().find(|m| m.id == id)
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

/// The Helm emitter.
pub struct Helm;

impl Emitter for Helm {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        [
            "<apiVersion>/<Kind>",
            "chart",
            "kustomization",
            "release",
            "values",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Helm has no handler that runs when something else changes. A restart
    /// is emulated the way chart authors do it by hand: a checksum of the
    /// watched object on the workload's pod template, which rolls the pods
    /// when it changes. Nothing in a chart is decided at the instant of
    /// apply from the state of the cluster.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
            secrets: true,
            credentials: false,
        }
    }

    fn category(&self) -> Category {
        Category::ContainerOrchestration
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        refuse_secrets_outside_values(graph)?;
        refuse_token_text(graph)?;
        let mut chart = transform::transform(graph)?;
        chart.secrets = graph.secrets.clone();
        let warnings = order::apply_edges(graph, &mut chart)?;
        validate::validate(&chart)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&chart),
            },
            warnings,
        })
    }
}
