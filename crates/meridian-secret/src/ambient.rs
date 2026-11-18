//! Whether a credential declared `from: ambient` would be found: the tool
//! finds such a credential for itself, in its environment, its profile
//! files or the machine's identity endpoint, and the check looks in the
//! same places, cheaply, without fetching a token.
//!
//! The order is each cloud's own default chain, as far as it can be looked
//! at without a call that costs anything: the environment first, then the
//! files a login leaves behind, and last the instance metadata service,
//! which is asked one question with a short timeout. No value is read: a
//! file is looked for and not opened, and a metadata answer is its status,
//! not its body.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Probe;

/// How long the metadata service has to answer. It is on the same machine,
/// or it is not there at all.
const METADATA_TIMEOUT: Duration = Duration::from_secs(1);

/// The environment the probe reads, so that a test can hand it one.
pub type Env = BTreeMap<String, String>;

/// The process's environment, as the probe reads it.
pub fn environment() -> Env {
    std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// Whether an ambient credential of type `kind` would be found here.
///
/// `aws`, `azure` and `gcp` are looked for where each cloud's tools look;
/// `ssh` is the agent socket; `kubeconfig` is the configuration file. Any
/// other type is `Unknown`: the tool finds it in a way nothing cheap can
/// check.
pub fn probe_ambient(kind: &str, env: &Env) -> Probe {
    match kind {
        "aws" => probe_aws(env),
        "azure" => probe_azure(env),
        "gcp" => probe_gcp(env),
        "ssh" => probe_ssh(env),
        "kubeconfig" => probe_kubeconfig(env),
        other => Probe::Unknown(format!(
            "an ambient {other} credential is found by the tool itself, in a way that is \
             not checked here"
        )),
    }
}

fn set<'a>(env: &'a Env, name: &str) -> Option<&'a str> {
    env.get(name).map(String::as_str).filter(|v| !v.is_empty())
}

fn home(env: &Env) -> Option<PathBuf> {
    set(env, "HOME")
        .or_else(|| set(env, "USERPROFILE"))
        .map(PathBuf::from)
}

fn probe_aws(env: &Env) -> Probe {
    if set(env, "AWS_ACCESS_KEY_ID").is_some() && set(env, "AWS_SECRET_ACCESS_KEY").is_some() {
        return Probe::Present(
            "the environment holds AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY".to_owned(),
        );
    }
    if let Some(file) = set(env, "AWS_WEB_IDENTITY_TOKEN_FILE") {
        if !Path::new(file).is_file() {
            return Probe::Refused(format!(
                "AWS_WEB_IDENTITY_TOKEN_FILE names {file}, which does not exist"
            ));
        }
        return match set(env, "AWS_ROLE_ARN") {
            Some(_) => Probe::Present(
                "the environment holds AWS_WEB_IDENTITY_TOKEN_FILE and AWS_ROLE_ARN (web \
                 identity, such as a pipeline's OIDC token)"
                    .to_owned(),
            ),
            None => Probe::Refused(
                "AWS_WEB_IDENTITY_TOKEN_FILE is set, and AWS_ROLE_ARN, which says which role \
                 to assume with it, is not"
                    .to_owned(),
            ),
        };
    }
    for name in [
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    ] {
        if set(env, name).is_some() {
            return Probe::Present(format!(
                "the environment holds {name} (container credentials, not fetched)"
            ));
        }
    }
    let credentials = set(env, "AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .or_else(|| home(env).map(|h| h.join(".aws").join("credentials")));
    let config = set(env, "AWS_CONFIG_FILE")
        .map(PathBuf::from)
        .or_else(|| home(env).map(|h| h.join(".aws").join("config")));
    let profile = set(env, "AWS_PROFILE");
    for file in [credentials, config].into_iter().flatten() {
        if file.is_file() {
            return Probe::Present(match profile {
                Some(profile) => format!(
                    "AWS_PROFILE names {profile}, and {} exists (not read)",
                    file.display()
                ),
                None => format!("{} exists (not read)", file.display()),
            });
        }
    }
    if let Some(profile) = profile {
        return Probe::Refused(format!(
            "AWS_PROFILE names {profile}, and there is no credentials or config file to find \
             it in"
        ));
    }
    if set(env, "AWS_EC2_METADATA_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return Probe::Absent(
            "nothing in the environment or the AWS files, and AWS_EC2_METADATA_DISABLED says \
             not to ask the instance"
                .to_owned(),
        );
    }
    let endpoint = set(env, "AWS_EC2_METADATA_SERVICE_ENDPOINT")
        .unwrap_or("http://169.254.169.254")
        .trim_end_matches('/')
        .to_owned();
    // IMDSv2: a token first, then the role, if the instance has one.
    match http(
        "PUT",
        &format!("{endpoint}/latest/api/token"),
        &[("X-aws-ec2-metadata-token-ttl-seconds", "60")],
    ) {
        Ok((200, token)) => match http(
            "GET",
            &format!("{endpoint}/latest/meta-data/iam/security-credentials/"),
            &[("X-aws-ec2-metadata-token", token.trim())],
        ) {
            Ok((200, _)) => Probe::Present(format!(
                "the instance metadata service at {endpoint} says the instance has a role"
            )),
            Ok((404, _)) => Probe::Absent(format!(
                "nothing in the environment or the AWS files, and the instance metadata \
                 service at {endpoint} says the instance has no role"
            )),
            Ok((status, _)) => Probe::Refused(format!(
                "the instance metadata service at {endpoint} answered {status}"
            )),
            Err(why) => Probe::Absent(format!(
                "nothing in the environment or the AWS files, and the instance metadata \
                 service at {endpoint} did not answer: {why}"
            )),
        },
        Ok((status, _)) => Probe::Absent(format!(
            "nothing in the environment or the AWS files, and the instance metadata service \
             at {endpoint} gave no token (status {status}): this is not an EC2 instance"
        )),
        Err(why) => Probe::Absent(format!(
            "nothing in the environment or the AWS files, and the instance metadata service \
             at {endpoint} did not answer: {why}"
        )),
    }
}

fn probe_azure(env: &Env) -> Probe {
    if set(env, "AZURE_CLIENT_ID").is_some() && set(env, "AZURE_TENANT_ID").is_some() {
        if set(env, "AZURE_CLIENT_SECRET").is_some() {
            return Probe::Present(
                "the environment holds AZURE_CLIENT_ID, AZURE_TENANT_ID and \
                 AZURE_CLIENT_SECRET"
                    .to_owned(),
            );
        }
        for (name, what) in [
            (
                "AZURE_FEDERATED_TOKEN_FILE",
                "workload identity federation, such as a pipeline's OIDC token",
            ),
            ("AZURE_CLIENT_CERTIFICATE_PATH", "a client certificate"),
        ] {
            if let Some(file) = set(env, name) {
                return if Path::new(file).is_file() {
                    Probe::Present(format!(
                        "the environment holds AZURE_CLIENT_ID, AZURE_TENANT_ID and {name} \
                         ({what})"
                    ))
                } else {
                    Probe::Refused(format!("{name} names {file}, which does not exist"))
                };
            }
        }
    }
    // A managed identity on App Service, Functions, Container Apps or an
    // Arc machine: the endpoint is in the environment, and its answer to a
    // token request says whether an identity is assigned.
    if let Some(endpoint) = set(env, "IDENTITY_ENDPOINT").or_else(|| set(env, "MSI_ENDPOINT")) {
        let url = format!(
            "{}{}api-version=2019-08-01&resource=https%3A%2F%2Fmanagement.azure.com%2F",
            endpoint,
            if endpoint.contains('?') { "&" } else { "?" }
        );
        let header = set(env, "IDENTITY_HEADER")
            .or_else(|| set(env, "MSI_SECRET"))
            .unwrap_or("");
        let headers: Vec<(&str, &str)> = if header.is_empty() {
            vec![("Metadata", "true")]
        } else {
            vec![("Metadata", "true"), ("X-IDENTITY-HEADER", header)]
        };
        return match http("GET", &url, &headers) {
            Ok((200, _)) => Probe::Present(format!(
                "the managed identity endpoint {endpoint} answers for an identity"
            )),
            Ok((400 | 404, _)) => Probe::Absent(format!(
                "the managed identity endpoint {endpoint} says no identity is assigned"
            )),
            Ok((status, _)) => Probe::Refused(format!(
                "the managed identity endpoint {endpoint} answered {status}"
            )),
            Err(why) => Probe::Absent(format!(
                "nothing in the environment, and the managed identity endpoint {endpoint} \
                 did not answer: {why}"
            )),
        };
    }
    let config = set(env, "AZURE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home(env).map(|h| h.join(".azure")));
    if let Some(profile) = config.map(|dir| dir.join("azureProfile.json")) {
        if profile.is_file() {
            return Probe::Present(format!(
                "the Azure CLI is logged in: {} exists (not read)",
                profile.display()
            ));
        }
    }
    let endpoint = set(env, "AZURE_IMDS_ENDPOINT").unwrap_or("http://169.254.169.254");
    let url = format!(
        "{}/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fmanagement.azure.com%2F",
        endpoint.trim_end_matches('/')
    );
    match http("GET", &url, &[("Metadata", "true")]) {
        Ok((200, _)) => Probe::Present(format!(
            "the instance metadata service at {endpoint} answers for a managed identity"
        )),
        Ok((400 | 404, _)) => Probe::Absent(format!(
            "nothing in the environment or the Azure CLI, and the instance metadata service \
             at {endpoint} says the machine has no managed identity"
        )),
        Ok((status, _)) => Probe::Refused(format!(
            "the instance metadata service at {endpoint} answered {status}"
        )),
        Err(why) => Probe::Absent(format!(
            "nothing in the environment or the Azure CLI, and the instance metadata service \
             at {endpoint} did not answer: {why}"
        )),
    }
}

fn probe_gcp(env: &Env) -> Probe {
    if let Some(file) = set(env, "GOOGLE_APPLICATION_CREDENTIALS") {
        return if Path::new(file).is_file() {
            Probe::Present(format!(
                "GOOGLE_APPLICATION_CREDENTIALS names {file}, which exists (not read)"
            ))
        } else {
            Probe::Refused(format!(
                "GOOGLE_APPLICATION_CREDENTIALS names {file}, which does not exist"
            ))
        };
    }
    if set(env, "CLOUDSDK_AUTH_ACCESS_TOKEN").is_some() {
        return Probe::Present("the environment holds CLOUDSDK_AUTH_ACCESS_TOKEN".to_owned());
    }
    let config = set(env, "CLOUDSDK_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            set(env, "APPDATA")
                .map(|appdata| PathBuf::from(appdata).join("gcloud"))
                .filter(|_| cfg!(windows))
        })
        .or_else(|| home(env).map(|h| h.join(".config").join("gcloud")));
    if let Some(adc) = config.map(|dir| dir.join("application_default_credentials.json")) {
        if adc.is_file() {
            return Probe::Present(format!(
                "the application default credentials file {} exists (not read)",
                adc.display()
            ));
        }
    }
    let host = set(env, "GCE_METADATA_HOST").unwrap_or("169.254.169.254");
    let url = format!("http://{host}/computeMetadata/v1/instance/service-accounts/default/email");
    match http("GET", &url, &[("Metadata-Flavor", "Google")]) {
        Ok((200, _)) => Probe::Present(format!(
            "the metadata server at {host} says the instance has a service account"
        )),
        Ok((404, _)) => Probe::Absent(format!(
            "nothing in the environment or gcloud's files, and the metadata server at {host} \
             says the instance has no service account"
        )),
        Ok((status, _)) => {
            Probe::Refused(format!("the metadata server at {host} answered {status}"))
        }
        Err(why) => Probe::Absent(format!(
            "nothing in the environment or gcloud's files, and the metadata server at {host} \
             did not answer: {why}"
        )),
    }
}

fn probe_ssh(env: &Env) -> Probe {
    match set(env, "SSH_AUTH_SOCK") {
        Some(socket) if Path::new(socket).exists() => {
            Probe::Present(format!("SSH_AUTH_SOCK names the agent at {socket}"))
        }
        Some(socket) => Probe::Refused(format!(
            "SSH_AUTH_SOCK names {socket}, which does not exist"
        )),
        None => Probe::Absent("SSH_AUTH_SOCK is not set, so there is no agent".to_owned()),
    }
}

fn probe_kubeconfig(env: &Env) -> Probe {
    if let Some(list) = set(env, "KUBECONFIG") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        let files: Vec<&str> = list.split(separator).filter(|f| !f.is_empty()).collect();
        return if files.iter().any(|f| Path::new(f).is_file()) {
            Probe::Present(format!("KUBECONFIG names {list} (not read)"))
        } else {
            Probe::Refused(format!("KUBECONFIG names {list}, which does not exist"))
        };
    }
    match home(env).map(|h| h.join(".kube").join("config")) {
        Some(file) if file.is_file() => {
            Probe::Present(format!("{} exists (not read)", file.display()))
        }
        Some(file) => Probe::Absent(format!(
            "KUBECONFIG is not set, and {} does not exist",
            file.display()
        )),
        None => Probe::Absent("KUBECONFIG is not set, and there is no home directory".to_owned()),
    }
}

/// One plain-HTTP request to a metadata service, which is reached by its
/// address and never by a name a resolver has to look up far away. The
/// answer is its status and its body, which is never a secret here: a token
/// for the next question at most, which is wiped with the string.
fn http(method: &str, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url} is not a plain http:// address"))?;
    let (authority, path) = match rest.find(['/', '?']) {
        Some(at) if rest.as_bytes()[at] == b'/' => (&rest[..at], rest[at..].to_owned()),
        Some(at) => (&rest[..at], format!("/{}", &rest[at..])),
        None => (rest, "/".to_owned()),
    };
    let address = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    let socket = address
        .to_socket_addrs()
        .map_err(|e| format!("{authority}: {e}"))?
        .next()
        .ok_or_else(|| format!("{authority} has no address"))?;
    let mut stream = TcpStream::connect_timeout(&socket, METADATA_TIMEOUT)
        .map_err(|e| format!("connecting to {authority}: {e}"))?;
    let _ = stream.set_read_timeout(Some(METADATA_TIMEOUT));
    let _ = stream.set_write_timeout(Some(METADATA_TIMEOUT));
    let mut request = format!("{method} {path} HTTP/1.0\r\nHost: {authority}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("asking {authority}: {e}"))?;
    let mut response = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut response)
        .map_err(|e| format!("reading the answer of {authority}: {e}"))?;
    let text = String::from_utf8_lossy(&response);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("{authority} did not answer with HTTP"))?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A metadata service that answers every request with one status, and
    /// counts the requests, on a port of its own.
    fn stand_in(status: u16, answers: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "token-or-role";
                let _ = write!(
                    stream,
                    "HTTP/1.0 {status} X\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{address}")
    }

    /// A port nothing listens on.
    fn closed_port() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meridian-ambient-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn aws_is_present_from_keys_a_token_file_or_the_instance() {
        let home = scratch("aws");
        let base = [
            ("HOME", home.to_str().unwrap()),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ];
        let mut keys = env(&base);
        keys.insert("AWS_ACCESS_KEY_ID".into(), "AKIA".into());
        keys.insert("AWS_SECRET_ACCESS_KEY".into(), "s".into());
        assert!(matches!(probe_ambient("aws", &keys), Probe::Present(_)));

        let token = home.join("token");
        std::fs::write(&token, "t").unwrap();
        let mut web = env(&base);
        web.insert(
            "AWS_WEB_IDENTITY_TOKEN_FILE".into(),
            token.display().to_string(),
        );
        assert!(
            matches!(probe_ambient("aws", &web), Probe::Refused(_)),
            "no role"
        );
        web.insert("AWS_ROLE_ARN".into(), "arn:aws:iam::1:role/x".into());
        assert!(matches!(probe_ambient("aws", &web), Probe::Present(_)));

        let mut instance = env(&[("HOME", home.to_str().unwrap())]);
        instance.insert("AWS_EC2_METADATA_SERVICE_ENDPOINT".into(), stand_in(200, 2));
        assert!(matches!(probe_ambient("aws", &instance), Probe::Present(_)));
    }

    #[test]
    fn aws_is_absent_with_nothing_set_and_no_instance_answering() {
        let home = scratch("aws-absent");
        let mut disabled = env(&[
            ("HOME", home.to_str().unwrap()),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ]);
        assert!(matches!(probe_ambient("aws", &disabled), Probe::Absent(_)));
        disabled.remove("AWS_EC2_METADATA_DISABLED");
        disabled.insert("AWS_EC2_METADATA_SERVICE_ENDPOINT".into(), closed_port());
        match probe_ambient("aws", &disabled) {
            Probe::Absent(why) => assert!(why.contains("did not answer"), "{why}"),
            other => panic!("{other:?}"),
        }
        let mut not_ec2 = env(&[("HOME", home.to_str().unwrap())]);
        not_ec2.insert("AWS_EC2_METADATA_SERVICE_ENDPOINT".into(), stand_in(404, 1));
        match probe_ambient("aws", &not_ec2) {
            Probe::Absent(why) => assert!(why.contains("not an EC2 instance"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn azure_is_present_from_a_federated_token_a_managed_identity_or_a_login() {
        let home = scratch("azure");
        let token = home.join("token");
        std::fs::write(&token, "t").unwrap();
        let mut federated = env(&[
            ("HOME", home.to_str().unwrap()),
            ("AZURE_CLIENT_ID", "c"),
            ("AZURE_TENANT_ID", "t"),
        ]);
        federated.insert(
            "AZURE_FEDERATED_TOKEN_FILE".into(),
            token.display().to_string(),
        );
        assert!(matches!(
            probe_ambient("azure", &federated),
            Probe::Present(_)
        ));
        federated.insert(
            "AZURE_FEDERATED_TOKEN_FILE".into(),
            home.join("missing").display().to_string(),
        );
        assert!(matches!(
            probe_ambient("azure", &federated),
            Probe::Refused(_)
        ));

        let mut managed = env(&[("HOME", home.to_str().unwrap())]);
        managed.insert("IDENTITY_ENDPOINT".into(), stand_in(200, 1));
        managed.insert("IDENTITY_HEADER".into(), "h".into());
        assert!(matches!(
            probe_ambient("azure", &managed),
            Probe::Present(_)
        ));
        managed.insert("IDENTITY_ENDPOINT".into(), stand_in(400, 1));
        assert!(matches!(probe_ambient("azure", &managed), Probe::Absent(_)));
        managed.insert("IDENTITY_ENDPOINT".into(), stand_in(500, 1));
        assert!(matches!(
            probe_ambient("azure", &managed),
            Probe::Refused(_)
        ));
        managed.insert("IDENTITY_ENDPOINT".into(), closed_port());
        match probe_ambient("azure", &managed) {
            Probe::Absent(why) => assert!(why.contains("did not answer"), "{why}"),
            other => panic!("{other:?}"),
        }

        std::fs::create_dir_all(home.join(".azure")).unwrap();
        std::fs::write(home.join(".azure/azureProfile.json"), "{}").unwrap();
        let login = env(&[("HOME", home.to_str().unwrap())]);
        assert!(matches!(probe_ambient("azure", &login), Probe::Present(_)));
    }

    #[test]
    fn azure_is_absent_when_nothing_is_set_and_the_instance_has_no_identity() {
        let home = scratch("azure-absent");
        let mut none = env(&[("HOME", home.to_str().unwrap())]);
        none.insert("AZURE_IMDS_ENDPOINT".into(), stand_in(400, 1));
        assert!(matches!(probe_ambient("azure", &none), Probe::Absent(_)));
        none.insert("AZURE_IMDS_ENDPOINT".into(), closed_port());
        assert!(matches!(probe_ambient("azure", &none), Probe::Absent(_)));
    }

    #[test]
    fn gcp_is_present_from_a_credentials_file_a_token_or_the_metadata_server() {
        let home = scratch("gcp");
        let key = home.join("key.json");
        std::fs::write(&key, "{}").unwrap();
        let mut file = env(&[("HOME", home.to_str().unwrap())]);
        file.insert(
            "GOOGLE_APPLICATION_CREDENTIALS".into(),
            key.display().to_string(),
        );
        assert!(matches!(probe_ambient("gcp", &file), Probe::Present(_)));
        file.insert(
            "GOOGLE_APPLICATION_CREDENTIALS".into(),
            home.join("missing").display().to_string(),
        );
        assert!(matches!(probe_ambient("gcp", &file), Probe::Refused(_)));

        let token = env(&[
            ("HOME", home.to_str().unwrap()),
            ("CLOUDSDK_AUTH_ACCESS_TOKEN", "t"),
        ]);
        assert!(matches!(probe_ambient("gcp", &token), Probe::Present(_)));

        let mut metadata = env(&[("HOME", home.to_str().unwrap())]);
        metadata.insert(
            "GCE_METADATA_HOST".into(),
            stand_in(200, 1).trim_start_matches("http://").to_owned(),
        );
        assert!(matches!(probe_ambient("gcp", &metadata), Probe::Present(_)));
        metadata.insert(
            "GCE_METADATA_HOST".into(),
            stand_in(404, 1).trim_start_matches("http://").to_owned(),
        );
        assert!(matches!(probe_ambient("gcp", &metadata), Probe::Absent(_)));
        metadata.insert(
            "GCE_METADATA_HOST".into(),
            closed_port().trim_start_matches("http://").to_owned(),
        );
        assert!(matches!(probe_ambient("gcp", &metadata), Probe::Absent(_)));
    }

    #[test]
    fn ssh_and_kubeconfig_are_files_and_the_rest_is_unknown() {
        let home = scratch("ssh");
        let socket = home.join("agent.sock");
        std::fs::write(&socket, "").unwrap();
        let mut agent = env(&[("HOME", home.to_str().unwrap())]);
        assert!(matches!(probe_ambient("ssh", &agent), Probe::Absent(_)));
        agent.insert("SSH_AUTH_SOCK".into(), socket.display().to_string());
        assert!(matches!(probe_ambient("ssh", &agent), Probe::Present(_)));

        let kube = env(&[("HOME", home.to_str().unwrap())]);
        assert!(matches!(
            probe_ambient("kubeconfig", &kube),
            Probe::Absent(_)
        ));
        std::fs::create_dir_all(home.join(".kube")).unwrap();
        std::fs::write(home.join(".kube/config"), "").unwrap();
        assert!(matches!(
            probe_ambient("kubeconfig", &kube),
            Probe::Present(_)
        ));

        assert!(matches!(
            probe_ambient("registry", &kube),
            Probe::Unknown(_)
        ));
        assert!(matches!(probe_ambient("token", &kube), Probe::Unknown(_)));
    }
}
