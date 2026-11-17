//! Importing an existing Compose file.
//!
//! A Compose file is what this target compiles to, so the mapping is the
//! target's own, read backwards: each service, network, volume, config and
//! secret becomes a resource whose id is the element's name and whose params
//! are the element's fields.
//!
//! What a document says its own way is translated:
//!
//! - `depends_on` becomes `dependsOn` wherever the two mean exactly the same,
//!   and stays a param wherever they do not;
//! - `depends_on` with `restart: true` becomes `notifies` with the action
//!   `restart`, on the service that is waited for;
//! - the project's name becomes the document's name, and the directory the
//!   file was in becomes the project's `directory`, so the file's relative
//!   paths keep meaning what they meant.
//!
//! And what a document cannot say is reported: Compose's own `${VARIABLE}`
//! substitution, `include`, and an element whose name is already another
//! element's.

use std::collections::{BTreeMap, BTreeSet};

use meridian_import::yaml::{self, Rules};
use meridian_import::{literal, unique_id, Notes};
use meridian_sdk::{
    DocumentKind, ImportError, ImportFormat, ImportSource, Imported, ImportedDocument,
    ImportedNotify, ImportedResource, Importer, Value,
};

use crate::{Section, NAME};

/// Reads a Compose file into a document for this target.
pub struct DockerImporter;

impl Importer for DockerImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        vec![ImportFormat::new(
            "compose",
            "Compose files",
            &[
                "compose.yaml",
                "compose.yml",
                "docker-compose.yaml",
                "docker-compose.yml",
                "compose.*.yaml",
                "compose.*.yml",
                "docker-compose.*.yaml",
                "docker-compose.*.yml",
            ],
        )]
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        let mut files = source.files.iter();
        let Some((file, text)) = files.next() else {
            return Err(ImportError::new("there is no Compose file to import"));
        };
        if let Some((second, _)) = files.next() {
            return Err(ImportError::new(format!(
                "{file} and {second} are two Compose files, and Compose merges several files \
                 into one project by rules of its own. Import one file, or merge them first \
                 with `docker compose -f {file} -f {second} config` and import what that prints"
            )));
        }
        let documents = yaml::read(file, text, Rules::Yaml12)?;
        let [root] = documents.as_slice() else {
            return Err(ImportError::at(
                file,
                0,
                format!(
                    "a Compose file is one YAML document, and this holds {}",
                    documents.len()
                ),
            ));
        };
        let Some(top) = root.as_map() else {
            return Err(ImportError::at(
                file,
                root.line,
                format!(
                    "a Compose file is a mapping, and this is {}",
                    root.describe()
                ),
            ));
        };
        if !top.iter().any(|(key, _)| key == "services") {
            return Err(ImportError::at(
                file,
                root.line,
                "this has no services, so it is not a Compose file",
            ));
        }

        let project_name = root.get("name").and_then(|n| n.scalar_text());
        let name = match (&project_name, source.name.is_empty()) {
            (Some(project), _) => project.clone(),
            (None, false) => source.name.clone(),
            (None, true) => "imported".to_owned(),
        };
        let mut notes = Notes::new();
        notes.in_document(&name);

        // The elements, services first: a service is what the rest exist
        // for, so it is the one that keeps a name two elements share.
        let mut taken = BTreeSet::new();
        let mut elements: Vec<(Section, String, BTreeMap<String, Value>, String)> = Vec::new();
        for section in Section::ALL {
            let Some(node) = root.get(section.key()) else {
                continue;
            };
            let entries = match &node.kind {
                yaml::Kind::Map(entries) => entries.as_slice(),
                yaml::Kind::Null => &[],
                _ => {
                    return Err(ImportError::at(
                        file,
                        node.line,
                        format!("{} must be a mapping of names", section.key()),
                    ))
                }
            };
            for (element, body) in entries {
                let origin = format!("{file}:{}", body.line);
                let subject = format!("{} {element:?}", section.type_name());
                let usable = !element.is_empty()
                    && element
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                if !usable {
                    notes.untranslated(
                        &origin,
                        &subject,
                        "",
                        "a resource's id is the element's name, and an id holds only letters, \
                         digits, underscores and hyphens. Rename it in the Compose file and \
                         import again; it was left out",
                    );
                    continue;
                }
                if !taken.insert(element.clone()) {
                    notes.untranslated(
                        &origin,
                        &subject,
                        "",
                        format!(
                            "a resource's id is the element's name and a document's ids are \
                             unique, but {element:?} is already the name of another element. \
                             Rename one of them in the Compose file and import again; this one \
                             was left out, so the document will not compile where a service \
                             uses it"
                        ),
                    );
                    continue;
                }
                let fields = match body.value() {
                    Value::Map(fields) => fields,
                    Value::Null => BTreeMap::new(),
                    _ => {
                        notes.untranslated(
                            &origin,
                            &subject,
                            "",
                            format!(
                                "its definition must be a mapping and is {}; it was left out",
                                body.describe()
                            ),
                        );
                        taken.remove(element);
                        continue;
                    }
                };
                elements.push((section, element.clone(), fields, origin));
            }
        }

        for (key, node) in top {
            let origin = format!("{file}:{}", node.line);
            match key.as_str() {
                "name" | "services" | "networks" | "volumes" | "configs" | "secrets" => {}
                "version" => notes.info(
                    &origin,
                    "version",
                    "",
                    "left out: Compose has ignored the version field since it stopped having \
                     file format versions",
                ),
                "include" => notes.untranslated(
                    &origin,
                    "include",
                    "",
                    "the files it includes were not read. Merge them first with `docker \
                     compose config` and import what that prints",
                ),
                extension if extension.starts_with("x-") => notes.info(
                    &origin,
                    extension,
                    "",
                    "left out: an extension field is not part of the project. Anything that \
                     reused it through a YAML anchor was written out in full",
                ),
                other => notes.untranslated(
                    &origin,
                    other,
                    "",
                    "this is not something a Compose file declares at its top level, so it was \
                     left out",
                ),
            }
        }

        let healthy: BTreeSet<&str> = elements
            .iter()
            .filter(|(section, _, fields, _)| {
                *section == Section::Service
                    && matches!(fields.get("healthcheck"), Some(Value::Map(h))
                        if h.get("disable") != Some(&Value::Bool(true)))
            })
            .map(|(_, name, _, _)| name.as_str())
            .collect();
        let services: BTreeSet<&str> = elements
            .iter()
            .filter(|(section, ..)| *section == Section::Service)
            .map(|(_, name, _, _)| name.as_str())
            .collect();

        let mut resources: Vec<ImportedResource> = Vec::new();
        let mut restarts: Vec<(String, String)> = Vec::new();
        for (section, element, fields, origin) in &elements {
            let subject = format!("{} {element:?}", section.type_name());
            let mut fields = fields.clone();
            let extensions: Vec<String> = fields
                .keys()
                .filter(|k| k.starts_with("x-"))
                .cloned()
                .collect();
            for key in &extensions {
                fields.remove(key);
            }
            if !extensions.is_empty() {
                notes.info(
                    origin,
                    &subject,
                    element,
                    format!(
                        "left out its extension fields, which Compose itself ignores: {}",
                        extensions.join(", ")
                    ),
                );
            }

            let mut depends_on = Vec::new();
            if *section == Section::Service {
                if let Some(written) = fields.remove("depends_on") {
                    let kept = dependencies(
                        written,
                        element,
                        &services,
                        &healthy,
                        &mut depends_on,
                        &mut restarts,
                    );
                    if let Some(kept) = kept {
                        fields.insert("depends_on".to_owned(), kept);
                    }
                }
            }

            let mut variables = BTreeSet::new();
            let params: BTreeMap<String, Value> = fields
                .iter()
                .map(|(key, value)| (key.clone(), literal(&unescape(value, &mut variables))))
                .collect();
            if !variables.is_empty() {
                notes.untranslated(
                    origin,
                    &subject,
                    element,
                    format!(
                        "Compose would have put the value of {} here when the project started. \
                         A document carries text literally, so the variable's name is what the \
                         container gets; write the value in its place",
                        variables.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                );
            }
            resources.push(ImportedResource {
                id: element.clone(),
                r#type: section.type_name().to_owned(),
                params,
                depends_on,
                origin: origin.clone(),
                ..ImportedResource::default()
            });
        }
        for (notifier, notified) in restarts {
            if let Some(r) = resources.iter_mut().find(|r| r.id == notifier) {
                r.notifies.push(ImportedNotify {
                    resource: notified,
                    action: "restart".to_owned(),
                });
            }
        }

        // Compose resolves a relative path against the directory its file is
        // in. The compiled file lives elsewhere, so the project says where.
        if !source.root.is_empty() {
            let id = unique_id("project", &mut taken);
            notes.info(
                file,
                "project directory",
                &id,
                format!(
                    "relative paths in the file (a build context, a bind mount, an env_file) \
                     are resolved against {}, the directory the file was in, as Compose did",
                    source.root
                ),
            );
            resources.insert(
                0,
                ImportedResource {
                    id,
                    r#type: "project".to_owned(),
                    params: BTreeMap::from([(
                        "directory".to_owned(),
                        Value::String(source.root.clone()),
                    )]),
                    origin: file.clone(),
                    ..ImportedResource::default()
                },
            );
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

/// Splits a service's `depends_on` into what a document says its own way and
/// what has to stay as written. Returns what stays.
///
/// A document's `dependsOn` compiles to waiting for the other service to be
/// healthy when it declares a healthcheck, and to have started when it does
/// not. An entry that asks for exactly that is a `dependsOn`; one that also
/// asks for a restart is a `notifies`; anything else stays a param, which the
/// target writes back out untouched.
fn dependencies(
    written: Value,
    service: &str,
    services: &BTreeSet<&str>,
    healthy: &BTreeSet<&str>,
    depends_on: &mut Vec<String>,
    restarts: &mut Vec<(String, String)>,
) -> Option<Value> {
    let entries: Vec<(String, BTreeMap<String, Value>)> = match written {
        Value::List(names) => names
            .into_iter()
            .filter_map(|n| Some((n.as_str()?.to_owned(), BTreeMap::new())))
            .collect(),
        Value::Map(entries) => entries
            .into_iter()
            .map(|(name, fields)| match fields {
                Value::Map(fields) => (name, fields),
                _ => (name, BTreeMap::new()),
            })
            .collect(),
        other => return Some(other),
    };
    let mut kept = BTreeMap::new();
    for (on, fields) in entries {
        let condition = fields
            .get("condition")
            .and_then(Value::as_str)
            .unwrap_or("service_started");
        let default = if healthy.contains(on.as_str()) {
            "service_healthy"
        } else {
            "service_started"
        };
        let restart = fields.get("restart") == Some(&Value::Bool(true));
        let plain = fields.keys().all(|k| {
            k == "condition"
                || (k == "restart" && fields.get(k).is_some_and(|v| matches!(v, Value::Bool(_))))
                || (k == "required" && fields.get(k) == Some(&Value::Bool(true)))
        });
        if plain && condition == default && services.contains(on.as_str()) && on != service {
            if restart {
                restarts.push((on, service.to_owned()));
            } else {
                depends_on.push(on);
            }
        } else {
            let mut fields = fields;
            fields
                .entry("condition".to_owned())
                .or_insert_with(|| Value::String("service_started".to_owned()));
            kept.insert(on, Value::Map(fields));
        }
    }
    (!kept.is_empty()).then_some(Value::Map(kept))
}

/// Reads a value's text the way Compose does: `$$` is one dollar, and a
/// single dollar starts a variable, whose name is recorded.
fn unescape(value: &Value, variables: &mut BTreeSet<String>) -> Value {
    match value {
        Value::String(s) => Value::String(unescape_text(s, variables)),
        Value::List(items) => Value::List(items.iter().map(|v| unescape(v, variables)).collect()),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), unescape(v, variables)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn unescape_text(text: &str, variables: &mut BTreeSet<String>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('$') => {
                out.push('$');
                i += 2;
            }
            Some('{') => {
                let end = chars[i..].iter().position(|c| *c == '}').map(|n| i + n);
                let Some(end) = end else {
                    out.push('$');
                    i += 1;
                    continue;
                };
                let inside: String = chars[i + 2..end].iter().collect();
                let variable: String = inside
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                variables.insert(format!("${{{variable}}}"));
                out.extend(&chars[i..=end]);
                i = end + 1;
            }
            Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
                let variable: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .collect();
                variables.insert(format!("${variable}"));
                out.push('$');
                i += 1;
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_doubled_dollar_is_one_and_a_single_one_names_a_variable() {
        let mut variables = BTreeSet::new();
        assert_eq!(
            unescape_text("echo $$HOME costs $5", &mut variables),
            "echo $HOME costs $5"
        );
        assert!(variables.is_empty());
        let text = "img:${TAG:-latest} $USER $${literal}";
        assert_eq!(
            unescape_text(text, &mut variables),
            "img:${TAG:-latest} $USER ${literal}"
        );
        assert_eq!(
            variables.into_iter().collect::<Vec<_>>(),
            ["$USER", "${TAG}"]
        );
    }
}
