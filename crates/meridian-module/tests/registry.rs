//! Modules from an OCI registry, against a small registry served from this
//! process. It speaks enough of the distribution API to be fetched from:
//! tags, manifests by tag and by digest, blobs, and token authentication.

#![cfg(unix)]

use base64::Engine as _;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use meridian_module::{
    install, pack, push, Change, Manifest, Mode, Modules, Project, ARTIFACT_TYPE, LAYER_MEDIA_TYPE,
    LOCK_FILE,
};
use sha2::{Digest, Sha256};

const CORE: &str = "0.1.0";
/// `reader:s3cret`, as the Docker configuration file stores it.
const LOGIN: &str = "cmVhZGVyOnMzY3JldA==";
const TOKEN: &str = "a-short-lived-token";

fn digest(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn new(status: u16, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn with(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// What the registry holds, by request path.
#[derive(Default)]
struct Content {
    paths: HashMap<String, Reply>,
    /// Requests seen, as `METHOD path`.
    log: Vec<String>,
}

struct Registry {
    /// `127.0.0.1:<port>`.
    host: String,
    content: Arc<Mutex<Content>>,
}

impl Registry {
    /// Starts a registry. With `private`, everything under `/v2/` needs the
    /// bearer token, which `/token` hands out to the right login.
    fn start(private: bool) -> Registry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let content = Arc::new(Mutex::new(Content::default()));
        let (served, realm) = (Arc::clone(&content), format!("http://{host}/token"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (served, realm) = (Arc::clone(&served), realm.clone());
                std::thread::spawn(move || serve(stream, &served, private, &realm));
            }
        });
        Registry { host, content }
    }

    fn put(&self, path: &str, reply: Reply) {
        self.content
            .lock()
            .unwrap()
            .paths
            .insert(path.to_owned(), reply);
    }

    /// Publishes the module directory `dir` to `repository` under `tag`.
    fn publish(&self, repository: &str, tag: &str, dir: &Path) -> String {
        self.publish_layer(repository, tag, &pack(dir).unwrap(), LAYER_MEDIA_TYPE)
    }

    fn publish_layer(&self, repository: &str, tag: &str, layer: &[u8], media_type: &str) -> String {
        let layer_digest = digest(layer);
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"{ARTIFACT_TYPE}","config":{{"mediaType":"application/vnd.oci.empty.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2}},"layers":[{{"mediaType":"{media_type}","digest":"{layer_digest}","size":{}}}]}}"#,
            layer.len()
        );
        let manifest_digest = digest(manifest.as_bytes());
        self.put(
            &format!("/v2/{repository}/blobs/{layer_digest}"),
            Reply::new(200, layer),
        );
        for reference in [tag, &manifest_digest] {
            self.put(
                &format!("/v2/{repository}/manifests/{reference}"),
                Reply::new(200, manifest.clone()),
            );
        }
        manifest_digest
    }

    fn tags(&self, repository: &str, tags: &[&str]) {
        let list: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
        self.put(
            &format!("/v2/{repository}/tags/list"),
            Reply::new(
                200,
                format!(
                    "{{\"name\":\"{repository}\",\"tags\":[{}]}}",
                    list.join(",")
                ),
            ),
        );
    }

    /// The requests seen since the last call that mention `about`. The
    /// registries are shared between tests, so each asks about its own
    /// repository.
    fn requests(&self, about: &str) -> Vec<String> {
        let mut content = self.content.lock().unwrap();
        let (mine, rest) = std::mem::take(&mut content.log)
            .into_iter()
            .partition(|r| r.contains(about));
        content.log = rest;
        mine
    }
}

fn serve(stream: TcpStream, content: &Mutex<Content>, private: bool, realm: &str) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut authorization = String::new();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                authorization = value.trim().to_owned();
            }
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut sent = vec![0u8; length];
    if std::io::Read::read_exact(&mut reader, &mut sent).is_err() {
        return;
    }

    let mut content = content.lock().unwrap();
    content.log.push(format!("{method} {target}"));
    let path = target.split('?').next().unwrap_or(target);
    let unauthorized = || {
        Reply::new(401, "").with(
            "WWW-Authenticate",
            &format!("Bearer realm=\"{realm}\",service=\"test-registry\",scope=\"repository:acme/private:pull\""),
        )
    };
    let missing = Reply::new(404, "{}");
    let reply = if path == "/token" {
        let asked_right = target.contains("service=test-registry")
            && target.contains("scope=repository%3Aacme%2Fprivate%3Apull");
        if authorization == format!("Basic {LOGIN}") && asked_right {
            Reply::new(200, format!("{{\"token\":\"{TOKEN}\"}}"))
        } else {
            Reply::new(401, "")
        }
    } else if private && authorization != format!("Bearer {TOKEN}") {
        unauthorized()
    } else if method == "POST" && path.ends_with("/blobs/uploads/") {
        // An upload session: the bytes go to the address in Location.
        Reply::new(202, "").with("Location", &format!("{path}session-1?state=x"))
    } else if method == "PUT" && path.contains("/blobs/uploads/") {
        let named = target
            .split("digest=")
            .nth(1)
            .unwrap_or_default()
            .replace("%3A", ":");
        if named == digest(&sent) {
            let repository = path.split("/blobs/uploads/").next().unwrap_or_default();
            let stored = format!("{repository}/blobs/{named}");
            content.paths.insert(stored, Reply::new(200, sent));
            Reply::new(201, "")
        } else {
            Reply::new(400, "the digest does not match the bytes")
        }
    } else if method == "PUT" && path.contains("/manifests/") {
        let repository = path.split("/manifests/").next().unwrap_or_default();
        let by_digest = format!("{repository}/manifests/{}", digest(&sent));
        content
            .paths
            .insert(by_digest, Reply::new(200, sent.clone()));
        content.paths.insert(path.to_owned(), Reply::new(200, sent));
        Reply::new(201, "")
    } else {
        match content.paths.get(path) {
            Some(reply) => Reply {
                status: reply.status,
                headers: reply.headers.clone(),
                // A HEAD is answered like a GET, without the bytes.
                body: if method == "HEAD" {
                    Vec::new()
                } else {
                    reply.body.clone()
                },
            },
            None => missing,
        }
    };
    drop(content);

    let mut out = stream;
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&reply.body);
    let _ = out.flush();
}

/// The two registries every test shares. The Docker configuration is set up
/// once, before anything reads it, with a login for the private one only.
fn registries() -> &'static (Registry, Registry) {
    static REGISTRIES: OnceLock<(Registry, Registry)> = OnceLock::new();
    REGISTRIES.get_or_init(|| {
        let (open, private) = (Registry::start(false), Registry::start(true));
        let config = Path::new(env!("CARGO_TARGET_TMPDIR")).join("registry-docker-config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("config.json"),
            format!(
                "{{\"auths\":{{\"{}\":{{\"auth\":\"{LOGIN}\"}}}}}}",
                private.host
            ),
        )
        .unwrap();
        std::env::set_var("DOCKER_CONFIG", &config);
        (open, private)
    })
}

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("registry")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a module directory and returns it.
fn module(root: &Path, name: &str, version: &str, marker: &str) -> PathBuf {
    let dir = root.join(format!("src-{}-{version}", name.replace('/', "-")));
    fs::create_dir_all(dir.join("documents")).unwrap();
    fs::write(
        dir.join("meridian.yaml"),
        format!("apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: {name}\n  version: {version}\n"),
    )
    .unwrap();
    fs::write(dir.join("documents/db.yaml"), format!("# {marker}\n")).unwrap();
    dir
}

fn site(root: &Path, entries: &str) -> PathBuf {
    let dir = root.join("site");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("meridian.yaml"),
        format!("apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: site\nspec:\n  modules:\n{entries}"),
    )
    .unwrap();
    dir
}

fn run(dir: &Path, mode: Mode) -> Result<meridian_module::Report, String> {
    install(
        &Project {
            dir,
            state_dir: &dir.join(".meridian"),
            core_version: CORE,
            signatures: &meridian_module::NO_SIGNATURES,
        },
        mode,
    )
    .map_err(|e| e.to_string())
}

fn entry(name: &str, registry: &Registry, repository: &str, req: &str) -> String {
    format!(
        "    - name: {name}\n      version: \"{req}\"\n      oci: {}/{repository}\n      insecure: true\n",
        registry.host
    )
}

#[test]
fn a_module_is_fetched_by_version_and_refetched_by_digest() {
    let root = scratch("fetch");
    let (registry, _) = registries();
    let mut digests = HashMap::new();
    for version in ["1.0.0", "1.2.0", "2.0.0"] {
        let dir = module(
            &root,
            "acme/postgres",
            version,
            &format!("postgres {version}"),
        );
        let tag = format!("v{version}");
        digests.insert(version, registry.publish("acme/postgres", &tag, &dir));
    }
    registry.tags("acme/postgres", &["v1.0.0", "v1.2.0", "v2.0.0", "latest"]);

    let dir = site(
        &root,
        &entry("acme/postgres", registry, "acme/postgres", "^1"),
    );
    let report = run(&dir, Mode::Install).unwrap();
    let pin = &report.lock.modules[0];
    assert_eq!(pin.version, "1.2.0");
    assert_eq!(pin.resolved, digests["1.2.0"]);
    assert_eq!(
        pin.source,
        format!("oci+http://{}/acme/postgres", registry.host)
    );
    // What was fetched hashes the same as the directory it was packed from.
    assert_eq!(
        pin.integrity,
        meridian_module::hash_dir(&root.join("src-acme-postgres-1.2.0")).unwrap()
    );
    let stored = dir.join(".meridian/modules/acme/postgres/1.2.0");
    assert_eq!(
        fs::read_to_string(stored.join("documents/db.yaml")).unwrap(),
        "# postgres 1.2.0\n"
    );

    // Installed and pinned: nothing more is asked of the registry.
    registry.requests("postgres");
    assert!(!run(&dir, Mode::Locked).unwrap().wrote_lock);
    assert_eq!(registry.requests("postgres"), Vec::<String>::new());

    // A machine with nothing fetched asks for the pinned digest, not the tag,
    // and does not list versions at all.
    fs::remove_dir_all(dir.join(".meridian")).unwrap();
    run(&dir, Mode::Locked).unwrap();
    let asked = registry.requests("postgres");
    assert_eq!(
        asked[0],
        format!("GET /v2/acme/postgres/manifests/{}", digests["1.2.0"])
    );
    assert!(asked.iter().all(|r| !r.contains("tags/list")), "{asked:?}");
    let project = Project {
        dir: &dir,
        state_dir: &dir.join(".meridian"),
        core_version: CORE,
        signatures: &meridian_module::NO_SIGNATURES,
    };
    Modules::load(&project).unwrap();

    // A new release is taken only by an update.
    let newer = module(&root, "acme/postgres", "1.3.0", "postgres 1.3.0");
    registry.publish("acme/postgres", "v1.3.0", &newer);
    registry.tags("acme/postgres", &["v1.0.0", "v1.2.0", "v1.3.0", "v2.0.0"]);
    assert_eq!(
        run(&dir, Mode::Install).unwrap().lock.modules[0].version,
        "1.2.0"
    );
    let report = run(&dir, Mode::Update(vec![])).unwrap();
    assert_eq!(report.lock.modules[0].version, "1.3.0");
    assert_eq!(
        report.changes[0].1,
        Change::Updated {
            from: "1.2.0".into()
        }
    );
}

#[test]
fn a_private_registry_is_read_with_the_login_docker_already_has() {
    let root = scratch("private");
    let (open, private) = registries();
    let dir = module(&root, "acme/private", "1.0.0", "private");
    private.publish("acme/private", "1.0.0", &dir);
    private.tags("acme/private", &["1.0.0"]);

    let project = site(&root, &entry("acme/private", private, "acme/private", "1"));
    let report = run(&project, Mode::Install).unwrap();
    assert_eq!(report.lock.modules[0].version, "1.0.0");
    let asked = private.requests("private");
    // Refused, a token fetched with the login, then the same request again.
    assert_eq!(asked[0], "GET /v2/acme/private/tags/list");
    assert!(
        asked[1].starts_with("GET /token?service=test-registry&scope="),
        "{asked:?}"
    );
    assert_eq!(asked[2], "GET /v2/acme/private/tags/list");

    // The same module on a registry there is no login for.
    let locked_out = Registry::start(true);
    locked_out.tags("acme/private", &["1.0.0"]);
    let project = site(
        &root,
        &entry("acme/private", &locked_out, "acme/private", "1"),
    );
    let _ = fs::remove_file(project.join(LOCK_FILE));
    assert_eq!(
        run(&project, Mode::Install).unwrap_err(),
        format!(
            "the registry {host} did not allow reading {host}/acme/private: no login for it \
             was found in the Docker configuration file; run `docker login` for that registry, \
             or name a credential for it in meridian.yaml",
            host = locked_out.host
        )
    );
    let _ = open;
}

#[test]
fn what_a_registry_serves_is_checked_against_what_was_asked_for() {
    let root = scratch("checked");
    let (registry, _) = registries();
    let good = module(&root, "acme/checked", "1.0.0", "as published");
    let install_fresh = |repository: &str| {
        let project = site(&root, &entry("acme/checked", registry, repository, "1"));
        let _ = fs::remove_file(project.join(LOCK_FILE));
        let _ = fs::remove_dir_all(project.join(".meridian"));
        run(&project, Mode::Install)
    };

    // A layer that is not the bytes its digest names.
    let digest = registry.publish("acme/swapped", "1.0.0", &good);
    registry.tags("acme/swapped", &["1.0.0"]);
    let layer = digest_of_layer(&good);
    registry.put(
        &format!("/v2/acme/swapped/blobs/{layer}"),
        Reply::new(
            200,
            pack(&module(&root, "acme/checked", "1.0.1", "swapped")).unwrap(),
        ),
    );
    let err = install_fresh("acme/swapped").unwrap_err();
    assert!(
        err.contains(&format!("answered a request for {layer} of"))
            && err.contains("with different content"),
        "{err}"
    );

    // A manifest that is not the bytes its digest names, on a re-fetch.
    registry.publish("acme/moved", "1.0.0", &good);
    registry.tags("acme/moved", &["1.0.0"]);
    let project = site(&root, &entry("acme/checked", registry, "acme/moved", "1"));
    let _ = fs::remove_file(project.join(LOCK_FILE));
    run(&project, Mode::Install).unwrap();
    fs::remove_dir_all(project.join(".meridian")).unwrap();
    let other = module(&root, "acme/checked", "1.0.2", "moved");
    let other_manifest = registry.publish("acme/moved-other", "1.0.0", &other);
    let stolen = registry.content.lock().unwrap().paths
        [&format!("/v2/acme/moved-other/manifests/{other_manifest}")]
        .body
        .clone();
    registry.put(
        &format!("/v2/acme/moved/manifests/{digest}"),
        Reply::new(200, stolen),
    );
    let err = run(&project, Mode::Install).unwrap_err();
    assert!(
        err.contains(&format!("answered a request for {digest} of"))
            && err.contains("with different content"),
        "{err}"
    );

    // An image that is not a module.
    registry.publish_layer(
        "acme/image",
        "1.0.0",
        b"layer",
        "application/vnd.oci.image.layer.v1.tar+gzip",
    );
    registry.tags("acme/image", &["1.0.0"]);
    let err = install_fresh("acme/image").unwrap_err();
    assert!(
        err.contains("is not a Meridian module: it has no layer of type application/vnd.meridian.module.layer.v1.tar+gzip (found: application/vnd.oci.image.layer.v1.tar+gzip)"),
        "{err}"
    );

    // A repository with no versions, and one that is not there.
    registry.tags("acme/empty", &["latest", "stable"]);
    assert_eq!(
        install_fresh("acme/empty").unwrap_err(),
        "no version of acme/checked satisfies every requirement on it\n    ^1   required by the \
         project\n    available: none: the source has no tags that are versions"
    );
    let err = install_fresh("acme/nowhere").unwrap_err();
    assert!(
        err.starts_with("the list of versions was not found in "),
        "{err}"
    );
}

/// The digest of the layer `pack` makes of `dir`.
fn digest_of_layer(dir: &Path) -> String {
    digest(&pack(dir).unwrap())
}

#[test]
fn a_long_list_of_versions_is_read_page_by_page() {
    let root = scratch("pages");
    let (registry, _) = registries();
    let dir = module(&root, "acme/paged", "3.1.0", "paged");
    registry.publish("acme/paged", "3.1.0", &dir);
    registry.put(
        "/v2/acme/paged/tags/list",
        Reply::new(200, r#"{"tags":["1.0.0","2.0.0"]}"#)
            .with("Link", "</v2/acme/paged/tags/page2>; rel=\"next\""),
    );
    registry.put(
        "/v2/acme/paged/tags/page2",
        Reply::new(200, r#"{"tags":["3.1.0"]}"#)
            // A link off this registry is never followed.
            .with(
                "Link",
                "<http://elsewhere.invalid/v2/x/tags/list>; rel=\"next\"",
            ),
    );
    let project = site(&root, &entry("acme/paged", registry, "acme/paged", ">=1"));
    assert_eq!(
        run(&project, Mode::Install).unwrap().lock.modules[0].version,
        "3.1.0"
    );
}

/// Publishes the module directory `dir` with `push`, the way the command does.
fn publish(
    registry: &Registry,
    repository: &str,
    dir: &Path,
) -> Result<meridian_module::Pushed, String> {
    let manifest = Manifest::load(dir).unwrap();
    push(
        &format!("{}/{repository}", registry.host),
        true,
        None,
        &manifest.name,
        manifest.version.as_ref().unwrap(),
        &manifest.description,
        &pack(dir).unwrap(),
    )
    .map_err(|e| e.to_string())
}

#[test]
fn a_pushed_module_installs_and_a_version_is_published_once() {
    let root = scratch("push");
    let (registry, private) = registries();
    let dir = module(&root, "acme/pushed", "1.0.0", "pushed");

    let pushed = publish(registry, "acme/pushed", &dir).unwrap();
    assert_eq!(pushed.tag, "1.0.0");
    assert!(!pushed.already_published);
    let asked = registry.requests("acme/pushed");
    // The empty configuration and the layer are each checked for, then
    // uploaded, and the manifest goes last.
    let methods: Vec<&str> = asked.iter().map(|r| r.split(' ').next().unwrap()).collect();
    assert_eq!(
        methods,
        ["GET", "HEAD", "POST", "PUT", "HEAD", "POST", "PUT", "PUT"],
        "{asked:?}"
    );
    assert_eq!(asked[7], "PUT /v2/acme/pushed/manifests/1.0.0");

    // What was pushed is what installs, with the hash of the directory.
    registry.tags("acme/pushed", &["1.0.0"]);
    let project = site(&root, &entry("acme/pushed", registry, "acme/pushed", "1"));
    let report = run(&project, Mode::Install).unwrap();
    let pin = &report.lock.modules[0];
    assert_eq!(pin.resolved, pushed.digest);
    assert_eq!(pin.integrity, meridian_module::hash_dir(&dir).unwrap());

    // The same content again uploads nothing.
    registry.requests("acme/pushed");
    let again = publish(registry, "acme/pushed", &dir).unwrap();
    assert!(again.already_published);
    assert_eq!(again.digest, pushed.digest);
    assert_eq!(
        registry.requests("acme/pushed"),
        ["GET /v2/acme/pushed/manifests/1.0.0"]
    );

    // Other content under the same version is refused, and nothing is sent.
    fs::write(dir.join("documents/db.yaml"), "# changed after release\n").unwrap();
    let err = publish(registry, "acme/pushed", &dir).unwrap_err();
    assert!(
        err.starts_with("acme/pushed 1.0.0 is already published in ")
            && err.ends_with("a version is published once. Release a new version instead"),
        "{err}"
    );
    assert_eq!(
        registry.requests("acme/pushed"),
        ["GET /v2/acme/pushed/manifests/1.0.0"]
    );

    // Build metadata cannot be in a tag, and comes back as the same version.
    let built = module(&root, "acme/pushed", "1.1.0+build.5", "built");
    assert_eq!(
        publish(registry, "acme/pushed", &built).unwrap().tag,
        "1.1.0_build.5"
    );
    registry.tags("acme/pushed", &["1.0.0", "1.1.0_build.5"]);
    let report = run(&project, Mode::Update(vec![])).unwrap();
    assert_eq!(report.lock.modules[0].version, "1.1.0+build.5");

    // A registry that wants a login takes the push with it, and refuses
    // without it.
    let secret = module(&root, "acme/private", "2.0.0", "pushed privately");
    assert!(
        !publish(private, "acme/private", &secret)
            .unwrap()
            .already_published
    );
    let locked_out = Registry::start(true);
    assert_eq!(
        publish(&locked_out, "acme/private", &secret).unwrap_err(),
        format!(
            "the registry {host} did not allow writing to {host}/acme/private: no login for it \
             was found in the Docker configuration file; run `docker login` for that registry, \
             or name a credential for it in meridian.yaml",
            host = locked_out.host
        )
    );
}

/// A file only its owner can read, holding the password the private
/// registry's login is made of.
fn login_file(root: &Path) -> PathBuf {
    let login = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(LOGIN)
            .unwrap(),
    )
    .unwrap();
    let path = root.join("registry-login");
    fs::write(&path, format!("{}\n", login.split_once(':').unwrap().1)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

/// A `spec.credentials` list with one credential, named `registry`.
fn declared(kind: &str, fields: &str, held_in: &Path) -> String {
    format!(
        "  credentials:\n    - name: registry\n      type: {kind}\n{fields}      password:\n        \
         from: file\n        path: {}\n",
        held_in.display()
    )
}

/// An `oci` requirement that names the credential `registry`.
fn entry_with_login(name: &str, registry: &Registry, repository: &str, req: &str) -> String {
    format!(
        "{}      credential: registry\n",
        entry(name, registry, repository, req)
    )
}

#[test]
fn a_registry_is_read_with_a_credential_the_project_declares() {
    let root = scratch("declared");
    let _ = registries();
    // A private registry the Docker configuration has no login for.
    let locked = Registry::start(true);
    let dir = module(&root, "acme/private", "1.0.0", "behind a declared login");
    locked.publish("acme/private", "1.0.0", &dir);
    locked.tags("acme/private", &["1.0.0"]);
    let kept = login_file(&root);
    let wanted = entry_with_login("acme/private", &locked, "acme/private", "1");

    // The server the credential is for is the registry: it is sent.
    let server = format!("      server: {}\n      user: reader\n", locked.host);
    let project = site(
        &root,
        &format!("{wanted}{}", declared("registry", &server, &kept)),
    );
    let report = run(&project, Mode::Install).unwrap();
    assert_eq!(report.lock.modules[0].version, "1.0.0");
    assert!(project
        .join(".meridian/modules/acme/private/1.0.0/documents/db.yaml")
        .is_file());
    // Nothing of the login is written down.
    let lock = fs::read_to_string(project.join(LOCK_FILE)).unwrap();
    let held = fs::read_to_string(&kept).unwrap();
    assert!(!lock.contains(held.trim()) && !lock.contains("reader"));

    // A usernamePassword credential names no server, and logs in too.
    let _ = fs::remove_dir_all(project.join(".meridian"));
    let _ = fs::remove_file(project.join(LOCK_FILE));
    site(
        &root,
        &format!(
            "{wanted}{}",
            declared("usernamePassword", "      user: reader\n", &kept)
        ),
    );
    assert_eq!(
        run(&project, Mode::Install).unwrap().lock.modules[0].version,
        "1.0.0"
    );
}

#[test]
fn a_declared_credential_goes_only_where_it_was_declared_for() {
    let root = scratch("declared-refused");
    let _ = registries();
    let locked = Registry::start(true);
    let dir = module(&root, "acme/private", "1.0.0", "behind a declared login");
    locked.publish("acme/private", "1.0.0", &dir);
    locked.tags("acme/private", &["1.0.0"]);
    let kept = login_file(&root);
    let wanted = entry_with_login("acme/private", &locked, "acme/private", "1");
    let host = &locked.host;
    let install = |credentials: String| {
        let project = site(&root, &format!("{wanted}{credentials}"));
        let _ = fs::remove_file(project.join(LOCK_FILE));
        run(&project, Mode::Install).unwrap_err()
    };

    // A login for another registry is not sent to this one.
    let before = locked.content.lock().unwrap().log.len();
    assert_eq!(
        install(declared(
            "registry",
            "      server: registry.example.com\n      user: reader\n",
            &kept
        )),
        format!(
            "the credential registry is for the registry registry.example.com, and \
             {host}/acme/private is on {host}; it was not sent"
        )
    );
    assert_eq!(locked.content.lock().unwrap().log.len(), before);

    // A login the registry does not accept is named, and not shown.
    let refused = install(declared(
        "registry",
        &format!("      server: {host}\n      user: someone-else\n"),
        &kept,
    ));
    assert_eq!(
        refused,
        format!(
            "the registry {host} did not allow reading {host}/acme/private: the credential \
             registry was refused"
        )
    );

    // A token is not a registry login.
    let token = root.join("token");
    fs::copy(&kept, &token).unwrap();
    assert_eq!(
        install(format!(
            "  credentials:\n    - name: registry\n      type: token\n      token:\n        \
             from: file\n        path: {}\n",
            token.display()
        )),
        "the credential registry is a token credential; a registry is logged in to with a \
         registry or a usernamePassword credential"
    );

    // A secret that cannot be read stops the install, and says which.
    let missing = install(declared(
        "registry",
        &format!("      server: {host}\n      user: reader\n"),
        &root.join("not-there"),
    ));
    assert!(
        missing.starts_with("the credential registry: ") && missing.contains("does not exist"),
        "{missing}"
    );
}

#[test]
fn a_fetched_module_cannot_have_a_credential_sent() {
    let root = scratch("declared-transitive");
    let (open, _) = registries();
    let locked = Registry::start(true);
    let inner = module(&root, "acme/private", "1.0.0", "behind a declared login");
    locked.publish("acme/private", "1.0.0", &inner);
    locked.tags("acme/private", &["1.0.0"]);
    let kept = login_file(&root);

    // A published module that declares a credential and names it for one of
    // its own requirements. Only the project's manifest is acted on.
    let outer = module(&root, "acme/outer", "1.0.0", "asks for a login");
    let mut manifest = fs::read_to_string(outer.join("meridian.yaml")).unwrap();
    manifest.push_str(&format!(
        "spec:\n  modules:\n{}{}",
        entry_with_login("acme/private", &locked, "acme/private", "1"),
        declared(
            "registry",
            &format!("      server: {}\n      user: reader\n", locked.host),
            &kept
        )
    ));
    fs::write(outer.join("meridian.yaml"), manifest).unwrap();
    open.publish("acme/outer-login", "1.0.0", &outer);
    open.tags("acme/outer-login", &["1.0.0"]);

    let project = site(&root, &entry("acme/outer", open, "acme/outer-login", "1"));
    assert_eq!(
        run(&project, Mode::Install).unwrap_err(),
        format!(
            "the registry {host} did not allow reading {host}/acme/private: no login for it \
             was found in the Docker configuration file; run `docker login` for that registry, \
             or name a credential for it in meridian.yaml",
            host = locked.host
        )
    );

    // The project names the login for that module itself, and it is used.
    site(
        &root,
        &format!(
            "{}{}{}",
            entry("acme/outer", open, "acme/outer-login", "1"),
            entry_with_login("acme/private", &locked, "acme/private", "1"),
            declared(
                "registry",
                &format!("      server: {}\n      user: reader\n", locked.host),
                &kept
            )
        ),
    );
    let report = run(&project, Mode::Install).unwrap();
    assert_eq!(report.lock.modules.len(), 2);
}

#[test]
fn a_module_is_pushed_with_a_declared_credential() {
    let root = scratch("declared-push");
    let _ = registries();
    let locked = Registry::start(true);
    let kept = login_file(&root);
    let dir = module(
        &root,
        "acme/private",
        "3.0.0",
        "pushed with a declared login",
    );
    let mut text = fs::read_to_string(dir.join("meridian.yaml")).unwrap();
    text.push_str(&format!(
        "spec:\n{}",
        declared(
            "registry",
            &format!("      server: {}\n      user: reader\n", locked.host),
            &kept
        )
    ));
    fs::write(dir.join("meridian.yaml"), text).unwrap();

    let manifest = Manifest::load(&dir).unwrap();
    let login = meridian_module::resolve_login(&manifest, "registry").unwrap();
    let pushed = push(
        &format!("{}/acme/private", locked.host),
        true,
        Some(&login),
        &manifest.name,
        manifest.version.as_ref().unwrap(),
        &manifest.description,
        &pack(&dir).unwrap(),
    )
    .unwrap();
    assert!(!pushed.already_published);
    assert_eq!(
        meridian_module::resolve_login(&manifest, "other")
            .unwrap_err()
            .to_string(),
        "the credential other: it is not declared (declared: registry)"
    );
}
