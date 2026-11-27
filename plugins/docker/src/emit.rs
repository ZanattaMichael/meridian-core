//! Stage four: the elements become their files.

use std::collections::BTreeMap;

use meridian_sdk::Value;
use meridian_yaml::Yaml;

use crate::{Composition, OutputPart, Section};

/// The Compose file.
pub const COMPOSE_PATH: &str = "compose.yaml";
/// What the project is called, which daemon it runs on, how it is brought up
/// and how to read each output. The runner reads it; Docker never does.
pub const PROJECT_PATH: &str = "project.json";

/// The keys of a service a reader looks for first.
const SERVICE_FIRST: &[&str] = &["image", "build", "command", "depends_on"];

fn yaml(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(i) => Yaml::Int(*i),
        Value::Uint(u) => Yaml::Uint(*u),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::str(s),
        // A plugin is never sent a secret as a value; one would be written as its mask.
        Value::Secret(_) => Yaml::str(meridian_sdk::SENSITIVE),
        Value::List(items) => Yaml::Seq(items.iter().map(yaml).collect()),
        Value::Map(map) => Yaml::Map(map.iter().map(|(k, v)| (k.clone(), yaml(v))).collect()),
    }
}

/// A map with the keys a reader looks for first moved to the front.
fn leading(map: &BTreeMap<String, Value>, first: &[&str]) -> Yaml {
    let mut pairs = Vec::with_capacity(map.len());
    for key in first {
        if let Some(value) = map.get(*key) {
            pairs.push(((*key).to_owned(), yaml(value)));
        }
    }
    for (key, value) in map {
        if !first.contains(&key.as_str()) {
            pairs.push((key.clone(), yaml(value)));
        }
    }
    Yaml::Map(pairs)
}

fn string(text: &str) -> Value {
    Value::String(text.to_owned())
}

fn map<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// Renders the project. This stage performs no mapping and no ordering.
/// Sections come in the order the Compose specification presents them, and
/// the elements of each in the order the document declares them.
pub(crate) fn serialise(composition: &Composition) -> BTreeMap<String, String> {
    let mut sections = vec![("name".to_owned(), Yaml::str(&composition.project.name))];
    for section in Section::ALL {
        let elements: Vec<(String, Yaml)> = composition
            .elements
            .iter()
            .filter(|e| e.section == section)
            .map(|e| {
                let first = if section == Section::Service {
                    SERVICE_FIRST
                } else {
                    &[]
                };
                (e.id.clone(), leading(&e.body, first))
            })
            .collect();
        if !elements.is_empty() {
            sections.push((section.key().to_owned(), Yaml::Map(elements)));
        }
    }

    let outputs = composition
        .outputs
        .iter()
        .map(|(name, parts)| {
            let parts = parts
                .iter()
                .map(|part| match part {
                    OutputPart::Text(literal) => map([("text", string(literal))]),
                    OutputPart::Port { service, port } => {
                        map([("service", string(service)), ("port", string(port))])
                    }
                    OutputPart::Inspect { service, path } => {
                        map([("service", string(service)), ("inspect", string(path))])
                    }
                })
                .collect();
            map([("name", string(name)), ("parts", Value::List(parts))])
        })
        .collect();
    let named = |section: Section| {
        Value::List(
            composition
                .elements
                .iter()
                .filter(|e| e.section == section)
                .map(|e| string(&e.id))
                .collect(),
        )
    };
    let project = map([
        ("name", string(&composition.project.name)),
        ("host", string(&composition.project.host)),
        ("context", string(&composition.project.context)),
        ("directory", string(&composition.project.directory)),
        (
            "profiles",
            Value::List(
                composition
                    .project
                    .profiles
                    .iter()
                    .map(|p| string(p))
                    .collect(),
            ),
        ),
        ("wait", Value::Bool(composition.project.wait)),
        (
            "wait_timeout",
            Value::Uint(composition.project.wait_timeout),
        ),
        ("services", named(Section::Service)),
        (
            "adopt",
            Value::List(
                composition
                    .elements
                    .iter()
                    .filter(|e| e.adopt)
                    .map(|e| string(&e.id))
                    .collect(),
            ),
        ),
        ("outputs", Value::List(outputs)),
    ]);

    BTreeMap::from([
        (
            COMPOSE_PATH.to_owned(),
            meridian_yaml::write(&Yaml::Map(sections)),
        ),
        (PROJECT_PATH.to_owned(), project.pretty()),
    ])
}
