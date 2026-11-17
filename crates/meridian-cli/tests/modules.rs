//! The `meridian module` commands and `@module:path`, run as a process
//! against a module on disk and a module in a real git repository.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("modules")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .output()
        .expect("meridian runs")
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = meridian(dir, args);
    assert!(
        out.status.success(),
        "meridian {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fails(dir: &Path, args: &[&str]) -> (String, String) {
    let out = meridian(dir, args);
    assert_eq!(out.status.code(), Some(1), "meridian {args:?} should fail");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Meridian Tests",
            "-c",
            "user.email=tests@example.com",
        ])
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

const MOTD: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: motd
spec:
  target: fake
  targetHost:
    from: inline
    host: web-1.example.com
  resources:
    - id: motd
      type: file
      params:
        content: "from the module\n"
    - id: banner
      type: file
      when: "data.banner == true"
      params:
        content: "banner\n"
"#;

/// A git repository holding the module `acme/motd` at 1.0.0 and 1.1.0, and a
/// project that requires it and a module on disk.
fn project(test: &str) -> (PathBuf, PathBuf) {
    let root = scratch(test);
    let repo = root.join("motd.git");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    for version in ["1.0.0", "1.1.0"] {
        write(
            &repo.join("meridian.yaml"),
            &format!(
                "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: acme/motd\n  version: {version}\n  description: A message of the day\n"
            ),
        );
        write(&repo.join("documents/motd.yaml"), MOTD);
        write(&repo.join("data/defaults.yaml"), "banner: true\n");
        git(&repo, &["add", "--all"]);
        git(&repo, &["commit", "--quiet", "-m", version]);
        git(&repo, &["tag", &format!("v{version}")]);
    }

    write(
        &root.join("local/meridian.yaml"),
        "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: local\n",
    );
    write(&root.join("local/data/site.yaml"), "banner: false\n");

    let site = root.join("site");
    write(
        &site.join("meridian.yaml"),
        &format!(
            "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: site\nspec:\n  modules:\n    - name: acme/motd\n      version: \"^1.0\"\n      git: file://{}\n    - name: local\n      path: ../local\n",
            repo.display()
        ),
    );
    (root, site)
}

#[test]
fn install_pins_modules_and_a_run_reads_documents_and_data_from_them() {
    let (root, site) = project("run");
    let repo = format!("git+file://{}", root.join("motd.git").display());

    // A run never fetches: before install it says what to do.
    let (_, err) = fails(&site, &["validate", "@acme/motd:documents/motd.yaml"]);
    assert_eq!(
        err,
        "error: the project requires modules and has no meridian.lock; run `meridian module install`\n"
    );

    assert_eq!(
        ok(&site, &["module", "install"]),
        format!("+ acme/motd 1.1.0 ({repo})\n+ local (path+../local)\nwrote meridian.lock\n")
    );
    assert_eq!(
        ok(&site, &["module", "install", "--locked"]),
        "2 modules unchanged\nmeridian.lock is up to date\n"
    );
    assert_eq!(
        ok(&site, &["module", "list"]),
        format!("acme/motd  1.1.0  {repo}\nlocal      -      path+../local\n")
    );
    assert_eq!(
        ok(&site, &["module", "verify"]),
        "ok      acme/motd 1.1.0\nok      local\n"
    );

    assert_eq!(
        ok(&site, &["validate", "@acme/motd:documents/motd.yaml"]),
        "1 document valid\n"
    );
    // The module's own data says the banner is on; the site's says off, and
    // a later file wins.
    let both = ok(
        &site,
        &[
            "plan",
            "@acme/motd:documents/motd.yaml",
            "--data",
            "@acme/motd:data/defaults.yaml",
        ],
    );
    assert!(
        both.contains("+ create files/motd") && both.contains("+ create files/banner"),
        "{both}"
    );
    let one = ok(
        &site,
        &[
            "plan",
            "@acme/motd:documents/motd.yaml",
            "--data",
            "@acme/motd:data/defaults.yaml",
            "--data",
            "@local:data/site.yaml",
        ],
    );
    assert!(
        one.contains("+ create files/motd") && !one.contains("files/banner"),
        "{one}"
    );

    let (_, err) = fails(&site, &["validate", "@acme/motd:documents/nope.yaml"]);
    assert_eq!(
        err,
        "error: @acme/motd:documents/nope.yaml: the module acme/motd has no file documents/nope.yaml\n"
    );
    let (_, err) = fails(&site, &["validate", "@acme/other:documents/motd.yaml"]);
    assert_eq!(
        err,
        "error: @acme/other:documents/motd.yaml: acme/other is not a module of this project (it has acme/motd, local)\n"
    );
}

#[test]
fn a_module_changed_after_it_was_pinned_stops_the_run() {
    let (_, site) = project("tamper");
    ok(&site, &["module", "install"]);
    let stored = site.join(".meridian/modules/acme/motd/1.1.0");
    fs::write(
        stored.join("documents/motd.yaml"),
        MOTD.replace("from the module", "from somewhere else"),
    )
    .unwrap();

    let (out, err) = fails(&site, &["plan", "@acme/motd:documents/motd.yaml"]);
    assert_eq!(out, "");
    assert!(
        err.starts_with("error: acme/motd 1.1.0 in .meridian/modules/acme/motd/1.1.0 does not match meridian.lock\n"),
        "{err}"
    );

    let (out, err) = fails(&site, &["module", "verify"]);
    assert!(
        out.starts_with("FAILED  acme/motd 1.1.0\n    acme/motd 1.1.0 in "),
        "{out}"
    );
    assert!(out.ends_with("ok      local\n"), "{out}");
    assert_eq!(err, "error: 1 module failed verification\n");

    // Installing again fetches what was pinned.
    ok(&site, &["module", "install", "--locked"]);
    ok(&site, &["module", "verify"]);
    ok(&site, &["validate", "@acme/motd:documents/motd.yaml"]);
}

#[test]
fn update_moves_a_pin_and_locked_refuses_to() {
    let (root, site) = project("update");
    // Pin 1.0.0 exactly first, then loosen the requirement.
    let manifest = fs::read_to_string(site.join("meridian.yaml")).unwrap();
    fs::write(
        site.join("meridian.yaml"),
        manifest.replace("^1.0", "=1.0.0"),
    )
    .unwrap();
    ok(&site, &["module", "install"]);
    fs::write(site.join("meridian.yaml"), &manifest).unwrap();

    // 1.0.0 still fits ^1.0, so nothing moves.
    assert_eq!(
        ok(&site, &["module", "install"]),
        "2 modules unchanged\nmeridian.lock is up to date\n"
    );
    assert_eq!(
        ok(&site, &["module", "update", "acme/motd"]),
        "~ acme/motd 1.0.0 -> 1.1.0\n1 module unchanged\nwrote meridian.lock\n"
    );

    fs::write(
        site.join("meridian.yaml"),
        manifest.replace("^1.0", "=1.0.0"),
    )
    .unwrap();
    let (_, err) = fails(&site, &["module", "install", "--locked"]);
    assert_eq!(
        err,
        "error: meridian.lock is out of date with meridian.yaml (acme/motd would change); run `meridian module install` and commit the result\n"
    );
    let (_, err) = fails(&site, &["module", "update", "nope"]);
    assert_eq!(
        err,
        "error: nope is not a module in meridian.lock, so there is nothing to update\n"
    );
    let _ = root;
}

#[test]
fn pack_writes_the_same_archive_every_time() {
    let (root, _) = project("pack");
    let module = root.join("motd.git");
    let out = ok(&module, &["module", "pack"]);
    assert!(
        out.starts_with(
            "packed acme/motd 1.1.0 into .meridian/packages/acme-motd-1.1.0.tar.gz\n  size:       "
        ),
        "{out}"
    );
    assert!(
        out.ends_with("  media type: application/vnd.meridian.module.layer.v1.tar+gzip\n"),
        "{out}"
    );
    let first = fs::read(module.join(".meridian/packages/acme-motd-1.1.0.tar.gz")).unwrap();
    // The archive it just wrote is under the state directory, which is not
    // part of the module, so packing again gives the same bytes.
    ok(&module, &["module", "pack", "--out", "../again.tar.gz"]);
    assert_eq!(fs::read(root.join("again.tar.gz")).unwrap(), first);

    // A project with no version is not something to publish.
    let (_, err) = fails(&root.join("local"), &["module", "pack"]);
    assert_eq!(
        err,
        "error: local has no metadata.version, and a module is published as a version\n"
    );
}

#[test]
fn a_signed_module_is_checked_on_install_and_on_every_run() {
    let (root, site) = project("signed");
    let repo = root.join("motd.git");
    let keys = root.join("keys");
    fs::create_dir_all(&keys).unwrap();
    // Paths to the two key files, not the keys.
    let (signer, public) = (keys.join("acme.key"), keys.join("acme.pub"));
    let (signer, public) = (signer.to_str().unwrap(), public.to_str().unwrap());
    ok(
        &root,
        &["signing", "keygen", "--secret", signer, "--public", public],
    );
    ok(
        &root,
        &[
            "signing",
            "keygen",
            "--secret",
            "keys/other.key",
            "--public",
            "keys/other.pub",
        ],
    );

    // 1.1.0 is published unsigned. Where signatures are required it is not
    // installed, and nothing is pinned.
    let (_, err) = fails(
        &site,
        &[
            "module",
            "install",
            "--module-signatures",
            "require",
            "--trusted-key",
            public,
        ],
    );
    assert_eq!(
        err,
        "error: acme/motd 1.1.0 is not signed: it holds no meridian.sig. Module signatures \
         are required; relax the check with --module-signatures warn, or ask its publisher to \
         sign it with `meridian module sign`\n"
    );
    assert!(!site.join("meridian.lock").exists());

    // The publisher signs the next release: sign last, then commit and tag.
    write(
        &repo.join("meridian.yaml"),
        "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: acme/motd\n  version: 1.2.0\n",
    );
    let out = ok(&repo, &["module", "sign", "--key", signer]);
    assert!(out.starts_with("signed acme/motd 1.2.0 with key "), "{out}");
    assert!(out.ends_with("  wrote:     meridian.sig\n"), "{out}");
    let key_id = out
        .lines()
        .next()
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .to_owned();
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "--quiet", "-m", "1.2.0"]);
    git(&repo, &["tag", "v1.2.0"]);

    let trusting = ["--module-signatures", "require", "--trusted-key", public];
    let with =
        |args: &[&'static str]| -> Vec<&str> { args.iter().copied().chain(trusting).collect() };
    let out = ok(&site, &with(&["module", "install"]));
    assert!(out.starts_with("+ acme/motd 1.2.0 ("), "{out}");
    assert_eq!(
        ok(&site, &with(&["module", "verify"])),
        format!("ok      acme/motd 1.2.0  signed by key {key_id}\nok      local\n")
    );
    assert_eq!(
        ok(
            &site,
            &with(&["validate", "@acme/motd:documents/motd.yaml"])
        ),
        "1 document valid\n"
    );
    // Signatures off is the default, and then nothing is said about them.
    assert_eq!(
        ok(&site, &["module", "verify"]),
        "ok      acme/motd 1.2.0\nok      local\n"
    );

    // Someone who trusts another key does not get to use it...
    let other = root.join("keys/other.pub");
    let other = other.to_str().unwrap();
    let (_, err) = fails(
        &site,
        &[
            "validate",
            "@acme/motd:documents/motd.yaml",
            "--module-signatures",
            "require",
            "--trusted-key",
            other,
        ],
    );
    assert_eq!(
        err,
        format!(
            "error: acme/motd 1.2.0: its signature does not pass: it was signed by key \
             {key_id}, which is not a trusted key. Module signatures are required, so it was \
             not used; trust its signer's key with --trusted-key\n"
        )
    );
    // ...unless they asked only to be told.
    let out = meridian(
        &site,
        &[
            "validate",
            "@acme/motd:documents/motd.yaml",
            "--module-signatures",
            "warn",
            "--trusted-key",
            other,
        ],
    );
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "warning: acme/motd 1.2.0: its signature does not pass: it was signed by key \
             {key_id}, which is not a trusted key\n"
        )
    );

    // A file changed after signing is caught by the pin, as before; a
    // signature swapped for another is caught by the signature.
    let stored = site.join(".meridian/modules/acme/motd/1.2.0");
    ok(&root.join("local"), &["module", "sign", "--key", signer]);
    fs::copy(root.join("local/meridian.sig"), stored.join("meridian.sig")).unwrap();
    let (out, _) = fails(&site, &with(&["module", "verify"]));
    assert!(
        out.starts_with(
            "FAILED  acme/motd 1.2.0\n    acme/motd 1.2.0: its signature does not pass: the \
             module is not the module that was signed"
        ),
        "{out}"
    );
}

#[test]
fn the_modules_example_runs_as_its_readme_says() {
    use meridian_plugin_testkit::{plugin_binary, repo_root};
    let site = repo_root().join("examples/modules/site");
    let state = scratch("example");
    let state = state.to_str().unwrap();
    let plugins = plugin_binary("meridian-target-ansible");
    let plugins = plugins.parent().unwrap().to_str().unwrap();

    // The committed lockfile is the one install writes.
    assert_eq!(
        ok(
            &site,
            &["module", "install", "--locked", "--state-dir", state]
        ),
        "1 module unchanged\nmeridian.lock is up to date\n"
    );
    let out = ok(
        &site,
        &[
            "compile",
            "web.yaml",
            "@acme/hardening:documents/ssh.yaml",
            "--data",
            "@acme/hardening:data/defaults.yaml",
            "--data",
            "data.yaml",
            "--state-dir",
            state,
            "--plugin-dir",
            plugins,
            "--out",
            &format!("{state}/out"),
        ],
    );
    assert!(
        out.contains("web (ResourceSet, target ansible on web01.example.com)"),
        "{out}"
    );
    assert!(
        out.contains("ssh-baseline (ResourceSet, target ansible on web01.example.com)"),
        "{out}"
    );
    // The module's default data decided the condition.
    let playbook = fs::read_to_string(format!("{state}/out/ssh-baseline/playbook.yml")).unwrap();
    assert!(
        playbook.contains("/etc/ssh/sshd_config.d/10-no-root.conf"),
        "{playbook}"
    );
}

#[test]
fn push_takes_a_declared_credential_and_a_secret_is_fetched_only_when_used() {
    let root = scratch("push-credential");
    write(
        &root.join("meridian.yaml"),
        "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: acme/login\n  version: 1.0.0\n\
         spec:\n  credentials:\n    - name: internal\n      type: registry\n      \
         server: registry.example.com\n      user: ci\n      password:\n        from: env\n        \
         key: MERIDIAN_TEST_A_VARIABLE_THAT_IS_NOT_SET\n",
    );
    // Nothing names the credential, so nothing fetches its secret.
    ok(&root, &["module", "install"]);
    ok(&root, &["module", "verify"]);
    ok(&root, &["module", "pack"]);

    // Both refusals come before anything is sent to the registry.
    let repository = "registry.example.com/acme/login";
    let (_, err) = fails(
        &root,
        &["module", "push", repository, "--credential", "other"],
    );
    assert_eq!(
        err,
        "error: the credential other: it is not declared (declared: internal)\n"
    );
    let (_, err) = fails(
        &root,
        &["module", "push", repository, "--credential", "internal"],
    );
    assert!(
        err.starts_with("error: the credential internal: ")
            && err.contains("MERIDIAN_TEST_A_VARIABLE_THAT_IS_NOT_SET"),
        "{err}"
    );
}
