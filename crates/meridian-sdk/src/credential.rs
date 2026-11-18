//! The credential a document names for its host, as a plugin receives it.
//!
//! A credential reaches a plugin twice, as a secret does. When the document
//! is compiled, [`ResourceGraph::credential`](crate::ResourceGraph::credential)
//! holds the [`Credential`]: its name, its type, its plain fields and the
//! names of its secret fields, so the emitter can write what the tool reads
//! in the open, such as the user into an inventory, and never a value,
//! because it has none. When the artifact is run,
//! [`RunContext::credential`](crate::RunContext::credential) holds the
//! [`HeldCredential`]: the same, with the value of each secret field in
//! protected memory, for the runner to hand to the tool by a private channel:
//! a file only the user can read, named on the command line by its path and
//! nothing more, or the tool's standard input. Never the value on the
//! command line, and never in the environment.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use meridian_secret::SecretValue;

/// An identity a document declares, as a compile sees it. It holds no
/// secret: only which fields hold one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credential {
    /// The name the document declared it under.
    pub name: String,
    /// Its type: `ssh`, `usernamePassword`, `token`, `certificate`, `aws`,
    /// `azure`, `gcp`, `kubeconfig` or `registry`.
    pub kind: String,
    /// Whether the tool finds the credential for itself (`from: ambient`).
    /// No secret field is given then.
    pub ambient: bool,
    /// The fields that are not secret, by name: a user, a region, a server.
    pub text: BTreeMap<String, String>,
    /// The fields that are a yes or a no, by name: `agent`,
    /// `managedIdentity`. One left out is no.
    pub flags: BTreeMap<String, bool>,
    /// The names of the fields that hold secret material, sorted.
    pub secret_fields: Vec<String>,
}

impl Credential {
    /// A field that is not secret, when the credential has it.
    pub fn text(&self, field: &str) -> Option<&str> {
        self.text.get(field).map(String::as_str)
    }

    /// A flag, which is no when it is left out.
    pub fn flag(&self, field: &str) -> bool {
        self.flags.get(field).copied().unwrap_or(false)
    }

    /// Whether `field` is one of the secret fields.
    pub fn has_secret(&self, field: &str) -> bool {
        self.secret_fields.iter().any(|f| f == field)
    }

    /// The credential in a few words, as a plan names it: `deploy (ssh,
    /// user deploy)`. Never any part of a secret.
    pub fn summary(&self) -> String {
        let mut what = self.kind.clone();
        if let Some(user) = self.text("user") {
            what.push_str(&format!(", user {user}"));
        }
        if self.ambient {
            what.push_str(", ambient");
        }
        format!("{} ({what})", self.name)
    }
}

#[derive(Default)]
struct Held {
    values: BTreeMap<String, SecretValue>,
}

/// A credential with the value of each of its secret fields, as a plugin's
/// runner receives it.
///
/// It is cheap to clone and compare, and neither reaches a value: a clone
/// shares the one copy, `Debug` prints the name and the type, and two are
/// equal when they describe the same credential. The values are reached
/// through [`HeldCredential::secret`], each a [`SecretValue`] that cannot be
/// printed or copied by accident, and they are wiped when the last clone is
/// dropped: at the end of the call that received them.
#[derive(Clone, Default)]
pub struct HeldCredential {
    /// What the compile saw of it.
    pub declared: Credential,
    held: Arc<Held>,
}

impl HeldCredential {
    /// A credential with its secret fields' values, as the host gathers
    /// them for a run. The declared secret fields are those given here.
    pub fn from_values(mut declared: Credential, values: Vec<(String, SecretValue)>) -> Self {
        let mut held = Held::default();
        for (field, value) in values {
            held.values.insert(field, value);
        }
        declared.secret_fields = held.values.keys().cloned().collect();
        HeldCredential {
            declared,
            held: Arc::new(held),
        }
    }

    /// The name the document declared it under.
    pub fn name(&self) -> &str {
        &self.declared.name
    }

    /// Its type.
    pub fn kind(&self) -> &str {
        &self.declared.kind
    }

    /// Whether the tool finds the credential for itself.
    pub fn ambient(&self) -> bool {
        self.declared.ambient
    }

    /// A field that is not secret, when the credential has it.
    pub fn text(&self, field: &str) -> Option<&str> {
        self.declared.text(field)
    }

    /// A flag, which is no when it is left out.
    pub fn flag(&self, field: &str) -> bool {
        self.declared.flag(field)
    }

    /// The value of the secret field `field`, when the credential has it.
    pub fn secret(&self, field: &str) -> Option<&SecretValue> {
        self.held.values.get(field)
    }

    /// Each secret field by name and value, sorted by name.
    pub fn secrets(&self) -> impl Iterator<Item = (&str, &SecretValue)> {
        self.held
            .values
            .iter()
            .map(|(field, value)| (field.as_str(), value))
    }

    /// The credential in a few words, never any part of a secret.
    pub fn summary(&self) -> String {
        self.declared.summary()
    }
}

impl fmt::Debug for HeldCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HeldCredential({:?}, {}, secrets {:?})",
            self.declared.name, self.declared.kind, self.declared.secret_fields
        )
    }
}

impl PartialEq for HeldCredential {
    fn eq(&self, other: &HeldCredential) -> bool {
        self.declared == other.declared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> SecretValue {
        SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
    }

    #[test]
    fn a_held_credential_reaches_its_values_and_prints_none() {
        let declared = Credential {
            name: "deploy".into(),
            kind: "ssh".into(),
            text: BTreeMap::from([("user".to_owned(), "deploy".to_owned())]),
            ..Credential::default()
        };
        let held = HeldCredential::from_values(
            declared,
            vec![("privateKey".to_owned(), value("-----KEY-----"))],
        );
        assert_eq!(held.text("user"), Some("deploy"));
        assert!(!held.flag("agent"));
        assert!(held
            .secret("privateKey")
            .unwrap()
            .same(&value("-----KEY-----")));
        assert!(held.secret("password").is_none());
        assert_eq!(held.declared.secret_fields, ["privateKey"]);
        assert_eq!(held.summary(), "deploy (ssh, user deploy)");
        let shown = format!("{held:?}");
        assert!(shown.contains("deploy") && shown.contains("privateKey"));
        assert!(!shown.contains("KEY"));
        assert_eq!(held.clone(), held);
    }
}

/// How a tool that reaches its host over SSH takes a credential: its name,
/// and whether a password can reach it by a private channel, with the reason
/// when it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SshTakes {
    /// The program, as a refusal names it: `ssh`, `salt-ssh`, `bolt`.
    pub tool: &'static str,
    /// `Ok` when a password reaches the tool privately; `Err` says why it
    /// cannot.
    pub password: Result<(), &'static str>,
}

/// Checks a credential against what an SSH tool can take: an `ssh` or
/// `usernamePassword` credential, a key (through an agent Meridian starts,
/// with its passphrase), a password where the tool takes one privately, and
/// never a certificate or a domain. The message of a refusal says what to
/// do instead.
pub fn check_ssh_credential(credential: &Credential, takes: &SshTakes) -> Result<(), String> {
    let tool = takes.tool;
    match credential.kind.as_str() {
        "ssh" => {}
        "usernamePassword" => {
            if let Err(why) = takes.password {
                return Err(format!(
                    "{tool} takes no password for a host: {why}; reach the host as an ssh \
                     credential with a privateKey"
                ));
            }
        }
        other => {
            return Err(format!(
                "{tool} reaches a host as an ssh or usernamePassword credential, not {other}"
            ))
        }
    }
    if credential.has_secret("password") {
        if let Err(why) = takes.password {
            return Err(format!(
                "{tool} takes no password for a host: {why}; give the credential a privateKey \
                 instead"
            ));
        }
    }
    if credential.text("certificate").is_some() {
        return Err(format!(
            "{tool} takes no certificate for a key from Meridian; put it beside the key in an \
             SSH agent of your own and set agent: true"
        ));
    }
    if credential.text("domain").is_some() {
        return Err(format!(
            "{tool} over SSH takes no domain; it is for WinRM hosts, which this plugin does \
             not reach"
        ));
    }
    if let Some(user) = credential.text("user") {
        if user.is_empty() || user.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!(
                "the user {user:?} cannot be given to {tool}: a user name holds no space or \
                 line break"
            ));
        }
    }
    Ok(())
}

/// The error for a credential named by a document whose host is the
/// machine Meridian runs on, which a tool configures as the user running it.
pub fn credential_for_this_machine(tool: &str, credential: &HeldCredential) -> crate::RunError {
    crate::RunError::new(format!(
        "the document names the credential {:?} for its host, and the host is this machine, \
         which {tool} configures as the user running Meridian; leave the credential out, or \
         name another host",
        credential.name()
    ))
}
