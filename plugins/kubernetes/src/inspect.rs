//! Reading one object for `meridian inspect`: which object the id compiled
//! to, from the manifests, and the live object cut to what they declare.

use meridian_import::yaml::{self, Rules};
use meridian_sdk::{Data, Value};

/// The annotation `kubectl apply` keeps the whole applied object in,
/// secrets and all.
const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

/// One object the manifests declare.
#[derive(Debug, PartialEq)]
pub struct Declared {
    /// As `kubectl` names it: `deployment.apps/web`.
    pub applied_as: String,
    /// Its own namespace; empty for the cluster's.
    pub namespace: String,
    pub kind: String,
    /// The object as written, to cut the live one to.
    pub shape: Value,
}

/// The objects in `manifests` whose `kubectl` name is one of `names`.
pub fn declared(manifests: &str, names: &[&str]) -> Result<Vec<Declared>, String> {
    let documents = yaml::read("manifests", manifests, Rules::Yaml11).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for document in documents {
        let value = document.value();
        let Some(map) = value.as_map() else { continue };
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let metadata = map.get("metadata").and_then(Value::as_map);
        let meta = |key: &str| {
            metadata
                .and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let kind = text("kind");
        let api_version = text("apiVersion");
        let group = api_version.rsplit_once('/').map_or("", |(g, _)| g);
        let lower = kind.to_ascii_lowercase();
        let applied_as = if group.is_empty() {
            format!("{lower}/{}", meta("name"))
        } else {
            format!("{lower}.{group}/{}", meta("name"))
        };
        if names.contains(&applied_as.as_str()) {
            found.push(Declared {
                applied_as,
                namespace: meta("namespace"),
                kind,
                shape: value.clone(),
            });
        }
    }
    Ok(found)
}

/// The live object as `kubectl get --output json` printed it, cut to the
/// fields the manifest declares, under the manifest's top-level keys. A
/// Secret's values are never shown.
pub fn actual(live: &str, declared: &Declared) -> Result<Data, String> {
    let live = meridian_sdk::decode_value(live.trim().as_bytes())?;
    let (Some(live), Some(shape)) = (live.as_map(), declared.shape.as_map()) else {
        return Ok(Data::new());
    };
    let mut actual = Data::new();
    for (key, wanted) in shape {
        if matches!(key.as_str(), "apiVersion" | "kind") {
            continue;
        }
        let Some(have) = live.get(key) else { continue };
        let mut cut = cut_to(have, wanted);
        if let Value::Map(metadata) = &mut cut {
            if let Some(Value::Map(annotations)) = metadata.get_mut("annotations") {
                annotations.remove(LAST_APPLIED);
            }
        }
        if declared.kind == "Secret" && matches!(key.as_str(), "data" | "stringData") {
            cut = hidden(&cut);
        }
        actual.insert(key.clone(), cut);
    }
    Ok(actual)
}

/// `have` with every map cut to the keys `wanted` has.
fn cut_to(have: &Value, wanted: &Value) -> Value {
    match (have, wanted) {
        (Value::Map(have), Value::Map(wanted)) => Value::Map(
            have.iter()
                .filter_map(|(k, v)| Some((k.clone(), cut_to(v, wanted.get(k)?))))
                .collect(),
        ),
        (Value::List(have), Value::List(wanted)) => Value::List(
            have.iter()
                .enumerate()
                .map(|(i, v)| match wanted.get(i) {
                    Some(w) => cut_to(v, w),
                    None => v.clone(),
                })
                .collect(),
        ),
        _ => have.clone(),
    }
}

fn hidden(value: &Value) -> Value {
    match value {
        Value::Map(map) => Value::Map(
            map.keys()
                .map(|k| (k.clone(), Value::from(meridian_sdk::SENSITIVE)))
                .collect(),
        ),
        _ => Value::from(meridian_sdk::SENSITIVE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFESTS: &str = "---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: shop
spec:
  replicas: 2
  template:
    spec:
      containers:
        - name: web
          image: nginx:1.27
---
apiVersion: v1
kind: Secret
metadata:
  name: creds
data:
  token: c2VjcmV0
";

    #[test]
    fn the_object_is_found_by_its_kubectl_name_with_its_namespace() {
        let found = declared(MANIFESTS, &["deployment.apps/web"]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].namespace, "shop");
        assert_eq!(found[0].kind, "Deployment");
        assert!(declared(MANIFESTS, &["deployment.apps/other"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_live_object_is_cut_to_what_is_declared_and_a_secret_is_hidden() {
        let found = declared(MANIFESTS, &["deployment.apps/web", "secret/creds"]).unwrap();
        let live = r#"{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"shop","uid":"1","annotations":{"kubectl.kubernetes.io/last-applied-configuration":"{}"}},"spec":{"replicas":3,"strategy":{},"template":{"spec":{"containers":[{"name":"web","image":"nginx:1.27","imagePullPolicy":"IfNotPresent"}]}}},"status":{"readyReplicas":3}}"#;
        let got = actual(live, &found[0]).unwrap();
        assert_eq!(
            got.get("spec").unwrap().canonical(),
            r#"{"replicas":3,"template":{"spec":{"containers":[{"image":"nginx:1.27","name":"web"}]}}}"#
        );
        assert_eq!(
            got.get("metadata").unwrap().canonical(),
            r#"{"name":"web","namespace":"shop"}"#
        );
        assert!(!got.contains_key("status"));

        let secret = r#"{"kind":"Secret","metadata":{"name":"creds"},"data":{"token":"c2VjcmV0"}}"#;
        let got = actual(secret, &found[1]).unwrap();
        assert_eq!(
            got.get("data").unwrap().canonical(),
            r#"{"token":"(sensitive)"}"#
        );
    }
}
