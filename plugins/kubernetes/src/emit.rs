//! Stage four: the objects become their files.

use std::collections::BTreeMap;

use meridian_sdk::Value;
use meridian_yaml::Yaml;

use meridian_sdk::SecretName;

use crate::{secret_token, Manifests, Object, OutputPart};

/// Every object, one YAML document each, in apply order.
pub const MANIFESTS_PATH: &str = "manifests.yaml";
/// Which cluster they go to, what `kubectl` will call each object, and how to
/// read each output. The runner reads it; Kubernetes never does.
pub const CLUSTER_PATH: &str = "cluster.json";

fn yaml(value: &Value, secrets: &[SecretName]) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(i) => Yaml::Int(*i),
        Value::Uint(u) => Yaml::Uint(*u),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::str(s),
        // A secret is written as its token, between the quotes of a JSON
        // string, so the runner can put the value between the same quotes.
        // A slot the graph does not name is written as its mask.
        Value::Secret(slot) => match secrets.iter().find(|s| s.secret == *slot) {
            Some(named) => Yaml::Raw(format!("\"{}\"", secret_token(&named.name))),
            None => Yaml::str(meridian_sdk::SENSITIVE),
        },
        Value::List(items) => Yaml::Seq(items.iter().map(|v| yaml(v, secrets)).collect()),
        Value::Map(map) => Yaml::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), yaml(v, secrets)))
                .collect(),
        ),
    }
}

/// A map with the keys a reader looks for first moved to the front.
fn leading(map: &BTreeMap<String, Value>, first: &[&str], secrets: &[SecretName]) -> Yaml {
    let mut pairs = Vec::with_capacity(map.len());
    for key in first {
        if let Some(value) = map.get(*key) {
            pairs.push(((*key).to_owned(), yaml(value, secrets)));
        }
    }
    for (key, value) in map {
        if !first.contains(&key.as_str()) {
            pairs.push((key.clone(), yaml(value, secrets)));
        }
    }
    Yaml::Map(pairs)
}

fn string(text: &str) -> Value {
    Value::String(text.to_owned())
}

/// Renders the manifests. This stage performs no mapping and no ordering.
/// Each object is laid out the way Kubernetes' own documentation does:
/// `apiVersion`, `kind`, `metadata`, then the rest.
pub(crate) fn serialise(manifests: &Manifests) -> BTreeMap<String, String> {
    let text = render(manifests, &manifests.objects.iter().collect::<Vec<_>>());
    let mut files = BTreeMap::new();
    let mut waves = Vec::new();
    for (n, wave) in manifests.waves.iter().enumerate() {
        let objects: Vec<&Object> = wave.iter().map(|i| &manifests.objects[*i]).collect();
        let file = wave_path(n + 1);
        files.insert(file.clone(), render(manifests, &objects));
        // The last wave has nothing after it to wait for.
        let waits = if n + 1 == manifests.waves.len() {
            Vec::new()
        } else {
            objects
                .iter()
                .filter_map(|o| {
                    let wait = wait_for(&o.kind)?;
                    Some(Value::Map(BTreeMap::from([
                        ("object".to_owned(), string(&o.applied_as())),
                        ("namespace".to_owned(), string(&o.namespace)),
                        ("for".to_owned(), string(wait)),
                    ])))
                })
                .collect()
        };
        waves.push(Value::Map(BTreeMap::from([
            ("file".to_owned(), string(&file)),
            ("wait".to_owned(), Value::List(waits)),
        ])));
    }
    files.insert(MANIFESTS_PATH.to_owned(), text);
    files.insert(CLUSTER_PATH.to_owned(), cluster(manifests, waves).pretty());
    files
}

/// The file of wave `n`, counted from 1.
pub fn wave_path(n: usize) -> String {
    format!("wave-{n}.yaml")
}

/// What the runner waits for on an object of `kind` before the next wave:
/// `rollout` for a workload, `complete` for a Job, `established` for a
/// CustomResourceDefinition, nothing for any other kind.
pub fn wait_for(kind: &str) -> Option<&'static str> {
    match kind {
        "Deployment" | "StatefulSet" | "DaemonSet" => Some("rollout"),
        "Job" => Some("complete"),
        "CustomResourceDefinition" => Some("established"),
        _ => None,
    }
}

/// The objects as YAML documents, in the order given.
fn render(manifests: &Manifests, objects: &[&Object]) -> String {
    let mut text = String::new();
    for object in objects {
        let mut pairs = vec![
            ("apiVersion".to_owned(), Yaml::str(&object.api_version)),
            ("kind".to_owned(), Yaml::str(&object.kind)),
        ];
        if let Some(Value::Map(metadata)) = object.body.get("metadata") {
            pairs.push((
                "metadata".to_owned(),
                leading(metadata, &["name", "namespace"], &manifests.secrets),
            ));
        }
        for (key, value) in &object.body {
            if key != "metadata" {
                pairs.push((key.clone(), yaml(value, &manifests.secrets)));
            }
        }
        text.push_str("---\n");
        text.push_str(&meridian_yaml::write(&Yaml::Map(pairs)));
    }
    text
}

/// `cluster.json`: where the manifests go, what kubectl calls each object,
/// how to read each output, and the waves.
fn cluster(manifests: &Manifests, waves: Vec<Value>) -> Value {
    let outputs = manifests
        .outputs
        .iter()
        .map(|(name, parts)| {
            let parts = parts
                .iter()
                .map(|part| match part {
                    OutputPart::Text(literal) => {
                        Value::Map(BTreeMap::from([("text".to_owned(), string(literal))]))
                    }
                    OutputPart::Read { object, path } => {
                        let object = &manifests.objects[*object];
                        Value::Map(BTreeMap::from([
                            ("get".to_owned(), string(&object.get_as())),
                            ("name".to_owned(), string(&object.name)),
                            ("namespace".to_owned(), string(&object.namespace)),
                            ("path".to_owned(), string(path)),
                        ]))
                    }
                })
                .collect();
            Value::Map(BTreeMap::from([
                ("name".to_owned(), string(name)),
                ("parts".to_owned(), Value::List(parts)),
            ]))
        })
        .collect();
    let mut map = BTreeMap::from([
        ("context".to_owned(), string(&manifests.cluster.context)),
        ("namespace".to_owned(), string(&manifests.cluster.namespace)),
        (
            "kubeconfig".to_owned(),
            string(&manifests.cluster.kubeconfig),
        ),
        (
            "objects".to_owned(),
            Value::Map(
                manifests
                    .objects
                    .iter()
                    .map(|o| (o.applied_as(), string(&o.id)))
                    .collect(),
            ),
        ),
        ("outputs".to_owned(), Value::List(outputs)),
    ]);
    if !waves.is_empty() {
        map.insert("waves".to_owned(), Value::List(waves));
    }
    Value::Map(map)
}
