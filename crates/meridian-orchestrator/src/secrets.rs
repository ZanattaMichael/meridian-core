//! Fetching a stage's secrets, just before the stage needs them, and what a
//! stage may do with them afterwards.

use std::collections::BTreeMap;

use meridian_ir::{Document, Secrets};
use meridian_sdk::{ResourceGraph, Value};
use meridian_secret::{FetchError, Probe, SecretTable};

/// Fetches every secret the document declares into `secrets`.
///
/// An error names the secret and its provider and says what the provider
/// said, which is never any part of a value. An optional secret that is not
/// found reads as null.
pub(crate) fn fetch(doc: &Document, secrets: &mut Secrets) -> Result<(), String> {
    for secret in doc.secrets() {
        let sources = secret.sources();
        match meridian_secret::fetch_first(&sources) {
            Ok((_, value)) => secrets.hold(&secret.name, value),
            Err(FetchError::Missing(_)) if secret.optional => secrets.absent(&secret.name),
            Err(e) => {
                let from: Vec<&str> = sources.iter().map(|s| s.from.as_str()).collect();
                return Err(format!(
                    "secret {:?} (from: {}): {e}",
                    secret.name,
                    from.join(", then ")
                ));
            }
        }
    }
    Ok(())
}

/// The value of every secret the graph's params refer to, each a copy in
/// protected memory of its own, for the tool. The plugin is handed them by
/// the names the graph gave them. An error names the secret and never a
/// value.
pub(crate) fn handed(
    graph: &ResourceGraph,
    table: &SecretTable,
) -> Result<meridian_sdk::Secrets, String> {
    let mut values = Vec::with_capacity(graph.secrets.len());
    for named in &graph.secrets {
        let value = table
            .with(named.secret, |held| held.duplicate())
            .map_err(|e| format!("secret {:?}: {e}", named.name))?
            .map_err(|e| format!("secret {:?}: {e}", named.name))?;
        values.push((named.secret, named.name.clone(), value));
    }
    Ok(meridian_sdk::Secrets::from_values(values))
}

/// The value of every declared secret that was fetched, by its declared
/// name, for a tool that is handed an artifact without the graph it was
/// compiled from: a rollback.
pub(crate) fn handed_by_name(secrets: &Secrets) -> Result<meridian_sdk::Secrets, String> {
    let mut values = Vec::new();
    for (name, at) in secrets.names() {
        let value = secrets
            .table()
            .with(at, |held| held.duplicate())
            .map_err(|e| format!("secret {name:?}: {e}"))?
            .map_err(|e| format!("secret {name:?}: {e}"))?;
        values.push((at, name, value));
    }
    Ok(meridian_sdk::Secrets::from_values(values))
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
    /// The provider of the place that would answer, or of the last place
    /// looked at when none would.
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
            // No place the secret names can be used on this machine, so
            // nothing can say it is merely absent.
            Probe::Refused(_) | Probe::Unavailable(_) => true,
        }
    }
}

/// Looks at where each declaration of the document says its value comes
/// from, without fetching: a file is not opened and a command is not run. A
/// declaration with a `fallback` is answered by the first place that would.
pub fn check_declared(doc: &Document) -> Vec<Declared> {
    doc.secrets()
        .iter()
        .map(|entry| {
            let sources = entry.sources();
            let (at, probe) = meridian_secret::probe_first(&sources);
            Declared {
                name: entry.name.clone(),
                from: sources[at].from.clone(),
                optional: entry.optional,
                probe,
            }
        })
        .collect()
}

/// Every place the secret `name` is looked for, in order, as the first
/// document that declares it says. `None` when no document declares it.
pub fn places_declared(documents: &[Document], name: &str) -> Option<Vec<meridian_secret::Source>> {
    documents
        .iter()
        .flat_map(|doc| doc.secrets())
        .find(|entry| entry.name == name)
        .map(|entry| entry.sources())
}

/// The names the documents declare, each once, in order.
pub fn names_declared(documents: &[Document]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in documents.iter().flat_map(|doc| doc.secrets()) {
        if !names.contains(&entry.name) {
            names.push(entry.name.clone());
        }
    }
    names
}
