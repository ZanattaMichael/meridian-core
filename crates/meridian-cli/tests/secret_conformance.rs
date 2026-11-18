//! A secret handed to a tool, on every target.
//!
//! Each target's own example is given a secret, in a param and as the key
//! of a credential, and compiled by the real command with the real plugin.
//! Whatever the target, three things must hold:
//!
//! - the value is in nothing the command printed and in no file it wrote;
//! - a target that takes a secret writes a reference to it by name, and
//!   one that does not writes the secret's name and is stopped by `plan`
//!   with a message that says which targets do;
//! - a credential reaches the host only on a target that can, and is
//!   refused before anything runs on one that cannot.
//!
//! The lists of targets that take a secret and a credential are written
//! here on purpose: a target that gains the capability fails this test
//! until it is added, which is the moment to read what it writes.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_testkit::{plugin_binary, repo_root, scratch};

/// Unlike anything a document or a target writes, so that finding it is
/// finding a leak.
const VALUE: &str = "zzq-Vt9-not-a-real-password-4kLm";
const KEY: &str = "zzq-not-a-real-private-key-7Hq2\n";

/// The targets that hand a secret in a param to their tool by a private
/// channel, and the reference each compiles in its place.
const TAKE_A_SECRET: &[(&str, &str)] = &[
    ("ansible", "meridian_secret_db_password"),
    ("arm", "[parameters('meridian_secret_db_password')]"),
    ("bicep", "meridian_secret_db_password"),
    ("chef", "node['meridian_secret_db_password']"),
    ("cloudformation", "MeridianSecretDbPassword"),
    ("docker", "${meridian_secret_db_password}"),
    ("helm", "${meridian_secret_db_password}"),
    ("kubernetes", "${meridian_secret_db_password}"),
    (
        "openvox",
        "Sensitive(lookup('meridian_secret_db_password'))",
    ),
    ("opentofu", "var.meridian_secret_db_password"),
    ("packer", "var.meridian_secret_db_password"),
    (
        "podman",
        "meridian_secret_db_password,type=env,target=PASSWORD",
    ),
    ("pulumi", "${meridian_secret_db_password}"),
    ("puppet", "Sensitive(lookup('meridian_secret_db_password'))"),
    (
        "salt",
        "{{ pillar['meridian_secret_db_password'] | yaml_encode }}",
    ),
    ("terraform", "var.meridian_secret_db_password"),
];

/// Where a target's example has no scalar param a secret may go in, because
/// every one is a name, a setting or a field of an object that is written
/// as it stands: a resource of the target's own to add to the example, with
/// `VALUE` where the secret goes. The place is the one the target documents.
const PLACE_FOR_A_SECRET: &[(&str, &str)] = &[
    (
        "kubernetes",
        "    - id: conformance_login\n      type: v1/Secret\n      params:\n        stringData:\n          \
         password: VALUE\n",
    ),
    (
        "podman",
        "    - id: conformance_probe\n      type: container\n      params:\n        Image: \
         docker.io/library/busybox:stable\n        Environment:\n          PASSWORD: VALUE\n",
    ),
];

/// The targets that reach a host as a credential, in name order, and
/// whether the artifact names its user. Ansible writes the user into the
/// inventory; the others hand it to their tool at run time (an ssh option,
/// a roster or inventory written for the run) and compile nothing of it.
const TAKE_A_CREDENTIAL: &[(&str, bool)] = &[
    ("ansible", true),
    ("chef", false),
    ("openvox", false),
    ("podman", false),
    ("puppet", false),
    ("salt", false),
];

/// The targets, each with the examples of its own that hold one document
/// and decide nothing from data, in name order.
fn examples() -> Vec<(String, Vec<PathBuf>)> {
    let root = repo_root();
    let open = ["$", "{{"].concat();
    let mut found = Vec::new();
    for plugin in fs::read_dir(root.join("plugins")).unwrap() {
        let target = plugin.unwrap().file_name().to_string_lossy().into_owned();
        let Ok(files) = fs::read_dir(root.join("examples").join(&target)) else {
            continue;
        };
        let mut files: Vec<PathBuf> = files
            .map(|file| file.unwrap().path())
            .filter(|file| file.extension().is_some_and(|e| e == "yaml"))
            .collect();
        files.sort();
        let plain: Vec<PathBuf> = files
            .into_iter()
            .filter(|file| {
                let text = fs::read_to_string(file).unwrap();
                text.lines()
                    .filter(|line| line.starts_with("  target: "))
                    .count()
                    == 1
                    && !text.contains(&open)
                    && !text
                        .lines()
                        .any(|line| line.trim_start().starts_with("when:"))
            })
            .collect();
        if !plain.is_empty() {
            found.push((target, plain));
        }
    }
    found.sort();
    found
}

const SECRETS: &str = "  secrets:\n    - name: db_password\n      from: env\n      key: \
                       CONFORMANCE_PASSWORD\n    - name: deploy_key\n      from: env\n      key: \
                       CONFORMANCE_KEY\n";

/// The example with the secrets declared, and one of its text params
/// replaced by `value`. `index` picks which param, in document order. For a
/// target in [`PLACE_FOR_A_SECRET`], the one place is that resource, added
/// to the example's resources.
fn with_secret_in_param(target: &str, example: &str, index: usize, value: &str) -> Option<String> {
    if let Some((_, place)) = PLACE_FOR_A_SECRET.iter().find(|(name, _)| *name == target) {
        if index > 0 {
            return None;
        }
        return Some(with_resource(example, &place.replace("VALUE", value)));
    }
    let mut out = String::new();
    let mut declared = false;
    let mut seen = 0;
    let mut replaced = false;
    for line in example.lines() {
        if !declared && line.starts_with("  target: ") {
            out.push_str(line);
            out.push('\n');
            out.push_str(SECRETS);
            declared = true;
            continue;
        }
        // A scalar param: eight spaces, a key, and text that is not the
        // start of a mapping, a list or a block.
        let scalar = line
            .strip_prefix("        ")
            .and_then(|rest| rest.split_once(": "))
            .is_some_and(|(key, rest)| {
                !key.starts_with([' ', '-', '#'])
                    && !key.contains(' ')
                    && !rest.is_empty()
                    && !rest.starts_with(['[', '{', '|', '>', '#'])
            });
        if scalar && !replaced {
            if seen == index {
                let (key, _) = line.trim_start().split_once(": ").unwrap();
                out.push_str(&format!("        {key}: {value}\n"));
                replaced = true;
                continue;
            }
            seen += 1;
        }
        out.push_str(line);
        out.push('\n');
    }
    (declared && replaced).then_some(out)
}

/// The example with the secrets declared and `resource` added at the end of
/// its resources, which end at the next key of the spec or at the end.
fn with_resource(example: &str, resource: &str) -> String {
    let mut out = String::new();
    let mut in_resources = false;
    let mut added = false;
    for line in example.lines() {
        if line.starts_with("  target: ") {
            out.push_str(line);
            out.push('\n');
            out.push_str(SECRETS);
            continue;
        }
        if line.starts_with("  resources:") {
            in_resources = true;
        } else if in_resources
            && !added
            && line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.trim().is_empty()
        {
            out.push_str(resource);
            added = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !added {
        out.push_str(resource);
    }
    out
}

/// The example with a credential declared whose key is a secret, named by
/// the host where the document has one.
fn with_credential(example: &str) -> (String, bool) {
    let mut out = String::new();
    let mut declared = false;
    let mut named = false;
    for line in example.lines() {
        out.push_str(line);
        out.push('\n');
        if !declared && line.starts_with("  target: ") {
            out.push_str(SECRETS);
            out.push_str(
                "  credentials:\n    - name: deploy\n      type: ssh\n      user: deploy\n      \
                 privateKey: ${{ secret.deploy_key }}\n",
            );
            declared = true;
        }
        if line == "  targetHost:" {
            out.push_str("    credential: deploy\n");
            named = true;
        }
    }
    assert!(declared, "no spec.target to declare beside");
    (out, named)
}

/// The directories every target's plugin was built into.
fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = examples()
        .iter()
        .map(|(target, _)| {
            plugin_binary(&format!("meridian-target-{target}"))
                .parent()
                .unwrap()
                .to_path_buf()
        })
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

struct Outcome {
    succeeded: bool,
    said: String,
    files: BTreeMap<String, String>,
}

fn meridian(dir: &Path, command: &str, document: &str) -> Outcome {
    let _ = fs::remove_dir_all(dir.join("out"));
    let _ = fs::remove_dir_all(dir.join(".meridian"));
    fs::write(dir.join("doc.yaml"), document).unwrap();
    let mut meridian = Command::new(env!("CARGO_BIN_EXE_meridian"));
    meridian.env("MERIDIAN_ANNOTATIONS", "off");
    meridian.args([command, "doc.yaml"]);
    if command != "validate" {
        // Every plugin is loaded, so a refusal can say which targets take
        // what this one does not.
        for plugin in plugin_dirs() {
            meridian.arg("--plugin-dir").arg(plugin);
        }
    }
    meridian
        .current_dir(dir)
        .env_remove("MERIDIAN_STATE_DIR")
        .env("CONFORMANCE_PASSWORD", VALUE)
        .env("CONFORMANCE_KEY", KEY);
    if command == "compile" {
        meridian.args(["--out", "out"]);
    }
    let output = meridian.output().expect("meridian runs");
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if !path.ends_with("doc.yaml") {
                let name = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.insert(
                    name,
                    String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned(),
                );
            }
        }
    }
    Outcome {
        succeeded: output.status.success(),
        said: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        files,
    }
}

/// Nothing the run printed or wrote may hold either value.
fn nothing_leaked(at: &str, outcome: &Outcome) {
    for secret in [VALUE, KEY.trim_end()] {
        assert!(
            !outcome.said.contains(secret),
            "{at}: the secret was printed:\n{}",
            outcome.said
        );
        for (path, text) in &outcome.files {
            assert!(!text.contains(secret), "{at}: {path} holds the secret");
        }
    }
}

fn takers() -> String {
    let mut names: Vec<&str> = TAKE_A_SECRET.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.join(", ")
}

/// One target: the secret in a param, bare and then inside text. The first
/// of the target's examples that takes the secret somewhere is the one
/// checked; an example may have no place for one, such as a Helm chart with
/// a Kustomize overlay, and that is the target's own rule.
fn secret_in_a_param(target: &str, files: &[PathBuf]) -> Vec<String> {
    let mut failures = Vec::new();
    let examples: Vec<String> = files
        .iter()
        .map(|file| fs::read_to_string(file).unwrap())
        .collect();
    let dir = scratch(
        env!("CARGO_TARGET_TMPDIR"),
        &format!("secret-conformance-{target}"),
    );
    let bare = ["$", "{{ secret.db_password }}"].concat();
    let derived = "\"db=${{ secret.db_password }}\"".to_string();
    let reference = TAKE_A_SECRET
        .iter()
        .find(|(name, _)| *name == target)
        .map(|(_, reference)| *reference);

    for (what, value, written) in [
        ("bare", bare.as_str(), reference.map(str::to_owned)),
        (
            "derived",
            derived.as_str(),
            // CloudFormation spells the name without its underscores.
            reference.map(|r| {
                r.replace("db_password", "derived_1")
                    .replace("DbPassword", "Derived1")
            }),
        ),
    ] {
        // The first text param the target accepts the secret in. A name or
        // a path may refuse what is written there; that is the target's own
        // rule and not this test's business, as long as nothing leaks.
        let mut compiled = None;
        'examples: for example in &examples {
            for index in 0.. {
                let Some(document) = with_secret_in_param(target, example, index, value) else {
                    break;
                };
                let outcome = meridian(&dir, "compile", &document);
                nothing_leaked(
                    &format!("{target}: {what}, param {index}, compile"),
                    &outcome,
                );
                if outcome.succeeded {
                    compiled = Some((document, outcome));
                    break 'examples;
                }
            }
        }
        let Some((document, outcome)) = compiled else {
            failures.push(format!("{target}: no text param takes a {what} secret"));
            continue;
        };
        let artifact: String = outcome.files.values().cloned().collect();
        match &written {
            Some(reference) => {
                if !artifact.contains(reference) {
                    failures.push(format!(
                        "{target}: {what}: the artifact does not refer to the secret as \
                         {reference}:\n{artifact}"
                    ));
                }
                if artifact.contains("(secret ") {
                    failures.push(format!(
                        "{target}: {what}: the artifact names the secret as text:\n{artifact}"
                    ));
                }
            }
            None => {
                let name = if what == "bare" {
                    "(secret db_password)"
                } else {
                    "(secret "
                };
                if !artifact.contains(name) {
                    failures.push(format!(
                        "{target}: {what}: compile must write the secret's name:\n{artifact}"
                    ));
                }
                // plan stops before the tool is started, and says who takes one.
                let planned = meridian(&dir, "plan", &document);
                nothing_leaked(&format!("{target}: {what}, plan"), &planned);
                let expected = format!("holds a secret, and target \"{target}\" does not take one");
                if planned.succeeded
                    || !planned.said.contains(&expected)
                    || !planned
                        .said
                        .contains(&format!("targets that do: {}", takers()))
                {
                    failures.push(format!(
                        "{target}: {what}: plan must stop and name the targets that take a \
                         secret:\n{}",
                        planned.said
                    ));
                }
            }
        }
    }
    failures
}

/// One target: a credential whose key is a secret, in its first example.
fn credential(target: &str, files: &[PathBuf]) -> Vec<String> {
    let mut failures = Vec::new();
    let example = fs::read_to_string(&files[0]).unwrap();
    let dir = scratch(
        env!("CARGO_TARGET_TMPDIR"),
        &format!("credential-conformance-{target}"),
    );
    let (document, named) = with_credential(&example);
    let takes = TAKE_A_CREDENTIAL.iter().find(|(name, _)| *name == target);
    let names_user = takes.is_some_and(|(_, names_user)| *names_user);
    let takes = takes.is_some();

    let validated = meridian(&dir, "validate", &document);
    nothing_leaked(&format!("{target}: validate"), &validated);
    if !validated.succeeded {
        failures.push(format!(
            "{target}: validate refused the credential:\n{}",
            validated.said
        ));
    }

    for command in ["compile", "plan"] {
        let outcome = meridian(&dir, command, &document);
        nothing_leaked(&format!("{target}: {command}"), &outcome);
        let artifact: String = outcome.files.values().cloned().collect();
        if named && !takes {
            let expected = format!(
                "spec.targetHost names the credential \"deploy\", and target \"{target}\" does \
                 not take one"
            );
            if outcome.succeeded || !outcome.said.contains(&expected) {
                failures.push(format!(
                    "{target}: {command} must refuse the credential before anything runs:\n{}",
                    outcome.said
                ));
            }
            let takers: Vec<&str> = TAKE_A_CREDENTIAL.iter().map(|(name, _)| *name).collect();
            if !outcome
                .said
                .contains(&format!("targets that do: {}", takers.join(", ")))
            {
                failures.push(format!(
                    "{target}: {command}: the refusal must say who takes one:\n{}",
                    outcome.said
                ));
            }
        } else if command == "compile" {
            // A credential nobody names is checked and kept; one the host
            // names on a target that takes it is written as the tool reads
            // it, without its key.
            if !outcome.succeeded {
                failures.push(format!(
                    "{target}: compile with a credential:\n{}",
                    outcome.said
                ));
            }
            if named && names_user && !artifact.contains("deploy") {
                failures.push(format!(
                    "{target}: the artifact does not name the user:\n{artifact}"
                ));
            }
            if artifact.contains("deploy_key") {
                failures.push(format!(
                    "{target}: the artifact names the secret:\n{artifact}"
                ));
            }
        }
    }
    failures
}

fn all(check: fn(&str, &[PathBuf]) -> Vec<String>) -> Vec<String> {
    let examples = examples();
    let targets = fs::read_dir(repo_root().join("plugins")).unwrap().count();
    assert_eq!(
        examples.len(),
        targets,
        "every target needs an example of its own"
    );
    std::thread::scope(|scope| {
        let running: Vec<_> = examples
            .iter()
            .map(|(target, files)| scope.spawn(move || check(target, files)))
            .collect();
        running
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect()
    })
}

#[test]
fn a_secret_in_a_param_reaches_a_tool_by_name_on_every_target_and_its_value_reaches_no_file() {
    let failures = all(secret_in_a_param);
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_credential_reaches_a_host_only_where_the_target_can_and_its_key_reaches_no_file() {
    let failures = all(credential);
    assert!(failures.is_empty(), "{failures:#?}");
}
