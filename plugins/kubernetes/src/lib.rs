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
//! be ready before the next. `dependsOn` therefore orders the file and splits
//! the objects into waves by depth; the runner applies one wave at a time
//! and waits for its workloads, Jobs and CustomResourceDefinitions before the
//! next.
//!
//! `notifies` with the action `restart`, aimed at a Deployment, StatefulSet or
//! DaemonSet, compiles to a checksum of the notifier on the workload's pod
//! template. When the notifier's content changes the checksum changes, and
//! Kubernetes rolls the pods. That is a synthetic notify and is declared as
//! one.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
pub mod failures;
mod import;
pub mod inspect;
mod runner;
mod schemas;
mod sort;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{
    holds_secret, Adoption, Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter,
    Position, Resource, ResourceGraph, SecretName, Value,
};

pub use emit::{CLUSTER_PATH, MANIFESTS_PATH};
pub use import::KubernetesImporter;
pub use runner::KubernetesRunner;
pub use schemas::KubernetesSchemas;

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
    /// The name of each secret a `stringData` value stands for, so the
    /// manifest can carry the token the runner fills in.
    pub secrets: Vec<SecretName>,
    /// The objects, by index, in the waves `dependsOn` puts them in; empty
    /// when the document has one wave.
    pub waves: Vec<Vec<usize>>,
}

/// What a secret's value is written as in the manifests: a token inside a
/// JSON string, which the runner replaces with the value on the way to
/// `kubectl`'s standard input. The file on disk never holds the value.
pub fn secret_token(name: &str) -> String {
    format!("${{meridian_secret_{name}}}")
}

/// The one place a secret may be: a value of a `v1/Secret`'s `stringData`,
/// which the cluster stores as a Secret and a workload reads from there.
/// Anywhere else it would be written into an object every reader of that
/// object can see, or onto `kubectl`'s command line.
fn refuse_secrets_outside_string_data(graph: &ResourceGraph) -> Result<(), CompileError> {
    for r in &graph.resources {
        for (key, value) in &r.params {
            if !holds_secret(value) {
                continue;
            }
            let place = format!("param {key:?} of {:?}", r.r#type);
            let msg = if r.r#type == "cluster" {
                format!(
                    "{place} holds a secret, and the cluster settings are the command line \
                     kubectl is given, where a secret cannot go"
                )
            } else if r.r#type != "v1/Secret" {
                format!(
                    "{place} holds a secret, and it would be written into the object for every \
                     reader of it to see. A secret goes in the stringData of a v1/Secret, which \
                     the workload reads by name"
                )
            } else if key == "data" {
                format!(
                    "{place} holds a secret; data is base64 the document writes itself, so put \
                     the secret in stringData, which Kubernetes encodes"
                )
            } else if key != "stringData" {
                format!(
                    "{place} holds a secret, and only a value of stringData can; everything else \
                     is written into the object as it is"
                )
            } else {
                match value {
                    Value::Map(entries)
                        if entries
                            .values()
                            .all(|v| matches!(v, Value::Secret(_)) || !holds_secret(v)) =>
                    {
                        continue
                    }
                    _ => format!(
                        "{place} holds a secret inside a value; a stringData value is a whole \
                         string, so each secret is a value of its own"
                    ),
                }
            };
            return Err(refuse(r, &rules::UNSUPPORTED_SECRET_PLACE, msg));
        }
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
            Value::Map(map) => map.values().any(has_token),
            _ => false,
        }
    }
    for r in &graph.resources {
        if let Some((key, _)) = r.params.iter().find(|(_, v)| has_token(v)) {
            return Err(refuse(
                r,
                &rules::RESERVED_NAME,
                format!(
                    "param {key:?} holds the text \"${{meridian_secret_\", which is how this \
                     target writes a secret's place in a manifest; write the secret as \
                     ${{{{ secret.<name> }}}} or choose other text"
                ),
            ));
        }
    }
    Ok(())
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
            secrets: true,
            credentials: false,
            cloud_credentials: false,
            adopt: Adoption::Converge,
        }
    }

    fn category(&self) -> Category {
        Category::ContainerOrchestration
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        refuse_secrets_outside_string_data(graph)?;
        refuse_token_text(graph)?;
        let mut manifests = transform::transform(graph)?;
        manifests.secrets = graph.secrets.clone();
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
