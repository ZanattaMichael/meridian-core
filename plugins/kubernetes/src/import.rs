//! Importing existing Kubernetes manifests.
//!
//! A manifest is already what this target compiles to, so the mapping is the
//! target's own, read backwards: an object's `apiVersion` and `kind` become
//! the resource's type, everything else becomes its params, and the object's
//! name gives the resource its id.
//!
//! Three things are not carried over as written, and each is reported:
//!
//! - what the cluster wrote into an object that was read back from it
//!   (`status`, `metadata.uid`, `metadata.managedFields` and the like), which
//!   is not desired state;
//! - a `${` in a string, which a document would read as a reference and a
//!   manifest never means as one, so it is written `$${`;
//! - anything that is not an object, such as a Kustomization, which is
//!   instructions for another tool.

use std::collections::{BTreeMap, BTreeSet};

use meridian_import::yaml::{self, Kind, Node, Rules};
use meridian_import::{literal, slug, Notes};
use meridian_sdk::{
    DocumentKind, ImportError, ImportFormat, ImportSource, Imported, ImportedDocument,
    ImportedResource, Importer, Value,
};

use crate::NAME;

/// Reads manifests into a document for this target.
pub struct KubernetesImporter;

/// What a cluster writes into an object's metadata. None of it is desired
/// state, and `kubectl apply` refuses some of it.
const SERVER_METADATA: &[&str] = &[
    "creationTimestamp",
    "deletionGracePeriodSeconds",
    "deletionTimestamp",
    "generation",
    "managedFields",
    "resourceVersion",
    "selfLink",
    "uid",
];

/// The annotations a cluster or `kubectl` writes for its own bookkeeping.
const SERVER_ANNOTATIONS: &[&str] = &[
    "deployment.kubernetes.io/revision",
    "kubectl.kubernetes.io/last-applied-configuration",
];

impl Importer for KubernetesImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        vec![ImportFormat::new(
            "manifests",
            "Kubernetes manifests",
            &["*.yaml", "*.yml", "*.json"],
        )]
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        let name = if source.name.is_empty() {
            "imported".to_owned()
        } else {
            source.name.clone()
        };
        let mut notes = Notes::new();
        notes.in_document(&name);

        // Every object of every file, in file order: that is the order
        // `kubectl apply` would have sent them in.
        let mut objects: Vec<(String, &Node)> = Vec::new();
        let mut parsed = Vec::new();
        for (file, text) in &source.files {
            if text.contains("{{") && text.contains("}}") && looks_templated(text) {
                return Err(ImportError::at(
                    file,
                    0,
                    "this looks like a Helm template, not a manifest. Render the chart with \
                     `helm template` and import what it prints, or use the helm target",
                ));
            }
            // Kubernetes reads its YAML by the older rules, where `yes` and
            // `on` are booleans.
            parsed.push((file, yaml::read(file, text, Rules::Yaml11)?));
        }
        for (file, documents) in &parsed {
            for document in documents {
                flatten(file, document, &mut objects);
            }
        }
        if objects.is_empty() {
            return Err(ImportError::new(
                "there are no manifests to import: the files hold no objects",
            ));
        }

        let mut taken = BTreeSet::new();
        let mut resources = Vec::new();
        for (file, node) in objects {
            let origin = format!("{file}:{}", node.line);
            let Value::Map(mut body) = node.value() else {
                notes.untranslated(
                    &origin,
                    "document",
                    "",
                    format!(
                        "a manifest is a mapping, and this is {}; it was left out",
                        node.describe()
                    ),
                );
                continue;
            };
            let (Some(Value::String(api_version)), Some(Value::String(kind))) =
                (body.remove("apiVersion"), body.remove("kind"))
            else {
                notes.untranslated(
                    &origin,
                    "document",
                    "",
                    "this has no apiVersion and kind, so it is not a Kubernetes object; it was \
                     left out",
                );
                continue;
            };
            let object_name = body
                .get("metadata")
                .and_then(Value::as_map)
                .and_then(|m| m.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let subject = format!("{kind} {object_name:?}");

            if api_version.starts_with("kustomize.config.k8s.io/") {
                notes.untranslated(
                    &origin,
                    &subject,
                    "",
                    "this is instructions for kustomize, not an object a cluster holds. Build \
                     it with `kubectl kustomize` and import what that prints",
                );
                continue;
            }
            let r#type = format!("{api_version}/{kind}");
            if !is_object_type(&api_version, &kind) {
                notes.untranslated(
                    &origin,
                    &subject,
                    "",
                    format!(
                        "{type:?} is not an API version and a kind this target accepts, so the \
                         object was left out"
                    ),
                );
                continue;
            }

            // The id is the object's name; a second object of the same name,
            // which is the usual case for a Deployment and its Service, gets
            // its kind as well. The name itself is never changed.
            let base = if object_name.is_empty() {
                kind.to_ascii_lowercase()
            } else {
                slug(&object_name, '_')
            };
            let id = if taken.insert(base.clone()) {
                base
            } else {
                meridian_import::unique_id(
                    &format!("{base}_{}", kind.to_ascii_lowercase()),
                    &mut taken,
                )
            };

            let mut dropped = Vec::new();
            if body.remove("status").is_some() {
                dropped.push("status".to_owned());
            }
            let mut has_generate_name = false;
            if let Some(Value::Map(metadata)) = body.get_mut("metadata") {
                for key in SERVER_METADATA {
                    if metadata.remove(*key).is_some() {
                        dropped.push(format!("metadata.{key}"));
                    }
                }
                if let Some(Value::Map(annotations)) = metadata.get_mut("annotations") {
                    for key in SERVER_ANNOTATIONS {
                        if annotations.remove(*key).is_some() {
                            dropped.push(format!("metadata.annotations[{key}]"));
                        }
                    }
                    if annotations.is_empty() {
                        metadata.remove("annotations");
                    }
                }
                has_generate_name = metadata.contains_key("generateName");
                // An object with no name of its own is named after its id,
                // so a name the id already gives is not written twice.
                if id.replace('_', "-") == object_name {
                    metadata.remove("name");
                }
                if metadata.is_empty() {
                    body.remove("metadata");
                }
            }
            if !dropped.is_empty() {
                notes.info(
                    &origin,
                    &subject,
                    &id,
                    format!(
                        "left out what the cluster wrote and a manifest does not declare: {}",
                        dropped.join(", ")
                    ),
                );
            }
            if object_name.is_empty() {
                notes.changed(
                    &origin,
                    &subject,
                    &id,
                    if has_generate_name {
                        format!(
                            "the object has a generateName and no name. `kubectl apply` needs a \
                             name, so it is named {:?} after its id; remove generateName, or \
                             create this object some other way",
                            id.replace('_', "-")
                        )
                    } else {
                        format!(
                            "the object has no name, so it is named {:?} after its id",
                            id.replace('_', "-")
                        )
                    },
                );
            }

            let params: BTreeMap<String, Value> = body
                .iter()
                .map(|(key, value)| (key.clone(), literal(value)))
                .collect();
            resources.push(ImportedResource {
                id,
                r#type,
                params,
                origin,
                ..ImportedResource::default()
            });
        }
        if resources.is_empty() {
            return Err(ImportError::new(
                "there are no manifests to import: nothing in the files is a Kubernetes object",
            ));
        }

        Ok(Imported {
            documents: vec![ImportedDocument {
                kind: DocumentKind::Infrastructure,
                name,
                target: NAME.to_owned(),
                resources,
                ..ImportedDocument::default()
            }],
            notes: notes.into_vec(),
        })
    }
}

/// Whether a file uses Go template actions the way a chart does: at the
/// start of a line, or as a whole value. A manifest may hold `{{ }}` inside a
/// string, as a Prometheus rule does, and that is not a template.
fn looks_templated(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("{{")
            || line
                .split_once(": ")
                .is_some_and(|(_, value)| value.trim_start().starts_with("{{"))
    })
}

/// Adds the objects a document holds: itself, or the items of a `List`.
fn flatten<'a>(file: &str, node: &'a Node, out: &mut Vec<(String, &'a Node)>) {
    let is_list = node
        .get("kind")
        .and_then(Node::as_str)
        .is_some_and(|k| k.ends_with("List"));
    match node.get("items").map(|items| &items.kind) {
        Some(Kind::Seq(items)) if is_list => {
            for item in items {
                flatten(file, item, out);
            }
        }
        _ => out.push((file.to_owned(), node)),
    }
}

/// The same test the compiler applies to a resource type: a group and a
/// version, or a version alone, and a kind in upper camel case.
fn is_object_type(api_version: &str, kind: &str) -> bool {
    let version = api_version.rsplit_once('/').map_or(api_version, |(_, v)| v);
    let ok_kind = kind.starts_with(|c: char| c.is_ascii_uppercase())
        && kind.chars().all(|c| c.is_ascii_alphanumeric());
    let ok_version = version.starts_with('v')
        && version.len() > 1
        && version.chars().all(|c| c.is_ascii_alphanumeric());
    let ok_group = api_version
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '/');
    ok_kind && ok_version && ok_group && api_version.matches('/').count() <= 1
}
