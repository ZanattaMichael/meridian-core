//! Compiles a Meridian document into Kubernetes manifests and applies them
//! with `kubectl`.
//!
//! # What a document looks like to this target
//!
//! A resource's `type` is the object's API version and kind
//! (`apps/v1/Deployment`, `v1/Service`) and its `params` are the object's
//! fields, exactly as Kubernetes documents them: `metadata`, `spec`, `data`.
//! An object with no `metadata.name` is named after the resource's id.
//!
//! One resource may have the type `cluster`. It is not a manifest: it says
//! which cluster and namespace the manifests go to (`context`, `namespace`,
//! `kubeconfig`).
//!
//! # References
//!
//! Kubernetes does not wire one object's state into another's spec, so inside
//! `params` a reference can only be something known at compile time:
//! `${web}` or `${web.metadata.name}` is the object's name, and
//! `${web.metadata.namespace}` its namespace.
//!
//! An output is different: it is read from the cluster after the apply, so
//! `${web_svc.status.loadBalancer.ingress[0].ip}` is any path into the live
//! object. That is how a later stage learns the address this one exposes.
//!
//! # Ordering and notification
//!
//! `kubectl apply` sends manifests in file order and does not wait for one to
//! be ready before the next. `dependsOn` therefore orders the file, and a
//! warning says that is all it does.
//!
//! `notifies` with the action `restart`, aimed at a Deployment, StatefulSet or
//! DaemonSet, compiles to a checksum of the notifier on the workload's pod
//! template. When the notifier's content changes the checksum changes, and
//! Kubernetes rolls the pods. That is a synthetic notify and is declared as
//! one.

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

pub use emit::{CLUSTER_PATH, MANIFESTS_PATH};
pub use import::KubernetesImporter;
pub use runner::KubernetesRunner;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "kubernetes";

/// One object to apply.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Object {
    /// The resource's id in the document.
    pub id: String,
    /// `apps/v1`, or `v1` for the core group.
    pub api_version: String,
    pub kind: String,
    pub name: String,
    /// Empty when the object leaves its namespace to the cluster settings.
    pub namespace: String,
    /// Every field but `apiVersion` and `kind`.
    pub body: BTreeMap<String, Value>,
    pub pos: Position,
}

impl Object {
    /// The API group: `apps` for `apps/v1`, empty for the core group.
    pub fn group(&self) -> &str {
        self.api_version
            .rsplit_once('/')
            .map_or("", |(group, _)| group)
    }

    pub fn version(&self) -> &str {
        self.api_version
            .rsplit_once('/')
            .map_or(self.api_version.as_str(), |(_, version)| version)
    }

    /// How `kubectl apply` names the object in its output:
    /// `deployment.apps/web`, `service/web`.
    pub fn applied_as(&self) -> String {
        let kind = self.kind.to_ascii_lowercase();
        if self.group().is_empty() {
            format!("{kind}/{}", self.name)
        } else {
            format!("{kind}.{}/{}", self.group(), self.name)
        }
    }

    /// The fully qualified type `kubectl get` takes: `deployment.v1.apps`.
    pub fn get_as(&self) -> String {
        let kind = self.kind.to_ascii_lowercase();
        if self.group().is_empty() {
            kind
        } else {
            format!("{kind}.{}.{}", self.version(), self.group())
        }
    }
}

/// Which cluster the manifests go to, from the document's `cluster` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Cluster {
    pub context: String,
    pub namespace: String,
    pub kubeconfig: String,
}

/// One piece of an output's value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutputPart {
    Text(String),
    /// A path read from a live object, by the object's index.
    Read {
        object: usize,
        path: String,
    },
}

/// A whole set of manifests and where they go.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Manifests {
    pub cluster: Cluster,
    pub objects: Vec<Object>,
    pub outputs: Vec<(String, Vec<OutputPart>)>,
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

/// The Kubernetes emitter.
pub struct Kubernetes;

impl Emitter for Kubernetes {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        ["<apiVersion>/<Kind>", "cluster"]
            .map(str::to_owned)
            .to_vec()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
        }
    }

    fn category(&self) -> Category {
        Category::ContainerOrchestration
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut manifests = transform::transform(graph)?;
        let warnings = sort::apply_ordering(graph, &mut manifests)?;
        validate::validate(&manifests)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&manifests),
            },
            warnings,
        })
    }
}
