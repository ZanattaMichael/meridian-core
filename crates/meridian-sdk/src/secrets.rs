//! The secrets a run hands a plugin, and how a plugin passes them on.
//!
//! A secret reaches a plugin twice, in two shapes. When the document is
//! compiled, a param that holds one arrives as [`Value::Secret`], and the
//! graph's [`ResourceGraph::secrets`](crate::ResourceGraph::secrets) names
//! it: the emitter writes a reference to the secret into the artifact, such
//! as a variable called after it, and never a value, because it has none.
//! When the artifact is run, [`RunContext::secrets`](crate::RunContext::secrets)
//! holds the value of each, in protected memory, under the same name and
//! slot, and the runner hands it to the tool by a private channel: the tool's
//! standard input ([`Tool::stdin_secret`](crate::tool::Tool::stdin_secret)),
//! or a file only the user can read ([`Secrets::json_file`]). Never on the
//! command line, where every user of the machine can read it, and never in
//! the environment, which a tool passes on to everything it starts.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use meridian_secret::{Part, SecretFile, SecretValue};
use meridian_value::{SecretRef, Value};

/// One secret a graph's params refer to: which slot stands for it in a
/// [`Value::Secret`], and the name the document declared it under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretName {
    pub secret: SecretRef,
    pub name: String,
}

#[derive(Default)]
struct Held {
    /// Each secret by name, in protected memory.
    values: BTreeMap<String, SecretValue>,
    /// The name each slot stands for.
    names: BTreeMap<SecretRef, String>,
}

/// The values of a run's secrets, as a plugin's runner receives them.
///
/// It is cheap to clone and compare, and neither reaches a value: a clone
/// shares the one copy, `Debug` lists the names, and two are equal when they
/// hold the same names. The values themselves are reached through
/// [`Secrets::get`] and [`Secrets::of`], each a [`SecretValue`] that cannot
/// be printed or copied by accident, and they are wiped when the last clone
/// is dropped: at the end of the call that received them.
#[derive(Clone, Default)]
pub struct Secrets {
    held: Arc<Held>,
}

impl Secrets {
    /// No secrets.
    pub fn none() -> Secrets {
        Secrets::default()
    }

    /// Secrets handed over one by one, as the host gathers them for a run.
    /// A name given twice keeps its last value.
    pub fn from_values(values: Vec<(SecretRef, String, SecretValue)>) -> Secrets {
        let mut held = Held::default();
        for (secret, name, value) in values {
            held.names.insert(secret, name.clone());
            held.values.insert(name, value);
        }
        Secrets {
            held: Arc::new(held),
        }
    }

    /// Whether there is nothing to hand to the tool.
    pub fn is_empty(&self) -> bool {
        self.held.values.is_empty()
    }

    /// How many secrets there are.
    pub fn len(&self) -> usize {
        self.held.values.len()
    }

    /// Every secret's name, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.held.values.keys().map(String::as_str).collect()
    }

    /// The value of the secret called `name`.
    pub fn get(&self, name: &str) -> Option<&SecretValue> {
        self.held.values.get(name)
    }

    /// The value of the secret a [`Value::Secret`] in the graph stood for.
    pub fn of(&self, secret: SecretRef) -> Option<&SecretValue> {
        self.held
            .names
            .get(&secret)
            .and_then(|name| self.held.values.get(name))
    }

    /// The name a [`Value::Secret`] in the graph stood for.
    pub fn name_of(&self, secret: SecretRef) -> Option<&str> {
        self.held.names.get(&secret).map(String::as_str)
    }

    /// Each secret by name and value, sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SecretValue)> {
        self.held
            .values
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Each slot with its name, for the host to send.
    pub(crate) fn slots(&self) -> impl Iterator<Item = (SecretRef, &str, &SecretValue)> {
        self.held.names.iter().filter_map(|(secret, name)| {
            self.held
                .values
                .get(name)
                .map(|value| (*secret, name.as_str(), value))
        })
    }

    /// Writes every secret into one private file as a JSON object, each
    /// under the key `key` gives its name, for a tool that reads its
    /// variables from a file: `{"db_password": "..."}`. The file is held in
    /// memory where the system allows, readable by the user alone, and
    /// overwritten and removed when the returned value is dropped, so keep
    /// it until the tool has exited.
    ///
    /// `file_name` is the file's own name, not a path. A secret that is not
    /// text cannot be a JSON string and is refused.
    pub fn json_file(
        &self,
        file_name: &str,
        key: impl Fn(&str) -> String,
    ) -> std::io::Result<SecretFile> {
        let keys: Vec<String> = self
            .held
            .values
            .keys()
            .map(|name| Value::String(key(name)).canonical())
            .collect();
        let mut parts = vec![Part::Text("{")];
        for (i, (key, value)) in keys.iter().zip(self.held.values.values()).enumerate() {
            if i > 0 {
                parts.push(Part::Text(","));
            }
            parts.push(Part::Text(key));
            parts.push(Part::Text(":\""));
            parts.push(Part::JsonString(value));
            parts.push(Part::Text("\""));
        }
        parts.push(Part::Text("}\n"));
        SecretFile::create(file_name, &parts)
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}

impl PartialEq for Secrets {
    fn eq(&self, other: &Secrets) -> bool {
        self.names() == other.names()
    }
}

/// Replaces every [`Value::Secret`] in `value` with the text `write` gives
/// for its name. A slot the graph does not name is written as its mask.
pub(crate) fn written_as(
    value: &Value,
    names: &[SecretName],
    write: &dyn Fn(&str) -> String,
) -> Value {
    match value {
        Value::Secret(at) => match names.iter().find(|n| n.secret == *at) {
            Some(named) => Value::String(write(&named.name)),
            None => Value::String(meridian_value::SENSITIVE.to_owned()),
        },
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| written_as(item, names, write))
                .collect(),
        ),
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), written_as(item, names, write)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(text: &str) -> SecretValue {
        SecretValue::from_reader(text.as_bytes(), 1024).unwrap()
    }

    #[test]
    fn secrets_are_found_by_name_and_by_slot_and_never_printed() {
        let secrets = Secrets::from_values(vec![
            (SecretRef::new(3), "db".to_owned(), secret("hunter2")),
            (SecretRef::new(5), "api".to_owned(), secret("k-1")),
        ]);
        assert_eq!(secrets.len(), 2);
        assert_eq!(secrets.names(), ["api", "db"]);
        assert!(secrets.get("db").unwrap().same(&secret("hunter2")));
        assert!(secrets.of(SecretRef::new(5)).unwrap().same(&secret("k-1")));
        assert_eq!(secrets.name_of(SecretRef::new(3)), Some("db"));
        assert!(secrets.of(SecretRef::new(9)).is_none());
        let shown = format!("{secrets:?}");
        assert_eq!(shown, "[\"api\", \"db\"]");
        assert!(!shown.contains("hunter2"));
        assert_eq!(secrets.clone(), secrets);
        assert_ne!(secrets, Secrets::none());
    }

    #[test]
    fn a_json_file_holds_every_secret_escaped_under_its_key() {
        let secrets = Secrets::from_values(vec![
            (SecretRef::new(0), "db".to_owned(), secret("a\"b\\c\nd")),
            (SecretRef::new(1), "api".to_owned(), secret("plain")),
        ]);
        let file = secrets
            .json_file("vars.json", |name| format!("meridian_secret_{name}"))
            .unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert_eq!(
            text,
            "{\"meridian_secret_api\":\"plain\",\"meridian_secret_db\":\"a\\\"b\\\\c\\nd\"}\n"
        );
        let path = file.path().to_path_buf();
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn no_secrets_make_an_empty_object() {
        let file = Secrets::none()
            .json_file("vars.json", str::to_owned)
            .unwrap();
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), "{}\n");
    }

    #[test]
    fn a_secret_that_is_not_text_is_refused_as_json() {
        let value = SecretValue::from_reader(&[0xffu8, 0xfe][..], 8).unwrap();
        let secrets = Secrets::from_values(vec![(SecretRef::new(0), "raw".to_owned(), value)]);
        let err = secrets.json_file("vars.json", str::to_owned).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn written_as_replaces_named_secrets_wherever_they_are() {
        let names = vec![SecretName {
            secret: SecretRef::new(2),
            name: "db".to_owned(),
        }];
        let value = Value::Map(BTreeMap::from([
            ("password".to_owned(), Value::Secret(SecretRef::new(2))),
            (
                "list".to_owned(),
                Value::List(vec![Value::Secret(SecretRef::new(7)), Value::Int(1)]),
            ),
        ]));
        let written = written_as(&value, &names, &|name| format!("{{{{ {name} }}}}"));
        assert_eq!(
            written.canonical(),
            "{\"list\":[\"(sensitive)\",1],\"password\":\"{{ db }}\"}"
        );
    }
}
