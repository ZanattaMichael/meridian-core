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
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};
use meridian_yaml::Yaml;

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
            secrets: false,
            credentials: false,
        }
    }

    fn category(&self) -> Category {
        Category::ContainerOrchestration
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut chart = transform::transform(graph)?;
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
