//! Modules from an OCI registry.
//!
//! A module is stored as an OCI artifact: one manifest with one layer, a
//! gzipped tar of the module. Any registry that implements the OCI
//! distribution specification holds it, so an organisation's existing
//! registry, with its existing access control, is already a module registry.
//!
//! Everything read is checked against the digest it was asked for by, and a
//! version that is already published is never replaced with other content.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use meridian_secret::Credential;
use semver::Version;
use ureq::http::Response;
use ureq::{Agent, Body};

use crate::integrity::sha256_hex;
use crate::pack::{unpack, MAX_UNPACKED_BYTES};
use crate::Error;

/// The artifact type of a module's manifest.
pub const ARTIFACT_TYPE: &str = "application/vnd.meridian.module.v1";
/// The media type of the one layer that holds the module.
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.meridian.module.layer.v1.tar+gzip";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// A manifest is a few hundred bytes; this is generous.
const MAX_MANIFEST_BYTES: u64 = 4 << 20;
const MAX_TAG_PAGES: usize = 100;

/// One repository on one registry.
pub(crate) struct Repository<'a> {
    agent: Agent,
    /// `https://host[:port]`.
    base: String,
    host: String,
    /// The repository's path on the registry.
    path: String,
    /// As written, for messages.
    name: String,
    insecure: bool,
    login: Login<'a>,
    /// The `Authorization` header that last worked.
    authorization: RefCell<Option<String>>,
    /// Whether this is a push, for the message when access is refused.
    writing: Cell<bool>,
}

/// Who to be to a registry.
enum Login<'a> {
    /// What the Docker configuration file holds for the host, if anything.
    Docker(Option<(String, String)>),
    /// A credential the project declared and named for this repository.
    Declared(&'a Credential),
}

/// One request to a registry.
enum Call<'a> {
    Get {
        accept: &'a str,
    },
    Head {
        accept: &'a str,
    },
    Post,
    Put {
        content_type: &'a str,
        body: &'a [u8],
    },
}

fn percent_encode(s: &str) -> String {
    use std::fmt::Write as _;
    s.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// Reads `key="value"` pairs out of a `WWW-Authenticate` challenge.
fn challenge_params(params: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut rest = params.trim();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq]
            .trim()
            .trim_start_matches(',')
            .trim()
            .to_ascii_lowercase();
        let after = &rest[eq + 1..];
        let (value, tail) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], &quoted[end + 1..]),
                None => (quoted, ""),
            }
        } else {
            match after.find(',') {
                Some(end) => (&after[..end], &after[end..]),
                None => (after, ""),
            }
        };
        out.insert(key, value.trim().to_owned());
        rest = tail;
    }
    out
}

/// The login for `host` from the Docker configuration file, if it has one.
/// Only `auths` entries are read: no credential helpers are run.
fn docker_credentials(host: &str) -> Option<(String, String)> {
    let dir = match std::env::var_os("DOCKER_CONFIG") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".docker"),
    };
    let data = std::fs::read(dir.join("config.json")).ok()?;
    credentials_from(&data, host)
}

fn credentials_from(config: &[u8], host: &str) -> Option<(String, String)> {
    let config: serde_json::Value = serde_json::from_slice(config).ok()?;
    let auths = config.get("auths")?.as_object()?;
    let entry = auths.iter().find_map(|(key, entry)| {
        // A key may be a bare host or a URL such as https://host/v1/.
        let stripped = key
            .strip_prefix("https://")
            .or_else(|| key.strip_prefix("http://"))
            .unwrap_or(key);
        let key_host = stripped.split('/').next().unwrap_or(stripped);
        (key_host == host).then_some(entry)
    })?;
    let auth = entry.get("auth")?.as_str()?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(auth.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some((user.to_owned(), password.to_owned()))
}

fn basic(user: &str, password: &str) -> String {
    basic_bytes(user, password.as_bytes())
}

/// The `Authorization` header for a user and a password that is not
/// necessarily text. The joined copy is overwritten before it is freed; the
/// header itself is what the registry is sent.
fn basic_bytes(user: &str, password: &[u8]) -> String {
    let mut joined = Vec::with_capacity(user.len() + 1 + password.len());
    joined.extend_from_slice(user.as_bytes());
    joined.push(b':');
    joined.extend_from_slice(password);
    let header = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(&joined)
    );
    joined.fill(0);
    header
}

fn header<'r>(response: &'r Response<Body>, name: &str) -> Option<&'r str> {
    response.headers().get(name)?.to_str().ok()
}

impl<'a> Repository<'a> {
    /// Opens a repository. `declared` is the credential the project named
    /// for it; without one, the Docker configuration file is looked in.
    pub(crate) fn open(
        repository: &str,
        insecure: bool,
        declared: Option<&'a Credential>,
    ) -> Result<Repository<'a>, Error> {
        let bad = || {
            Error::new(format!(
                "{repository:?} is not a registry repository such as \
                 registry.example.com/team/module"
            ))
        };
        let (host, path) = repository.split_once('/').ok_or_else(bad)?;
        let ok_host = !host.is_empty()
            && host.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')
            });
        let ok_path = !path.is_empty()
            && path.split('/').all(|part| {
                !part.is_empty()
                    && !part.starts_with('.')
                    && part.bytes().all(|b| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || matches!(b, b'.' | b'_' | b'-')
                    })
            });
        if !ok_host || !ok_path {
            return Err(bad());
        }
        let agent: Agent = Agent::config_builder()
            // Statuses are read here: 401 starts a login and 404 has a
            // message of its own.
            .http_status_as_error(false)
            // A redirect may not take a secure fetch to plain HTTP.
            .https_only(!insecure)
            .timeout_global(Some(Duration::from_secs(600)))
            .timeout_connect(Some(Duration::from_secs(30)))
            .user_agent(concat!("meridian/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let login = match declared {
            None => Login::Docker(docker_credentials(host)),
            Some(credential) => {
                if !matches!(credential.kind.as_str(), "registry" | "usernamePassword") {
                    return Err(Error::new(format!(
                        "the credential {} is a {} credential; a registry is logged in to \
                         with a registry or a usernamePassword credential",
                        credential.name, credential.kind
                    )));
                }
                // A login declared for one registry is never sent to another.
                if let Some(server) = credential.text("server").filter(|s| *s != host) {
                    return Err(Error::new(format!(
                        "the credential {} is for the registry {server}, and {repository} is \
                         on {host}; it was not sent",
                        credential.name
                    )));
                }
                Login::Declared(credential)
            }
        };
        let scheme = if insecure { "http" } else { "https" };
        Ok(Repository {
            agent,
            base: format!("{scheme}://{host}"),
            host: host.to_owned(),
            path: path.to_owned(),
            name: repository.to_owned(),
            insecure,
            login,
            authorization: RefCell::new(None),
            writing: Cell::new(false),
        })
    }

    fn send(
        &self,
        url: &str,
        call: &Call<'_>,
        authorization: Option<&str>,
    ) -> Result<Response<Body>, Error> {
        let auth = authorization.unwrap_or_default();
        let result = match call {
            Call::Get { accept } | Call::Head { accept } => {
                let mut request = match call {
                    Call::Head { .. } => self.agent.head(url),
                    _ => self.agent.get(url),
                }
                .header("Accept", *accept);
                if authorization.is_some() {
                    request = request.header("Authorization", auth);
                }
                request.call()
            }
            Call::Post => {
                let mut request = self.agent.post(url);
                if authorization.is_some() {
                    request = request.header("Authorization", auth);
                }
                request.send_empty()
            }
            Call::Put { content_type, body } => {
                let mut request = self.agent.put(url).content_type(*content_type);
                if authorization.is_some() {
                    request = request.header("Authorization", auth);
                }
                request.send(*body)
            }
        };
        result.map_err(|e| Error::new(format!("reaching the registry {}: {e}", self.host)))
    }

    /// The `Authorization` header that says who we are, when we have a login.
    fn basic_login(&self) -> Option<String> {
        match &self.login {
            Login::Docker(found) => found.as_ref().map(|(user, password)| basic(user, password)),
            Login::Declared(credential) => {
                let user = credential.text("user").unwrap_or_default();
                credential
                    .secret("password")
                    .map(|password| password.expose(|bytes| basic_bytes(user, bytes)))
            }
        }
    }

    /// Answers a 401 by logging in the way the registry asked for.
    fn authorize(&self, challenge: &str) -> Result<String, Error> {
        let (scheme, params) = challenge.split_once(' ').unwrap_or((challenge, ""));
        if scheme.eq_ignore_ascii_case("basic") {
            return self.basic_login().ok_or_else(|| self.no_access());
        }
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(Error::new(format!(
                "the registry {} asks for {scheme:?} authentication, which is not supported",
                self.host
            )));
        }
        let params = challenge_params(params);
        let Some(realm) = params.get("realm") else {
            return Err(Error::new(format!(
                "the registry {} asked for a token without saying where to get one",
                self.host
            )));
        };
        if !(realm.starts_with("https://") || self.insecure && realm.starts_with("http://")) {
            return Err(Error::new(format!(
                "the registry {} sends logins to {realm:?}, which is not an https address",
                self.host
            )));
        }
        let mut url = realm.clone();
        let mut separator = if realm.contains('?') { '&' } else { '?' };
        for key in ["service", "scope"] {
            if let Some(value) = params.get(key) {
                url.push(separator);
                url.push_str(&format!("{key}={}", percent_encode(value)));
                separator = '&';
            }
        }
        let login = self.basic_login();
        let token_call = Call::Get {
            accept: "application/json",
        };
        let mut response = self.send(&url, &token_call, login.as_deref())?;
        if !response.status().is_success() {
            return Err(self.no_access());
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_MANIFEST_BYTES)
            .read_to_vec()
            .map_err(|e| Error::new(format!("reading a token from {realm}: {e}")))?;
        let token = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("token")
                    .or_else(|| v.get("access_token"))
                    .and_then(|t| t.as_str().map(str::to_owned))
            })
            .ok_or_else(|| Error::new(format!("{realm} did not answer with a token")))?;
        Ok(format!("Bearer {token}"))
    }

    fn no_access(&self) -> Error {
        let hint = match &self.login {
            Login::Declared(credential) => {
                format!("the credential {} was refused", credential.name)
            }
            Login::Docker(Some(_)) => {
                "the login in the Docker configuration file was refused".to_owned()
            }
            Login::Docker(None) => "no login for it was found in the Docker configuration \
                                    file; run `docker login` for that registry, or name a \
                                    credential for it in meridian.yaml"
                .to_owned(),
        };
        let doing = if self.writing.get() {
            "writing to"
        } else {
            "reading"
        };
        Error::new(format!(
            "the registry {} did not allow {doing} {}: {hint}",
            self.host, self.name
        ))
    }

    /// A request that logs in once if the registry asks.
    fn request(&self, url: &str, call: &Call<'_>) -> Result<Response<Body>, Error> {
        let known = self.authorization.borrow().clone();
        let response = self.send(url, call, known.as_deref())?;
        if response.status().as_u16() != 401 {
            return Ok(response);
        }
        let Some(challenge) = header(&response, "www-authenticate").map(str::to_owned) else {
            return Err(self.no_access());
        };
        let authorization = self.authorize(&challenge)?;
        let response = self.send(url, call, Some(&authorization))?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(self.no_access());
        }
        *self.authorization.borrow_mut() = Some(authorization);
        Ok(response)
    }

    fn get(&self, url: &str, accept: &str) -> Result<Response<Body>, Error> {
        self.request(url, &Call::Get { accept })
    }

    fn check(&self, response: &Response<Body>, what: &str) -> Result<(), Error> {
        match response.status().as_u16() {
            200..=299 => Ok(()),
            404 => Err(Error::new(format!("{what} was not found in {}", self.name))),
            403 => Err(self.no_access()),
            status => Err(Error::new(format!(
                "the registry {} answered {status} for {what} of {}",
                self.host, self.name
            ))),
        }
    }

    fn body(&self, mut response: Response<Body>, limit: u64, what: &str) -> Result<Vec<u8>, Error> {
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| Error::new(format!("reading {what} of {}: {e}", self.name)))
    }

    /// Every tag of the repository.
    pub(crate) fn tags(&self) -> Result<Vec<String>, Error> {
        let mut out = Vec::new();
        let mut next = Some(format!("/v2/{}/tags/list", self.path));
        let mut pages = 0;
        while let Some(path) = next.take() {
            pages += 1;
            if pages > MAX_TAG_PAGES {
                break;
            }
            let response = self.get(&format!("{}{path}", self.base), "application/json")?;
            self.check(&response, "the list of versions")?;
            // `</v2/…/tags/list?last=…>; rel="next"`. Only a path on this
            // registry is followed.
            next = header(&response, "link").and_then(|link| {
                let target = link.split(';').next()?.trim();
                let target = target.strip_prefix('<')?.strip_suffix('>')?;
                (link.contains("rel=\"next\"") && target.starts_with("/v2/"))
                    .then(|| target.to_owned())
            });
            let body = self.body(response, MAX_MANIFEST_BYTES, "the list of versions")?;
            let listing: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
                Error::new(format!(
                    "the list of versions of {} is not JSON: {e}",
                    self.name
                ))
            })?;
            if let Some(tags) = listing.get("tags").and_then(|t| t.as_array()) {
                out.extend(tags.iter().filter_map(|t| t.as_str().map(str::to_owned)));
            }
        }
        Ok(out)
    }

    /// Fetches the manifest at a tag or digest. Returns the manifest's own
    /// digest and the digest of the layer that holds the module.
    fn manifest(&self, reference: &str) -> Result<(String, String), Error> {
        let what = format!("the manifest {reference}");
        let url = format!("{}/v2/{}/manifests/{reference}", self.base, self.path);
        let response = self.get(&url, MANIFEST_MEDIA_TYPE)?;
        self.check(&response, &what)?;
        let body = self.body(response, MAX_MANIFEST_BYTES, &what)?;
        let digest = format!("sha256:{}", sha256_hex(&body));
        if reference.starts_with("sha256:") && reference != digest {
            return Err(Error::new(format!(
                "the registry {} answered a request for {reference} of {} with different \
                 content ({digest})",
                self.host, self.name
            )));
        }
        let manifest: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| Error::new(format!("{what} of {} is not JSON: {e}", self.name)))?;
        let layers = manifest
            .get("layers")
            .and_then(|l| l.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let media = |layer: &serde_json::Value| {
            layer
                .get("mediaType")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let Some(layer) = layers.iter().find(|l| media(l) == LAYER_MEDIA_TYPE) else {
            let found: Vec<String> = layers.iter().map(media).collect();
            return Err(Error::new(format!(
                "{reference} of {} is not a Meridian module: it has no layer of type \
                 {LAYER_MEDIA_TYPE} (found: {})",
                self.name,
                if found.is_empty() {
                    "no layers".to_owned()
                } else {
                    found.join(", ")
                }
            )));
        };
        let layer_digest = layer
            .get("digest")
            .and_then(|d| d.as_str())
            .filter(|d| {
                d.strip_prefix("sha256:")
                    .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            })
            .ok_or_else(|| {
                Error::new(format!(
                    "{reference} of {} names its layer by something other than a sha256 digest",
                    self.name
                ))
            })?;
        Ok((digest, layer_digest.to_owned()))
    }

    fn blob(&self, digest: &str) -> Result<Vec<u8>, Error> {
        let what = format!("the layer {digest}");
        let url = format!("{}/v2/{}/blobs/{digest}", self.base, self.path);
        let response = self.get(&url, "application/octet-stream")?;
        self.check(&response, &what)?;
        let body = self.body(response, MAX_UNPACKED_BYTES, &what)?;
        let found = format!("sha256:{}", sha256_hex(&body));
        if found != digest {
            return Err(Error::new(format!(
                "the registry {} answered a request for {digest} of {} with different content \
                 ({found})",
                self.host, self.name
            )));
        }
        Ok(body)
    }
}

/// The empty JSON object every artifact without a configuration points at.
const EMPTY_CONFIG: &[u8] = b"{}";
const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";

impl Repository<'_> {
    /// The digest of the manifest at `reference`, if there is one.
    fn existing(&self, reference: &str) -> Result<Option<String>, Error> {
        let url = format!("{}/v2/{}/manifests/{reference}", self.base, self.path);
        let response = self.get(&url, MANIFEST_MEDIA_TYPE)?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        let what = format!("the manifest {reference}");
        self.check(&response, &what)?;
        let body = self.body(response, MAX_MANIFEST_BYTES, &what)?;
        Ok(Some(format!("sha256:{}", sha256_hex(&body))))
    }

    /// Uploads a blob unless the repository already holds it.
    fn upload(&self, bytes: &[u8]) -> Result<String, Error> {
        let digest = format!("sha256:{}", sha256_hex(bytes));
        let what = format!("the layer {digest}");
        let blob = format!("{}/v2/{}/blobs/{digest}", self.base, self.path);
        let head = Call::Head {
            accept: "application/octet-stream",
        };
        if self.request(&blob, &head)?.status().is_success() {
            return Ok(digest);
        }
        let start = format!("{}/v2/{}/blobs/uploads/", self.base, self.path);
        let response = self.request(&start, &Call::Post)?;
        self.check(&response, &what)?;
        // The registry says where to send the bytes. It may be a path on
        // this registry or a full address, which must still be this registry.
        let location = header(&response, "location").unwrap_or_default();
        let target = if location.starts_with('/') {
            format!("{}{location}", self.base)
        } else if location.starts_with(&format!("{}/", self.base)) {
            location.to_owned()
        } else {
            return Err(Error::new(format!(
                "the registry {} sent the upload of {} to {location:?}, which is not on that \
                 registry",
                self.host, self.name
            )));
        };
        let separator = if target.contains('?') { '&' } else { '?' };
        let put = Call::Put {
            content_type: "application/octet-stream",
            body: bytes,
        };
        let response = self.request(
            &format!("{target}{separator}digest={}", percent_encode(&digest)),
            &put,
        )?;
        self.check(&response, &what)?;
        Ok(digest)
    }
}

/// What [`push`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pushed {
    /// The tag the version was published under.
    pub tag: String,
    /// The digest of the manifest.
    pub digest: String,
    /// The version was already there with exactly this content, so nothing
    /// was uploaded.
    pub already_published: bool,
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
}

/// Publishes a packed module to a registry as `version`.
///
/// A version is published once. If the repository already holds that version
/// with other content the push is refused: consumers who pinned it would keep
/// the old content while new ones got the new, under one version number.
pub fn push(
    repository: &str,
    insecure: bool,
    login: Option<&Credential>,
    name: &str,
    version: &Version,
    description: &str,
    archive: &[u8],
) -> Result<Pushed, Error> {
    let repo = Repository::open(repository, insecure, login)?;
    repo.writing.set(true);
    // A tag cannot hold `+`.
    let tag = version.to_string().replace('+', "_");

    // Written by hand, key by key, so the same module always gives the same
    // manifest and therefore the same digest.
    let mut annotations = format!(
        "{{\"org.opencontainers.image.title\":{},\"org.opencontainers.image.version\":{}",
        json_string(name),
        json_string(&version.to_string())
    );
    if !description.is_empty() {
        annotations.push_str(&format!(
            ",\"org.opencontainers.image.description\":{}",
            json_string(description)
        ));
    }
    annotations.push('}');
    let manifest = format!(
        "{{\"schemaVersion\":2,\"mediaType\":\"{MANIFEST_MEDIA_TYPE}\",\"artifactType\":\"{ARTIFACT_TYPE}\",\
         \"config\":{{\"mediaType\":\"{EMPTY_CONFIG_MEDIA_TYPE}\",\"digest\":\"sha256:{}\",\"size\":{}}},\
         \"layers\":[{{\"mediaType\":\"{LAYER_MEDIA_TYPE}\",\"digest\":\"sha256:{}\",\"size\":{}}}],\
         \"annotations\":{annotations}}}",
        sha256_hex(EMPTY_CONFIG),
        EMPTY_CONFIG.len(),
        sha256_hex(archive),
        archive.len()
    );
    let digest = format!("sha256:{}", sha256_hex(manifest.as_bytes()));

    match repo.existing(&tag)? {
        Some(found) if found == digest => {
            return Ok(Pushed {
                tag,
                digest,
                already_published: true,
            })
        }
        Some(found) => {
            return Err(Error::new(format!(
                "{name} {version} is already published in {repository} with different content \
                 ({found}); a version is published once. Release a new version instead"
            )))
        }
        None => {}
    }

    repo.upload(EMPTY_CONFIG)?;
    repo.upload(archive)?;
    let url = format!("{}/v2/{}/manifests/{tag}", repo.base, repo.path);
    let put = Call::Put {
        content_type: MANIFEST_MEDIA_TYPE,
        body: manifest.as_bytes(),
    };
    let response = repo.request(&url, &put)?;
    repo.check(&response, &format!("the manifest {tag}"))?;
    Ok(Pushed {
        tag,
        digest,
        already_published: false,
    })
}

/// The versions a repository offers: its tags that are SemVer versions,
/// each with the tag that carries it.
pub(crate) fn versions(
    repository: &str,
    insecure: bool,
    login: Option<&Credential>,
) -> Result<BTreeMap<Version, String>, Error> {
    let repo = Repository::open(repository, insecure, login)?;
    let mut out = BTreeMap::new();
    for tag in repo.tags()? {
        // A tag cannot hold `+`, so build metadata is written with `_`.
        let text = tag.strip_prefix('v').unwrap_or(&tag).replace('_', "+");
        if let Ok(version) = Version::parse(&text) {
            if tag.starts_with('v') || !out.contains_key(&version) {
                out.insert(version, tag);
            }
        }
    }
    Ok(out)
}

/// Fetches the module at `reference` (a tag or a manifest digest) into the
/// new directory `dest`. Returns the manifest's digest.
pub(crate) fn fetch(
    repository: &str,
    insecure: bool,
    login: Option<&Credential>,
    reference: &str,
    dest: &Path,
) -> Result<String, Error> {
    let repo = Repository::open(repository, insecure, login)?;
    let (digest, layer) = repo.manifest(reference)?;
    let archive = repo.blob(&layer)?;
    unpack(&archive, dest)?;
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_challenge_is_read_whatever_its_spacing() {
        let p = challenge_params(
            "realm=\"https://auth.example.com/token\", service=\"registry.example.com\",scope=\"repository:acme/nginx:pull\"",
        );
        assert_eq!(p["realm"], "https://auth.example.com/token");
        assert_eq!(p["service"], "registry.example.com");
        assert_eq!(p["scope"], "repository:acme/nginx:pull");
        assert_eq!(challenge_params("realm=x,service=y")["service"], "y");
        assert_eq!(
            percent_encode("repository:acme/nginx:pull"),
            "repository%3Aacme%2Fnginx%3Apull"
        );
    }

    #[test]
    fn a_login_is_found_by_host_in_the_docker_configuration() {
        // "user:pass:word", so a colon in the password survives.
        let config = br#"{"auths": {
            "https://index.example.com/v1/": {"auth": "dXNlcjpwYXNzOndvcmQ="},
            "registry.example.com:5000": {"auth": "YTpi"},
            "broken.example.com": {"auth": "!!"},
            "helper.example.com": {}
        }}"#;
        assert_eq!(
            credentials_from(config, "index.example.com"),
            Some(("user".to_owned(), "pass:word".to_owned()))
        );
        assert_eq!(
            credentials_from(config, "registry.example.com:5000"),
            Some(("a".to_owned(), "b".to_owned()))
        );
        for host in [
            "registry.example.com",
            "broken.example.com",
            "helper.example.com",
            "other",
        ] {
            assert_eq!(credentials_from(config, host), None, "{host}");
        }
        assert_eq!(credentials_from(b"not json", "x"), None);
        // A login read from the file is sent as the file stored it.
        for (host, stored) in [
            ("index.example.com", "dXNlcjpwYXNzOndvcmQ="),
            ("registry.example.com:5000", "YTpi"),
        ] {
            let (user, password) = credentials_from(config, host).unwrap();
            assert_eq!(basic(&user, &password), format!("Basic {stored}"));
        }
    }

    #[test]
    fn a_repository_is_a_host_and_a_path_and_nothing_else() {
        assert!(Repository::open("registry.example.com:5000/acme/nginx", false, None).is_ok());
        for bad in [
            "nginx",
            "registry.example.com/",
            "registry.example.com/Acme",
            "reg/a b",
            "reg/a:1.0",
            "/a/b",
            "reg/a/../b",
        ] {
            assert!(Repository::open(bad, false, None).is_err(), "{bad}");
        }
    }
}
