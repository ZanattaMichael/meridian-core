//! A credential outside a run: an identity a project declares by name, for
//! work that needs one when nothing is being compiled, such as fetching a
//! module from a private registry.
//!
//! A credential is a type and its fields. A field that holds secret material
//! is never written out: it says where the secret comes from, as a
//! [`Source`], and is fetched into protected memory at the moment the
//! credential is resolved and wiped when the caller drops it.

use std::collections::BTreeMap;
use std::fmt;

use crate::{FetchError, SecretValue, Source};

/// The credential types that can be resolved outside a run.
pub const CREDENTIAL_TYPES: &[&str] = &["registry", "token", "usernamePassword"];

/// What a credential type is made of: its plain fields, and its fields that
/// hold secret material. The first secret field is the one it cannot do
/// without.
fn shape(kind: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match kind {
        "registry" => Some((&["server", "user"], &["password"])),
        "usernamePassword" => Some((&["user", "domain"], &["password"])),
        "token" => Some((&["scheme"], &["token"])),
        _ => None,
    }
}

/// One field of a declared credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldSource {
    /// A value that is not secret, as written: a user name, a server.
    Text(String),
    /// Where a secret value comes from.
    Secret(Source),
}

/// A credential as a project declares it. It holds nothing of any secret.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialDeclaration {
    /// The name it is used by.
    pub name: String,
    /// Its type: one of [`CREDENTIAL_TYPES`].
    pub kind: String,
    /// Its fields, by name.
    pub fields: BTreeMap<String, FieldSource>,
}

impl CredentialDeclaration {
    /// What is wrong with the declaration, as the field at fault and why.
    /// A secret written where a secret's source belongs is refused, and the
    /// message does not repeat what was written.
    pub fn problems(&self) -> Vec<(String, String)> {
        let mut found = Vec::new();
        if self.name.is_empty() {
            found.push((
                "name".to_owned(),
                "a credential needs a name; it is used by that name".to_owned(),
            ));
        }
        let Some((plain, secret)) = shape(&self.kind) else {
            let why = if self.kind.is_empty() {
                format!(
                    "a credential needs a type: one of {}",
                    CREDENTIAL_TYPES.join(", ")
                )
            } else {
                format!(
                    "{:?} is not a credential type this version has; type is one of {}",
                    self.kind,
                    CREDENTIAL_TYPES.join(", ")
                )
            };
            found.push(("type".to_owned(), why));
            return found;
        };
        for (field, value) in &self.fields {
            let is_plain = plain.contains(&field.as_str());
            let is_secret = secret.contains(&field.as_str());
            match value {
                _ if !is_plain && !is_secret => found.push((
                    field.clone(),
                    format!(
                        "{field} is not a field of a {} credential; its fields are {}",
                        self.kind,
                        plain
                            .iter()
                            .chain(secret)
                            .copied()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )),
                FieldSource::Text(_) if is_secret => found.push((
                    field.clone(),
                    format!(
                        "a {field} is written in the file; say where it comes from instead, \
                         as in {field}: {{from: env, key: ...}}"
                    ),
                )),
                FieldSource::Secret(_) if is_plain => found.push((
                    field.clone(),
                    format!("{field} is not secret; write its value"),
                )),
                FieldSource::Secret(source) => {
                    for (inner, why) in source.problems() {
                        found.push((format!("{field}.{inner}"), why));
                    }
                }
                FieldSource::Text(_) => {}
            }
        }
        let needed = secret[0];
        if !self.fields.contains_key(needed) {
            found.push((
                needed.to_owned(),
                format!("a {} credential needs {needed}", self.kind),
            ));
        }
        found
    }
}

/// One field of a resolved credential.
pub enum CredentialField {
    /// A value that is not secret.
    Text(String),
    /// A secret, in protected memory.
    Secret(SecretValue),
}

/// A resolved credential. Its secret fields are in protected memory and are
/// wiped when it is dropped; it prints as its name and type and nothing more.
pub struct Credential {
    /// The name it was declared by.
    pub name: String,
    /// Its type.
    pub kind: String,
    fields: BTreeMap<String, CredentialField>,
}

impl Credential {
    /// A field that is not secret, when the credential has it.
    pub fn text(&self, field: &str) -> Option<&str> {
        match self.fields.get(field) {
            Some(CredentialField::Text(text)) => Some(text),
            _ => None,
        }
    }

    /// A secret field, when the credential has it.
    pub fn secret(&self, field: &str) -> Option<&SecretValue> {
        match self.fields.get(field) {
            Some(CredentialField::Secret(secret)) => Some(secret),
            _ => None,
        }
    }

    /// The names of its fields.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.fields.keys().map(String::as_str)
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Credential({:?}, {})", self.name, self.kind)
    }
}

/// Why a credential could not be resolved. It never holds any part of a
/// secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialError {
    /// No credential of that name is declared. Holds the names that are.
    Undeclared(Vec<String>),
    /// The declaration is wrong: the field at fault and why.
    Invalid(String, String),
    /// A secret field could not be fetched: the field, its provider and why.
    Fetch(String, String, FetchError),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialError::Undeclared(names) if names.is_empty() => {
                f.write_str("it is not declared (no credential is)")
            }
            CredentialError::Undeclared(names) => {
                write!(f, "it is not declared (declared: {})", names.join(", "))
            }
            CredentialError::Invalid(field, why) => write!(f, "{field}: {why}"),
            CredentialError::Fetch(field, from, why) => {
                write!(f, "{field} (from: {from}): {why}")
            }
        }
    }
}

impl std::error::Error for CredentialError {}

/// Resolves the credential `name` of those a project declares: checks it,
/// and fetches each of its secret fields into protected memory.
///
/// Nothing is fetched for any other credential, nothing is cached, and the
/// secrets are wiped when the returned value is dropped. An error says which
/// field and which provider, never anything a provider read.
pub fn resolve_credential(
    project: &[CredentialDeclaration],
    name: &str,
) -> Result<Credential, CredentialError> {
    let Some(declared) = project.iter().find(|c| c.name == name) else {
        return Err(CredentialError::Undeclared(
            project.iter().map(|c| c.name.clone()).collect(),
        ));
    };
    if let Some((field, why)) = declared.problems().into_iter().next() {
        return Err(CredentialError::Invalid(field, why));
    }
    let mut fields = BTreeMap::new();
    for (field, value) in &declared.fields {
        let resolved = match value {
            FieldSource::Text(text) => CredentialField::Text(text.clone()),
            FieldSource::Secret(source) => CredentialField::Secret(
                source
                    .fetch()
                    .map_err(|e| CredentialError::Fetch(field.clone(), source.from.clone(), e))?,
            ),
        };
        fields.insert(field.clone(), resolved);
    }
    Ok(Credential {
        name: declared.name.clone(),
        kind: declared.kind.clone(),
        fields,
    })
}
