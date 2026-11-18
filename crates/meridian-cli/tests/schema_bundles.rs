//! Schema bundles from the command line: `meridian schema pack`, `push`,
//! `fetch`, `update` and `sync` taking a target's schemas from a registry's
//! bundles before it asks the tool. The registry here is a directory laid out
//! as an OCI one is (`file://`); the same code against a real OCI registry
//! runs in the modules workflow (tools/real_registry_test.sh).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SITE: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: "10.0.0.4"
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("schema_bundles")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_SCHEMA_SIGNATURES")
        .env_remove("MERIDIAN_SCHEMA_PREFER")
        .env_remove("MERIDIAN_TRUSTED_KEYS");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn ok(output: &Output) -> String {
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    out
}

fn failed(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A project that takes its schemas from the registry at `registry`.
fn project(dir: &Path, registry: &Path, signatures: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    fs::write(
        dir.join("meridian.yaml"),
        format!(
            "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: site\nspec:\n  schemas:\n    \
             registry: file://{}\n    signatures: {signatures}\n",
            registry.display()
        ),
    )
    .unwrap();
}

/// What the fake tool was asked, or nothing when it was not run.
fn tool_log(dir: &Path) -> String {
    fs::read_to_string(dir.join(".meridian/schemas/work/fake/schemas.log")).unwrap_or_default()
}

/// Syncs from the fake tool in `publisher`, packs the cache signed with a new
/// key, and publishes it to `registry`. Returns the public key's path.
fn publish(publisher: &Path, registry: &Path) -> PathBuf {
    fs::create_dir_all(publisher).unwrap();
    fs::write(publisher.join("site.yaml"), SITE).unwrap();
    ok(&meridian(
        publisher,
        &[
            "signing",
            "keygen",
            "--secret",
            "bundles.key",
            "--public",
            "bundles.pub",
        ],
        &[],
    ));
    let sync = ok(&meridian(
        publisher,
        &["schema", "sync", "--all-types", "site.yaml"],
        &[],
    ));
    assert!(sync.contains("fake: cached 1 resource type"), "{sync}");
    let packed = ok(&meridian(
        publisher,
        &[
            "schema",
            "pack",
            "fake",
            "--name",
            "fakelib",
            "--version",
            "2.3",
            "--key",
            "bundles.key",
            "--out",
            "fakelib-2.3.tar.gz",
        ],
        &[],
    ));
    assert!(
        packed.contains("packed fake/fakelib:2.3 (1 resource type from faketool 1.0; fakelib 2.3)"),
        "{packed}"
    );
    assert!(packed.contains("  signed:  key "), "{packed}");
    let location = format!("file://{}", registry.display());
    let pushed = ok(&meridian(
        publisher,
        &["schema", "push", "fakelib-2.3.tar.gz", &location],
        &[],
    ));
    assert!(
        pushed.contains("published fake/fakelib:2.3 (1 resource type)"),
        "{pushed}"
    );
    assert!(
        pushed.contains("listed in the registry's index"),
        "{pushed}"
    );
    // A version is published once; the same content again is a no-op.
    let again = ok(&meridian(
        publisher,
        &["schema", "push", "fakelib-2.3.tar.gz", &location],
        &[],
    ));
    assert!(again.contains("is already published"), "{again}");
    publisher.join("bundles.pub")
}

#[test]
fn a_project_takes_its_schemas_from_a_signed_bundle_without_running_the_tool() {
    let root = scratch("signed");
    let registry = root.join("registry");
    let key = publish(&root.join("publisher"), &registry);
    assert!(registry.join("index.json").is_file());
    assert!(registry.join("fake/fakelib/2.3.tar.gz").is_file());

    let site = root.join("site");
    project(&site, &registry, "require");
    let key_arg = key.to_string_lossy().into_owned();
    let sync = ok(&meridian(
        &site,
        &["schema", "sync", "--trusted-key", &key_arg, "site.yaml"],
        &[],
    ));
    assert!(
        sync.contains("fake: cached 1 resource type from the schema bundle fake/fakelib:2.3 in"),
        "{sync}"
    );
    assert_eq!(tool_log(&site), "", "the tool was not asked");
    let lock = fs::read_to_string(site.join("meridian.lock")).unwrap();
    assert!(lock.contains("    bundles:\n      - reference: "), "{lock}");
    assert!(
        lock.contains("/fake/fakelib/2.3.tar.gz\n        digest: "),
        "{lock}"
    );

    // The cache is the same bytes on every machine, so a check passes, and
    // lint reads it as it reads a cache the tool wrote.
    let check = ok(&meridian(
        &site,
        &["schema", "sync", "--check", "site.yaml"],
        &[],
    ));
    assert!(check.contains("fake: current"), "{check}");
    let status = ok(&meridian(&site, &["schema", "status"], &[]));
    assert!(
        status.contains("fetched from the schema bundle fake/fakelib:2.3: current"),
        "{status}"
    );
    fs::write(
        site.join("loose.yaml"),
        SITE.replace("        content: \"10.0.0.4\"\n", "        colour: blue\n"),
    )
    .unwrap();
    let lint = meridian(&site, &["lint", "loose.yaml"], &[]);
    assert!(
        String::from_utf8_lossy(&lint.stdout).contains("does not take param \"colour\""),
        "{}",
        String::from_utf8_lossy(&lint.stdout)
    );

    // A colleague's clone: --locked fetches exactly what the lock pins, from
    // the store or the registry, and gets the same cache.
    let first = fs::read(site.join(".meridian/schemas/fake.json")).unwrap();
    fs::remove_dir_all(site.join(".meridian")).unwrap();
    let locked = ok(&meridian(
        &site,
        &[
            "schema",
            "sync",
            "--locked",
            "--trusted-key",
            &key_arg,
            "site.yaml",
        ],
        &[],
    ));
    assert!(
        locked.contains("from the schema bundle fake/fakelib:2.3"),
        "{locked}"
    );
    assert_eq!(
        fs::read(site.join(".meridian/schemas/fake.json")).unwrap(),
        first
    );
    assert_eq!(tool_log(&site), "");

    // Without the publisher's key, a required signature does not pass and
    // nothing is written.
    fs::remove_dir_all(site.join(".meridian")).unwrap();
    let err = failed(&meridian(
        &site,
        &["schema", "sync", "--locked", "site.yaml"],
        &[],
    ));
    assert!(err.contains("not a trusted key"), "{err}");
    assert!(!site.join(".meridian/schemas/fake.json").exists());

    // --prefer tool asks the tool however many bundles there are.
    let tool = ok(&meridian(
        &site,
        &[
            "schema",
            "sync",
            "--prefer",
            "tool",
            "--no-lock",
            "site.yaml",
        ],
        &[],
    ));
    assert!(tool.contains("fake: asking the tool"), "{tool}");
    assert_eq!(tool_log(&site), "types: file\n");
}

#[test]
fn an_unsigned_bundle_is_refused_when_signatures_are_required_and_used_with_a_warning() {
    let root = scratch("unsigned");
    let publisher = root.join("publisher");
    fs::create_dir_all(&publisher).unwrap();
    fs::write(publisher.join("site.yaml"), SITE).unwrap();
    ok(&meridian(&publisher, &["schema", "sync", "site.yaml"], &[]));
    let packed = ok(&meridian(
        &publisher,
        &[
            "schema",
            "pack",
            "fake",
            "--name",
            "fakelib",
            "--version",
            "2.3",
        ],
        &[],
    ));
    assert!(packed.contains("signed:  no"), "{packed}");
    let archive = publisher.join(".meridian/schemas/packed/fake-fakelib-2.3.tar.gz");
    assert!(archive.is_file());
    let registry = root.join("registry");
    ok(&meridian(
        &publisher,
        &[
            "schema",
            "push",
            &archive.to_string_lossy(),
            &format!("file://{}", registry.display()),
        ],
        &[],
    ));

    let site = root.join("site");
    project(&site, &registry, "require");
    let err = failed(&meridian(&site, &["schema", "sync", "site.yaml"], &[]));
    assert!(err.contains("is not signed"), "{err}");
    assert!(!site.join("meridian.lock").exists());

    // The command line's policy wins over the manifest's.
    let sync = meridian(
        &site,
        &["schema", "sync", "--schema-signatures", "warn", "site.yaml"],
        &[],
    );
    ok(&sync);
    assert!(
        String::from_utf8_lossy(&sync.stderr)
            .contains("warning: the schema bundle fake/fakelib:2.3 is not signed"),
        "{}",
        String::from_utf8_lossy(&sync.stderr)
    );
}

#[test]
fn a_type_no_bundle_describes_is_asked_of_the_tool_and_fetch_writes_the_cache_directly() {
    let root = scratch("fallback");
    // A bundle that describes some other type of the fake tool's.
    let publisher = root.join("publisher");
    fs::create_dir_all(&publisher).unwrap();
    fs::write(publisher.join("site.yaml"), SITE).unwrap();
    ok(&meridian(&publisher, &["schema", "sync", "site.yaml"], &[]));
    let cache = publisher.join(".meridian/schemas/fake.json");
    fs::write(
        &cache,
        fs::read_to_string(&cache)
            .unwrap()
            .replace("\"type\": \"file\"", "\"type\": \"directory\""),
    )
    .unwrap();
    ok(&meridian(
        &publisher,
        &[
            "schema",
            "pack",
            "fake",
            "--name",
            "other",
            "--version",
            "1.0",
            "--out",
            "other.tar.gz",
        ],
        &[],
    ));
    let registry = root.join("registry");
    ok(&meridian(
        &publisher,
        &[
            "schema",
            "push",
            "other.tar.gz",
            &format!("file://{}", registry.display()),
        ],
        &[],
    ));

    let site = root.join("site");
    project(&site, &registry, "off");
    let sync = ok(&meridian(&site, &["schema", "sync", "site.yaml"], &[]));
    assert!(
        sync.contains("fake: no schema bundle describes file; asking the tool"),
        "{sync}"
    );
    assert_eq!(tool_log(&site), "types: file\n");
    let lock = fs::read_to_string(site.join("meridian.lock")).unwrap();
    assert!(!lock.contains("bundles:"), "{lock}");

    // --locked holds a tool-synced target to the tool and pins it recorded.
    ok(&meridian(
        &site,
        &["schema", "sync", "--locked", "site.yaml"],
        &[],
    ));

    // fetch writes the cache from one bundle, every type in full, and
    // records it.
    let archive = publisher.join("other.tar.gz");
    let fetched = ok(&meridian(
        &site,
        &["schema", "fetch", &archive.to_string_lossy()],
        &[],
    ));
    assert!(
        fetched.contains("fake: cached 1 resource type (1 in full) from the bundle fake/other:1.0"),
        "{fetched}"
    );
    let lock = fs::read_to_string(site.join("meridian.lock")).unwrap();
    assert!(
        lock.contains(&format!("reference: \"file+{}\"", archive.display()))
            || lock.contains(&format!("reference: file+{}", archive.display())),
        "{lock}"
    );
    // Asked for the documents' types, it lists the others by name.
    let fetched = ok(&meridian(
        &site,
        &[
            "schema",
            "fetch",
            &format!("file://{}", archive.display()),
            "site.yaml",
        ],
        &[],
    ));
    assert!(fetched.contains("(0 in full)"), "{fetched}");

    // update chooses again from the index.
    let updated = ok(&meridian(&site, &["schema", "update", "site.yaml"], &[]));
    assert!(
        updated.contains("no schema bundle describes file"),
        "{updated}"
    );
}

#[test]
fn a_bundle_name_and_version_are_checked_and_a_cache_is_needed_to_pack() {
    let root = scratch("refusals");
    fs::write(root.join("site.yaml"), SITE).unwrap();
    let err = failed(&meridian(
        &root,
        &[
            "schema",
            "pack",
            "fake",
            "--name",
            "fakelib",
            "--version",
            "2.3",
        ],
        &[],
    ));
    assert!(err.contains("nothing is cached for fake"), "{err}");
    ok(&meridian(&root, &["schema", "sync", "site.yaml"], &[]));
    for (name, version, want) in [
        ("Fake", "1", "cannot be a bundle name"),
        ("fake", "1+2", "cannot be a bundle version"),
    ] {
        let err = failed(&meridian(
            &root,
            &[
                "schema",
                "pack",
                "fake",
                "--name",
                name,
                "--version",
                version,
            ],
            &[],
        ));
        assert!(err.contains(want), "{err}");
    }
    let err = failed(&meridian(
        &root,
        &["schema", "fetch", "oci://ghcr.io/terraform:1"],
        &[],
    ));
    assert!(err.contains("is not a bundle in a registry"), "{err}");
    fs::remove_file(root.join("meridian.lock")).unwrap();
    let err = failed(&meridian(
        &root,
        &["schema", "sync", "--locked", "site.yaml"],
        &[],
    ));
    assert!(
        err.contains("there is no meridian.lock to hold to"),
        "{err}"
    );
}

#[test]
fn a_packed_bundle_is_signed_as_a_step_of_its_own_and_packed_by_prefix() {
    let root = scratch("sign");
    fs::write(root.join("site.yaml"), SITE).unwrap();
    ok(&meridian(&root, &["schema", "sync", "site.yaml"], &[]));
    let err = failed(&meridian(
        &root,
        &[
            "schema",
            "pack",
            "fake",
            "--name",
            "x",
            "--version",
            "1",
            "--prefix",
            "nothing.",
        ],
        &[],
    ));
    assert!(
        err.contains("no cached type of fake starts with nothing."),
        "{err}"
    );
    ok(&meridian(
        &root,
        &[
            "schema",
            "pack",
            "fake",
            "--name",
            "x",
            "--version",
            "1",
            "--prefix",
            "fi",
            "--out",
            "x.tar.gz",
        ],
        &[],
    ));
    ok(&meridian(
        &root,
        &[
            "signing", "keygen", "--secret", "k.key", "--public", "k.pub",
        ],
        &[],
    ));
    let err = failed(&meridian(
        &root,
        &[
            "schema",
            "fetch",
            "x.tar.gz",
            "--no-lock",
            "--schema-signatures",
            "require",
            "--trusted-key",
            "k.pub",
        ],
        &[],
    ));
    assert!(err.contains("is not signed"), "{err}");
    let signed = ok(&meridian(
        &root,
        &[
            "schema",
            "sign",
            "x.tar.gz",
            "--key",
            "k.key",
            "--out",
            "signed.tar.gz",
        ],
        &[],
    ));
    assert!(signed.contains("signed fake/x:1 with key "), "{signed}");
    ok(&meridian(
        &root,
        &[
            "schema",
            "fetch",
            "signed.tar.gz",
            "--no-lock",
            "--schema-signatures",
            "require",
            "--trusted-key",
            "k.pub",
        ],
        &[],
    ));
}
