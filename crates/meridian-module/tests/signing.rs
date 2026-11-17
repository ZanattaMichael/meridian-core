//! Signed modules: a module signed in its repository, fetched by a project
//! that trusts the key, one that trusts another key, and one that does not
//! look.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_module::{
    hash_dir, install, sign, verify, Mode, Modules, Policy, Project, SecretKey, Signature,
    Verification, NO_SIGNATURES, SIGNATURE_FILE,
};

const CORE: &str = "0.1.0";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("signing")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=Meridian Tests"])
        .args(["-c", "user.email=tests@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn module_manifest(name: &str, version: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: {name}\n  version: {version}\n"
    )
}

/// A repository with `acme/web` 1.0.0, unsigned, and 1.1.0, signed by the
/// returned key; and a project that requires `^1.0` of it.
fn fixture(test: &str) -> (PathBuf, PathBuf, SecretKey) {
    let root = scratch(test);
    let repo = root.join("web.git");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    let key = SecretKey::generate().unwrap();
    for version in ["1.0.0", "1.1.0"] {
        write(
            &repo.join("meridian.yaml"),
            &module_manifest("acme/web", version),
        );
        write(&repo.join("documents/web.yaml"), "# the web document\n");
        if version == "1.1.0" {
            let before = hash_dir(&repo).unwrap();
            let signed = sign(&repo, &key).unwrap();
            assert_eq!(signed.name, "acme/web");
            assert_eq!(signed.version, "1.1.0");
            assert_eq!(signed.key_id, key.public().id());
            // Signing does not change what the module is pinned as.
            assert_eq!(signed.integrity, before);
            assert_eq!(hash_dir(&repo).unwrap(), before);
        }
        git(&repo, &["add", "--all"]);
        git(&repo, &["commit", "--quiet", "-m", version]);
        git(&repo, &["tag", &format!("v{version}")]);
    }
    let site = root.join("site");
    require(&site, &repo, "^1.0");
    (repo, site, key)
}

fn require(site: &Path, repo: &Path, version: &str) {
    write(
        &site.join("meridian.yaml"),
        &format!(
            "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: site\nspec:\n  modules:\n    - name: acme/web\n      version: \"{version}\"\n      git: file://{}\n",
            repo.display()
        ),
    );
}

fn project<'a>(site: &'a Path, state: &'a Path, signatures: &'a Verification) -> Project<'a> {
    Project {
        dir: site,
        state_dir: state,
        core_version: CORE,
        signatures,
    }
}

fn trust(policy: Policy, key: &SecretKey) -> Verification {
    Verification {
        policy,
        trusted: vec![key.public()],
    }
}

#[test]
fn a_signed_module_installs_and_loads_when_its_key_is_trusted() {
    let (_, site, key) = fixture("trusted");
    let state = site.join(".meridian");
    let required = trust(Policy::Require, &key);
    let project = project(&site, &state, &required);

    let report = install(&project, Mode::Install).unwrap();
    assert_eq!(report.lock.modules[0].version, "1.1.0");
    assert_eq!(
        report.signatures["acme/web"],
        Signature::Verified {
            key_id: key.public().id()
        }
    );
    assert!(report.warnings().is_empty());
    // The signature travelled with the module, and is not in its hash.
    let stored = state.join("modules/acme/web/1.1.0");
    assert!(stored.join(SIGNATURE_FILE).is_file());
    assert_eq!(hash_dir(&stored).unwrap(), report.lock.modules[0].integrity);

    let modules = Modules::load(&project).unwrap();
    assert!(modules.warnings().is_empty());
    let checks = verify(&project).unwrap();
    assert_eq!(checks[0].problem, None);
    assert_eq!(
        checks[0].signature,
        Signature::Verified {
            key_id: key.public().id()
        }
    );

    // With signatures off nothing is looked at, and nothing is reported.
    let off = project_off(&site, &state);
    assert!(install(&off, Mode::Locked).unwrap().signatures.is_empty());
    assert_eq!(verify(&off).unwrap()[0].signature, Signature::NotChecked);
}

fn project_off<'a>(site: &'a Path, state: &'a Path) -> Project<'a> {
    project(site, state, &NO_SIGNATURES)
}

#[test]
fn a_module_signed_by_an_untrusted_key_is_refused_or_warned_about() {
    let (_, site, key) = fixture("untrusted");
    let state = site.join(".meridian");
    let stranger = SecretKey::generate().unwrap();
    let id = key.public().id();

    let required = trust(Policy::Require, &stranger);
    let err = install(&project(&site, &state, &required), Mode::Install)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "acme/web 1.1.0: its signature does not pass: it was signed by key {id}, which is \
             not a trusted key. Module signatures are required, so it was not used; trust its \
             signer's key with --trusted-key"
        )
    );
    // A module that may not be used is not pinned.
    assert!(!site.join("meridian.lock").exists());

    let warned = trust(Policy::Warn, &stranger);
    let report = install(&project(&site, &state, &warned), Mode::Install).unwrap();
    let warning = format!(
        "acme/web 1.1.0: its signature does not pass: it was signed by key {id}, which is not \
         a trusted key"
    );
    assert_eq!(report.warnings(), std::slice::from_ref(&warning));
    assert_eq!(
        Modules::load(&project(&site, &state, &warned))
            .unwrap()
            .warnings(),
        [warning]
    );
    // Installed under warn, it still does not load where signatures are
    // required: the check is made every time a module is used.
    let err = Modules::load(&project(&site, &state, &required))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("acme/web 1.1.0: its signature does not pass"),
        "{err}"
    );
}

#[test]
fn an_unsigned_module_is_refused_where_signatures_are_required() {
    let (repo, site, key) = fixture("unsigned");
    let state = site.join(".meridian");
    require(&site, &repo, "=1.0.0");
    let required = trust(Policy::Require, &key);
    let err = install(&project(&site, &state, &required), Mode::Install)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "acme/web 1.0.0 is not signed: it holds no meridian.sig. Module signatures are \
         required; relax the check with --module-signatures warn, or ask its publisher to sign \
         it with `meridian module sign`"
    );
    let warned = trust(Policy::Warn, &key);
    let report = install(&project(&site, &state, &warned), Mode::Install).unwrap();
    assert_eq!(report.warnings(), ["acme/web 1.0.0 is not signed"]);
    assert_eq!(
        verify(&project(&site, &state, &warned)).unwrap()[0].signature,
        Signature::Unsigned
    );
}

#[test]
fn a_signature_vouches_for_one_module_one_version_and_one_content() {
    let (_, site, key) = fixture("bound");
    let state = site.join(".meridian");
    let required = trust(Policy::Require, &key);
    let project = project(&site, &state, &required);
    install(&project, Mode::Install).unwrap();
    let stored = state.join("modules/acme/web/1.1.0");
    let signature = fs::read_to_string(stored.join(SIGNATURE_FILE)).unwrap();

    // The same files under another name are another module: its signature
    // is not this one's.
    let other = scratch("bound-other");
    write(
        &other.join("meridian.yaml"),
        &module_manifest("acme/other", "1.1.0"),
    );
    write(&other.join("documents/web.yaml"), "# the web document\n");
    let theirs = sign(&other, &key).unwrap();
    assert_ne!(
        fs::read_to_string(other.join(SIGNATURE_FILE)).unwrap(),
        signature
    );
    assert_ne!(theirs.integrity, hash_dir(&stored).unwrap());

    // A signature edited after the fact does not pass. The content hash
    // does not cover the signature file, so this is the check that does.
    fs::write(
        stored.join(SIGNATURE_FILE),
        fs::read_to_string(other.join(SIGNATURE_FILE)).unwrap(),
    )
    .unwrap();
    let err = Modules::load(&project).unwrap_err().to_string();
    assert_eq!(
        err,
        "acme/web 1.1.0: its signature does not pass: the module is not the module that was \
         signed: its contents have changed. Module signatures are required, so it was not \
         used; trust its signer's key with --trusted-key"
    );
    fs::remove_file(stored.join(SIGNATURE_FILE)).unwrap();
    let err = Modules::load(&project).unwrap_err().to_string();
    assert!(err.starts_with("acme/web 1.1.0 is not signed"), "{err}");
}
