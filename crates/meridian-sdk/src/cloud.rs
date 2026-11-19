//! A cloud credential handed to a tool in the tool's own environment.
//!
//! An infrastructure tool reaches its cloud through the provider's SDK, and
//! the SDKs take a login from the environment, from their own files under
//! the user's home directory, or from the machine's metadata service.
//! Meridian holds a credential in protected memory and writes nothing under
//! the user's home, so the environment is the way: rank 4 of the plan's
//! channels, the last one it allows, taken only because the providers take
//! a value no other way. The plan line of a stage that goes this way says
//! so, and the plugin's page lists the variables.
//!
//! What each type becomes, for every tool that takes it:
//!
//! | Type | Plain fields | Secret fields |
//! |---|---|---|
//! | `aws` | `accessKeyId` as `AWS_ACCESS_KEY_ID`, `profile` as `AWS_PROFILE`, `region` as `AWS_REGION` and `AWS_DEFAULT_REGION` | `secretAccessKey` as `AWS_SECRET_ACCESS_KEY`, `sessionToken` as `AWS_SESSION_TOKEN` |
//! | `azure` | `clientId` as `ARM_CLIENT_ID`, `tenantId` as `ARM_TENANT_ID`, `subscriptionId` as `ARM_SUBSCRIPTION_ID`, `managedIdentity: true` as `ARM_USE_MSI=true` | `clientSecret` as `ARM_CLIENT_SECRET`; `certificate` written to a file only the user can read, named in `ARM_CLIENT_CERTIFICATE_PATH` |
//! | `gcp` | `project` as `GOOGLE_PROJECT` and `CLOUDSDK_CORE_PROJECT`, `impersonate` as `GOOGLE_IMPERSONATE_SERVICE_ACCOUNT` | `serviceAccountKey` written to a file only the user can read, named in `GOOGLE_APPLICATION_CREDENTIALS` |
//!
//! A file is rank 2: it is made by `meridian-secret` in a directory only
//! the user can enter, and removed when the [`CloudHandover`] is dropped, at
//! the end of the stage. A `roleArn` is refused: the SDKs assume a role
//! through a profile that names it, which the credential can name instead.

use std::fmt;
use std::sync::Arc;

use meridian_secret::{Part, SecretFile, SecretValue};

use crate::credential::{Credential, HeldCredential};
use crate::tool::Tool;
use crate::RunError;

/// How a tool takes a cloud credential: its name, which clouds it reaches,
/// and what of a credential it has no way to take, with the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudTakes {
    /// The program, as a refusal names it: `terraform`, `aws cloudformation`.
    pub tool: &'static str,
    /// The credential types the tool reaches a cloud as: `aws`, `azure`,
    /// `gcp`, in that order where it takes more than one.
    pub providers: &'static [&'static str],
    /// `Ok` when an Azure `certificate` reaches the tool through
    /// `ARM_CLIENT_CERTIFICATE_PATH`; `Err` says why it cannot.
    pub azure_certificate: Result<(), &'static str>,
    /// `Ok` when a Google account to `impersonate` reaches the tool through
    /// `GOOGLE_IMPERSONATE_SERVICE_ACCOUNT`; `Err` says why it cannot.
    pub gcp_impersonation: Result<(), &'static str>,
}

/// Credential types as a sentence names them: `an aws, azure or gcp`.
fn describe(types: &[&str]) -> String {
    let list = match types {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [all @ .., last] => format!("{} or {last}", all.join(", ")),
    };
    let article = if types
        .first()
        .is_some_and(|t| t.starts_with(['a', 'e', 'i', 'o']))
    {
        "an"
    } else {
        "a"
    };
    format!("{article} {list}")
}

/// Checks a credential against what a tool can take of a cloud login, at
/// compile, so that a document is refused before anything is fetched. The
/// message of a refusal says what to do instead.
pub fn check_cloud_credential(credential: &Credential, takes: &CloudTakes) -> Result<(), String> {
    let tool = takes.tool;
    let kind = credential.kind.as_str();
    if !takes.providers.contains(&kind) {
        let where_ = if takes.providers.len() == 1 {
            format!("reaches {} only", cloud_name(takes.providers[0]))
        } else {
            "reaches a cloud".to_owned()
        };
        return Err(format!(
            "{tool} {where_}, as {} credential, not {kind}",
            describe(takes.providers)
        ));
    }
    for (field, value) in &credential.text {
        if value.chars().any(char::is_control) {
            return Err(format!(
                "the {field} cannot be given to {tool}: it holds a line break or a control \
                 character, which an environment variable cannot carry to a provider"
            ));
        }
    }
    match kind {
        "aws" => {
            if credential.text("roleArn").is_some() {
                return Err(format!(
                    "{tool} takes no roleArn from Meridian: the AWS SDK assumes a role \
                     through a profile that names it (role_arn and source_profile in the \
                     AWS configuration file); declare that profile instead"
                ));
            }
        }
        "azure" => {
            if let (true, Err(why)) = (
                credential.has_secret("certificate"),
                takes.azure_certificate,
            ) {
                return Err(format!(
                    "{tool} takes no certificate for an Azure login: {why}; give the \
                     credential a clientSecret, or set managedIdentity: true"
                ));
            }
        }
        "gcp" => {
            if let (true, Err(why)) = (
                credential.text("impersonate").is_some(),
                takes.gcp_impersonation,
            ) {
                return Err(format!(
                    "{tool} takes no account to impersonate from Meridian: {why}; give \
                     the credential a serviceAccountKey"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn cloud_name(provider: &str) -> &'static str {
    match provider {
        "aws" => "AWS",
        "azure" => "Azure",
        "gcp" => "Google Cloud",
        _ => "its cloud",
    }
}

/// A cloud credential prepared for the tools of one stage: the variables
/// each tool is told, and the files a provider reads, which live as long as
/// this does. Nothing of a secret is in it but the [`HeldCredential`] it
/// was made from.
pub struct CloudHandover {
    credential: HeldCredential,
    /// Variables with a value that is not secret.
    plain: Vec<(String, String)>,
    /// Variables that carry the value of a secret field: one copy of each,
    /// in protected memory, shared by every call of the stage.
    secret: Vec<(String, Arc<SecretValue>)>,
    /// Files a provider reads, kept until the handover is dropped. The
    /// variable naming each is in `plain`.
    files: Vec<SecretFile>,
}

impl CloudHandover {
    fn prepare(credential: &HeldCredential) -> Result<CloudHandover, RunError> {
        let mut handover = CloudHandover {
            credential: credential.clone(),
            plain: Vec::new(),
            secret: Vec::new(),
            files: Vec::new(),
        };
        let mut plain = |field: &str, variables: &[&str]| {
            if let Some(value) = credential.text(field) {
                for variable in variables {
                    handover
                        .plain
                        .push(((*variable).to_owned(), value.to_owned()));
                }
            }
        };
        match credential.kind() {
            "aws" => {
                plain("accessKeyId", &["AWS_ACCESS_KEY_ID"]);
                plain("profile", &["AWS_PROFILE"]);
                plain("region", &["AWS_REGION", "AWS_DEFAULT_REGION"]);
                handover.secret_variable("secretAccessKey", "AWS_SECRET_ACCESS_KEY")?;
                handover.secret_variable("sessionToken", "AWS_SESSION_TOKEN")?;
            }
            "azure" => {
                plain("clientId", &["ARM_CLIENT_ID"]);
                plain("tenantId", &["ARM_TENANT_ID"]);
                plain("subscriptionId", &["ARM_SUBSCRIPTION_ID"]);
                if credential.flag("managedIdentity") {
                    handover
                        .plain
                        .push(("ARM_USE_MSI".to_owned(), "true".to_owned()));
                }
                handover.secret_variable("clientSecret", "ARM_CLIENT_SECRET")?;
                handover.secret_file(
                    "certificate",
                    "azure-certificate.pem",
                    "ARM_CLIENT_CERTIFICATE_PATH",
                )?;
            }
            "gcp" => {
                plain("project", &["GOOGLE_PROJECT", "CLOUDSDK_CORE_PROJECT"]);
                plain("impersonate", &["GOOGLE_IMPERSONATE_SERVICE_ACCOUNT"]);
                handover.secret_file(
                    "serviceAccountKey",
                    "gcp-service-account.json",
                    "GOOGLE_APPLICATION_CREDENTIALS",
                )?;
            }
            other => {
                return Err(RunError::new(format!(
                    "the credential {:?} is {other}, which reaches no cloud; the document \
                     should have been refused at compile",
                    credential.name()
                )))
            }
        }
        Ok(handover)
    }

    /// Keeps a copy of the secret field `field` for `variable`, in
    /// protected memory of its own, which every call of the stage shares.
    fn secret_variable(&mut self, field: &str, variable: &str) -> Result<(), RunError> {
        if let Some(value) = self.credential.secret(field) {
            let copy = value.duplicate().map_err(|e| {
                RunError::new(format!(
                    "copying the credential's {field} for the tool: {e}"
                ))
            })?;
            self.secret.push((variable.to_owned(), Arc::new(copy)));
        }
        Ok(())
    }

    /// Writes the secret field `field` to a file only the user can read,
    /// and names the file's path in `variable`.
    fn secret_file(&mut self, field: &str, name: &str, variable: &str) -> Result<(), RunError> {
        let Some(value) = self.credential.secret(field) else {
            return Ok(());
        };
        let file = SecretFile::create(name, &[Part::Secret(value)]).map_err(|e| {
            RunError::new(format!(
                "writing the credential's {field} to a private file: {e}"
            ))
        })?;
        self.plain
            .push((variable.to_owned(), file.path().display().to_string()));
        self.files.push(file);
        Ok(())
    }

    /// The credential's name, as the document declared it.
    pub fn name(&self) -> &str {
        self.credential.name()
    }

    /// The names of the variables the tool is told, sorted, for a log or a
    /// test; never a value.
    pub fn variables(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .plain
            .iter()
            .map(|(variable, _)| variable.clone())
            .chain(self.secret.iter().map(|(variable, _)| variable.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The names of the variables that carry a secret, sorted.
    pub fn secret_variables(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .secret
            .iter()
            .map(|(variable, _)| variable.clone())
            .collect();
        names.sort();
        names
    }

    /// The tool told the credential: each variable in its environment, a
    /// secret one written from protected memory when the tool is spawned.
    pub fn tell(&self, mut tool: Tool) -> Tool {
        for (variable, value) in &self.plain {
            tool = tool.env(variable, value);
        }
        for (variable, value) in &self.secret {
            tool = tool.env_secret_shared(variable, Arc::clone(value));
        }
        tool
    }

    /// `tool`, told the credential when the stage has one.
    pub fn tell_if_any(cloud: Option<&CloudHandover>, tool: Tool) -> Tool {
        match cloud {
            Some(cloud) => cloud.tell(tool),
            None => tool,
        }
    }
}

impl fmt::Debug for CloudHandover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CloudHandover({:?}, {}, variables {:?})",
            self.credential.name(),
            self.credential.kind(),
            self.variables()
        )
    }
}

/// The credential prepared for the stage's tools, when the document names
/// one for its cloud: the one-line hook for a plugin's runner. A credential
/// of another kind than `takes` lists is an error here; the compile check
/// ([`check_cloud_credential`]) refuses it first.
pub fn cloud_for(
    credential: Option<&HeldCredential>,
    takes: &CloudTakes,
) -> Result<Option<CloudHandover>, RunError> {
    let Some(credential) = credential else {
        return Ok(None);
    };
    check_cloud_credential(&credential.declared, takes)
        .map_err(|msg| RunError::new(format!("the credential {:?}: {msg}", credential.name())))?;
    CloudHandover::prepare(credential).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_secret::SecretValue;
    use std::collections::BTreeMap;

    const TAKES: CloudTakes = CloudTakes {
        tool: "terraform",
        providers: &["aws", "azure", "gcp"],
        azure_certificate: Ok(()),
        gcp_impersonation: Ok(()),
    };

    const AWS_ONLY: CloudTakes = CloudTakes {
        tool: "aws cloudformation",
        providers: &["aws"],
        azure_certificate: Err("unused"),
        gcp_impersonation: Err("unused"),
    };

    fn value(text: &str) -> SecretValue {
        SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
    }

    fn held(kind: &str, text: &[(&str, &str)], secrets: &[(&str, &str)]) -> HeldCredential {
        HeldCredential::from_values(
            Credential {
                name: "prod".into(),
                kind: kind.into(),
                text: text
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                ..Credential::default()
            },
            secrets
                .iter()
                .map(|(field, v)| ((*field).to_owned(), value(v)))
                .collect(),
        )
    }

    #[test]
    fn a_credential_of_another_kind_or_with_a_role_is_refused_with_the_reason() {
        let ssh = Credential {
            kind: "ssh".into(),
            ..Credential::default()
        };
        assert_eq!(
            check_cloud_credential(&ssh, &TAKES).unwrap_err(),
            "terraform reaches a cloud, as an aws, azure or gcp credential, not ssh"
        );
        let azure = Credential {
            kind: "azure".into(),
            ..Credential::default()
        };
        assert_eq!(
            check_cloud_credential(&azure, &AWS_ONLY).unwrap_err(),
            "aws cloudformation reaches AWS only, as an aws credential, not azure"
        );
        let role = Credential {
            kind: "aws".into(),
            text: BTreeMap::from([("roleArn".to_owned(), "arn:aws:iam::1:role/x".to_owned())]),
            ..Credential::default()
        };
        let why = check_cloud_credential(&role, &TAKES).unwrap_err();
        assert!(why.contains("takes no roleArn") && why.contains("declare that profile"));
        let broken = Credential {
            kind: "aws".into(),
            text: BTreeMap::from([("region".to_owned(), "eu\nwest".to_owned())]),
            ..Credential::default()
        };
        assert!(check_cloud_credential(&broken, &TAKES)
            .unwrap_err()
            .contains("the region cannot be given to terraform"));
        let certificate = Credential {
            kind: "azure".into(),
            secret_fields: vec!["certificate".into()],
            ..Credential::default()
        };
        let without = CloudTakes {
            azure_certificate: Err("the plugin reads its login from the template"),
            ..TAKES
        };
        assert!(check_cloud_credential(&certificate, &without)
            .unwrap_err()
            .contains("takes no certificate for an Azure login: the plugin reads"));
        assert_eq!(check_cloud_credential(&certificate, &TAKES), Ok(()));
    }

    #[test]
    fn an_aws_credential_becomes_the_sdks_variables_and_a_gcp_key_a_private_file() {
        let aws = held(
            "aws",
            &[("accessKeyId", "AKIA1"), ("region", "eu-west-1")],
            &[("secretAccessKey", "s3cr3t"), ("sessionToken", "t0k3n")],
        );
        let handover = cloud_for(Some(&aws), &TAKES).unwrap().unwrap();
        assert_eq!(
            handover.variables(),
            [
                "AWS_ACCESS_KEY_ID",
                "AWS_DEFAULT_REGION",
                "AWS_REGION",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN"
            ]
        );
        assert_eq!(
            handover.secret_variables(),
            ["AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"]
        );
        let tool = handover.tell(Tool::new("tool"));
        assert_eq!(
            tool.secret_env_names(),
            ["AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"]
        );
        let shown = format!("{handover:?}{tool:?}");
        assert!(!shown.contains("s3cr3t") && !shown.contains("t0k3n"));

        let gcp = held(
            "gcp",
            &[("project", "shop-prod")],
            &[("serviceAccountKey", "{\"type\": \"service_account\"}")],
        );
        let handover = cloud_for(Some(&gcp), &TAKES).unwrap().unwrap();
        assert_eq!(
            handover.variables(),
            [
                "CLOUDSDK_CORE_PROJECT",
                "GOOGLE_APPLICATION_CREDENTIALS",
                "GOOGLE_PROJECT"
            ]
        );
        assert!(handover.secret_variables().is_empty());
        let path = handover
            .plain
            .iter()
            .find(|(v, _)| v == "GOOGLE_APPLICATION_CREDENTIALS")
            .map(|(_, path)| std::path::PathBuf::from(path))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"type\": \"service_account\"}"
        );
        drop(handover);
        assert!(!path.exists(), "the key file outlives the handover");

        assert!(cloud_for(None, &TAKES).unwrap().is_none());
        let ssh = held("ssh", &[("user", "deploy")], &[]);
        assert!(cloud_for(Some(&ssh), &TAKES)
            .unwrap_err()
            .msg
            .contains("the credential \"prod\": terraform reaches a cloud"));
    }

    #[test]
    fn an_azure_credential_uses_the_arm_variables_and_managed_identity_is_a_flag() {
        let mut declared = Credential {
            name: "prod".into(),
            kind: "azure".into(),
            text: BTreeMap::from([
                ("clientId".to_owned(), "c1".to_owned()),
                ("tenantId".to_owned(), "t1".to_owned()),
                ("subscriptionId".to_owned(), "s1".to_owned()),
            ]),
            ..Credential::default()
        };
        let with_secret = HeldCredential::from_values(
            declared.clone(),
            vec![("clientSecret".to_owned(), value("sp-secret"))],
        );
        let handover = cloud_for(Some(&with_secret), &TAKES).unwrap().unwrap();
        assert_eq!(
            handover.variables(),
            [
                "ARM_CLIENT_ID",
                "ARM_CLIENT_SECRET",
                "ARM_SUBSCRIPTION_ID",
                "ARM_TENANT_ID"
            ]
        );
        assert_eq!(handover.secret_variables(), ["ARM_CLIENT_SECRET"]);

        declared.flags.insert("managedIdentity".to_owned(), true);
        let identity = HeldCredential::from_values(declared, Vec::new());
        let handover = cloud_for(Some(&identity), &TAKES).unwrap().unwrap();
        assert_eq!(
            handover.variables(),
            [
                "ARM_CLIENT_ID",
                "ARM_SUBSCRIPTION_ID",
                "ARM_TENANT_ID",
                "ARM_USE_MSI"
            ]
        );
        assert!(handover.secret_variables().is_empty());
    }
}
