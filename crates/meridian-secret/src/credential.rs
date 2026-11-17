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

/// The credential types that can be resolved outside a run: the ones a
/// project's manifest may declare. The language takes every type of
/// [`credential_types`].
pub const CREDENTIAL_TYPES: &[&str] = &["registry", "token", "usernamePassword"];

/// A credential type: what it is made of, and what it is for.
///
/// A type is a fixed list of fields, so a misspelt one is refused when the
/// document is validated and not an authentication failure an hour later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialType {
    /// The name a declaration gives as `type`.
    pub name: &'static str,
    /// The fields that are not secret: a user name, a region, a server. They
    /// reach a tool in the open, on its command line or in its inventory.
    pub plain: &'static [&'static str],
    /// The fields that hold secret material. Each must be a secret: a
    /// literal there is refused.
    pub secret: &'static [&'static str],
    /// The fields that are a yes or a no.
    pub flags: &'static [&'static str],
    /// The ways to complete the credential: at least one of these sets of
    /// fields must be given, unless the credential is `from: ambient` and
    /// the tool finds its own.
    pub one_of: &'static [&'static [&'static str]],
    /// Whether a project may declare it outside a run, in its manifest.
    pub outside_run: bool,
    /// What it is used for.
    pub used_for: &'static str,
}

impl CredentialType {
    /// Every field of the type, in name order.
    pub fn fields(&self) -> Vec<&'static str> {
        let mut all: Vec<&str> = self
            .plain
            .iter()
            .chain(self.secret)
            .chain(self.flags)
            .copied()
            .collect();
        all.sort_unstable();
        all
    }

    /// Whether `field` is one of the type's fields.
    pub fn has(&self, field: &str) -> bool {
        self.plain.contains(&field) || self.secret.contains(&field) || self.flags.contains(&field)
    }

    /// Whether `field` holds secret material.
    pub fn is_secret(&self, field: &str) -> bool {
        self.secret.contains(&field)
    }

    /// Whether `field` is a yes or a no.
    pub fn is_flag(&self, field: &str) -> bool {
        self.flags.contains(&field)
    }

    /// Whether the fields `given` complete the credential: one of the
    /// type's ways is wholly there.
    pub fn complete(&self, given: impl Fn(&str) -> bool) -> bool {
        self.one_of
            .iter()
            .any(|way| way.iter().all(|field| given(field)))
    }

    /// The ways to complete the credential, as a sentence lists them:
    /// `privateKey, or password, or agent: true`.
    pub fn needs(&self) -> String {
        let ways: Vec<String> = self
            .one_of
            .iter()
            .map(|way| {
                way.iter()
                    .map(|field| {
                        if self.is_flag(field) {
                            format!("{field}: true")
                        } else {
                            (*field).to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" and ")
            })
            .collect();
        ways.join(", or ")
    }
}

const TYPES: &[CredentialType] = &[
    CredentialType {
        name: "aws",
        plain: &["accessKeyId", "profile", "region", "roleArn"],
        secret: &["secretAccessKey", "sessionToken"],
        flags: &[],
        one_of: &[
            &["accessKeyId", "secretAccessKey"],
            &["profile"],
            &["roleArn"],
        ],
        outside_run: false,
        used_for: "cloud endpoints on AWS, and AWS secret providers",
    },
    CredentialType {
        name: "azure",
        plain: &["clientId", "subscriptionId", "tenantId"],
        secret: &["certificate", "clientSecret"],
        flags: &["managedIdentity"],
        one_of: &[
            &["clientId", "clientSecret"],
            &["clientId", "certificate"],
            &["managedIdentity"],
        ],
        outside_run: false,
        used_for: "cloud endpoints on Azure, and Key Vault",
    },
    CredentialType {
        name: "certificate",
        plain: &["ca", "certificate"],
        secret: &["privateKey"],
        flags: &[],
        one_of: &[&["certificate", "privateKey"]],
        outside_run: false,
        used_for: "mutual TLS, and DSC",
    },
    CredentialType {
        name: "gcp",
        plain: &["impersonate", "project"],
        secret: &["serviceAccountKey"],
        flags: &[],
        one_of: &[&["serviceAccountKey"], &["impersonate"]],
        outside_run: false,
        used_for: "cloud endpoints on Google Cloud, and Secret Manager",
    },
    CredentialType {
        name: "kubeconfig",
        plain: &["ca", "context", "server"],
        secret: &["kubeconfig", "token"],
        flags: &[],
        one_of: &[&["kubeconfig"], &["server", "token"]],
        outside_run: false,
        used_for: "cluster endpoints",
    },
    CredentialType {
        name: "registry",
        plain: &["server", "user"],
        secret: &["password"],
        flags: &[],
        one_of: &[&["password"]],
        outside_run: true,
        used_for: "daemon endpoints, image pulls, and module registries",
    },
    CredentialType {
        name: "ssh",
        plain: &["certificate", "user"],
        secret: &["passphrase", "password", "privateKey"],
        flags: &["agent"],
        one_of: &[&["privateKey"], &["password"], &["agent"]],
        outside_run: false,
        used_for: "host endpoints over SSH",
    },
    CredentialType {
        name: "token",
        plain: &["scheme"],
        secret: &["token"],
        flags: &[],
        one_of: &[&["token"]],
        outside_run: true,
        used_for: "APIs, notification sinks, and secret stores",
    },
    CredentialType {
        name: "usernamePassword",
        plain: &["domain", "user"],
        secret: &["password"],
        flags: &[],
        one_of: &[&["password"]],
        outside_run: true,
        used_for: "WinRM hosts, registries, and basic authentication",
    },
];

/// Every credential type the language knows, in name order.
pub fn credential_types() -> &'static [CredentialType] {
    TYPES
}

/// The credential type called `name`, if there is one.
pub fn credential_type(name: &str) -> Option<&'static CredentialType> {
    TYPES.iter().find(|t| t.name == name)
}

/// What a manifest's credential type is made of: its plain fields and its
/// secret fields. Only a type that can be resolved outside a run.
fn shape(kind: &str) -> Option<&'static CredentialType> {
    credential_type(kind).filter(|t| t.outside_run)
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
        let Some(shape) = shape(&self.kind) else {
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
            let is_plain = !shape.is_secret(field) && shape.has(field);
            let is_secret = shape.is_secret(field);
            match value {
                _ if !is_plain && !is_secret => found.push((
                    field.clone(),
                    format!(
                        "{field} is not a field of a {} credential; its fields are {}",
                        self.kind,
                        shape.fields().join(", ")
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
        if !shape.complete(|field| self.fields.contains_key(field)) {
            let needed = shape.one_of[0][0];
            found.push((
                needed.to_owned(),
                format!("a {} credential needs {}", self.kind, shape.needs()),
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
