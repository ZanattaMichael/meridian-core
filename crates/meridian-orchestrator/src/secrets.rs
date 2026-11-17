//! Fetching a stage's secrets, just before the stage needs them, and what a
//! stage may do with them afterwards.

use std::collections::BTreeMap;

use meridian_ir::{Document, Secrets};
use meridian_sdk::Value;
use meridian_secret::{FetchError, Probe, SecretTable};

/// Fetches every secret the document declares into `secrets`.
///
/// An error names the secret and its provider and says what the provider
/// said, which is never any part of a value. An optional secret that is not
/// found reads as null.
pub(crate) fn fetch(doc: &Document, secrets: &mut Secrets) -> Result<(), String> {
    for secret in doc.secrets() {
        match secret.source().fetch() {
            Ok(value) => secrets.hold(&secret.name, value),
            Err(FetchError::Missing(_)) if secret.optional => secrets.absent(&secret.name),
            Err(e) => {
                return Err(format!(
                    "secret {:?} (from: {}): {e}",
                    secret.name, secret.from
                ))
            }
        }
    }
    Ok(())
}

/// Where the first secret in a document's params is: the resource and the
/// param, as a reader would name them.
pub(crate) fn first_in_params(doc: &Document) -> Option<(String, String)> {
    doc.resources().iter().find_map(|r| {
        r.params.iter().find_map(|(name, value)| {
            path_to_secret(value, &format!("params.{name}")).map(|at| (r.id.clone(), at))
        })
    })
}

fn path_to_secret(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Secret(_) => Some(path.to_owned()),
        Value::List(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, item)| path_to_secret(item, &format!("{path}[{i}]"))),
        Value::Map(entries) => entries
            .iter()
            .find_map(|(key, item)| path_to_secret(item, &format!("{path}.{key}"))),
        _ => None,
    }
}

/// Writes each secret in a document's params as what stands for it: the name
/// of one that is not fetched, as `(secret name)`, with the text around it.
/// This is what `compile` writes, since it never fetches a secret.
pub(crate) fn describe_in_params(doc: &mut Document, table: &SecretTable) {
    let resources = match doc {
        Document::Infrastructure(d) => &mut d.spec.resources,
        Document::ResourceSet(d) => &mut d.spec.resources,
    };
    for r in resources {
        let params = std::mem::take(&mut r.params);
        r.params = params
            .into_iter()
            .map(|(name, value)| (name, described(value, table)))
            .collect::<BTreeMap<_, _>>();
    }
}

fn described(value: Value, table: &SecretTable) -> Value {
    match value {
        Value::Secret(at) => Value::String(table.describe(at)),
        Value::List(items) => Value::List(items.into_iter().map(|v| described(v, table)).collect()),
        Value::Map(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k, described(v, table)))
                .collect(),
        ),
        other => other,
    }
}

/// What to do about memory that cannot be locked.
pub(crate) const RAISE_THE_LIMIT: &str = "Raise the limit on locked memory (ulimit -l, or \
                                          LimitMEMLOCK= for a service).";

/// The secrets `names`, as a sentence starts.
pub(crate) fn named(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    match quoted.as_slice() {
        [one] => format!("the secret {one}"),
        many => format!("the secrets {}", many.join(", ")),
    }
}

/// One declaration of `spec.secrets`, and whether the place it names would
/// answer. It holds what the document wrote and what a probe found: a probe
/// reads no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared {
    /// The name the document reads it by.
    pub name: String,
    /// Its provider.
    pub from: String,
    /// Whether nothing being there is allowed.
    pub optional: bool,
    /// What looking at the place found.
    pub probe: Probe,
}

impl Declared {
    /// Whether `plan` or `apply` would stop at this declaration.
    pub fn unanswered(&self) -> bool {
        match self.probe {
            Probe::Present(_) | Probe::Unknown(_) => false,
            Probe::Absent(_) => !self.optional,
            Probe::Refused(_) => true,
        }
    }
}

/// Looks at where each declaration of the document says its value comes
/// from, without fetching: a file is not opened and a command is not run.
pub fn check_declared(doc: &Document) -> Vec<Declared> {
    doc.secrets()
        .iter()
        .map(|entry| Declared {
            name: entry.name.clone(),
            from: entry.from.clone(),
            optional: entry.optional,
            probe: entry.source().probe(),
        })
        .collect()
}
