//! `spec.credentials` and `targetHost.credential` end to end: a document is
//! checked without fetching anything, the credential's secret fields reach
//! the tool by a private channel when the document is planned or applied,
//! and nothing the run prints or writes holds them.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-Q9fXw\n-----END OPENSSH PRIVATE KEY-----\n";
const PASSWORD: &str = "kq7Zp2-not-a-real-password-9fXw";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("credentials-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the command with the key and the password in the variables the
/// documents read, and with any further variables.
fn meridian_with(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env("DEPLOY_KEY", KEY)
        .env("DEPLOY_PASSWORD", PASSWORD);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    meridian_with(dir, args, &[])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn said(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

/// Every file under `dir`, with its content.
fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else if let Ok(bytes) = fs::read(&path) {
            found.push((path, bytes));
        }
    }
    found
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Asserts no part of the key or the password is in what the run printed
/// or in any file under `dir` other than the ones named.
fn nothing_leaked(output: &Output, dir: &Path, except: &[&str]) {
    for secret in ["not-a-real-key", PASSWORD] {
        assert!(
            !stdout(output).contains(secret),
            "stdout:\n{}",
            stdout(output)
        );
        assert!(
            !stderr(output).contains(secret),
            "stderr:\n{}",
            stderr(output)
        );
        for (path, bytes) in files_under(dir) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if except.contains(&name.as_str()) {
                continue;
            }
            assert!(!holds(&bytes, secret), "{} holds a secret", path.display());
        }
    }
}

/// A document reaching its host as `deploy`, an SSH credential whose key is
/// a secret read from the environment.
const BY_KEY: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: fake
  targetHost:
    from: inline
    host: web01
    credential: deploy
  secrets:
    - name: deploy_key
      from: env
      key: DEPLOY_KEY
    - name: deploy_password
      from: env
      key: DEPLOY_PASSWORD
  credentials:
    - name: deploy
      type: ssh
      user: deploy
      privateKey: "${{ secret.deploy_key }}"
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
"#;

fn by_key(test: &str) -> PathBuf {
    let dir = scratch(test);
    fs::write(dir.join("site.yaml"), BY_KEY).unwrap();
    dir
}

fn sink(dir: &Path) -> PathBuf {
    dir.join("sink.json")
}

#[test]
fn validate_checks_a_credential_against_its_type_and_fetches_nothing() {
    let dir = by_key("validate");
    // Valid, and the key is not read: the variable is not even there.
    let out = meridian_with(
        &dir,
        &["validate", "site.yaml"],
        &[("DEPLOY_KEY", ""), ("DEPLOY_PASSWORD", "")],
    );
    assert!(out.status.success(), "{}", said(&out));

    // A literal where a secret must be is refused, and not repeated.
    let literal = BY_KEY.replace(
        "privateKey: \"${{ secret.deploy_key }}\"",
        "password: hunter2-written-in-the-document",
    );
    fs::write(dir.join("site.yaml"), &literal).unwrap();
    let out = meridian(&dir, &["validate", "site.yaml"]);
    assert!(!out.status.success());
    let text = said(&out);
    assert!(
        text.contains("spec.credentials[0].password")
            && text.contains("a password is written in the document; declare a secret"),
        "{text}"
    );
    assert!(!text.contains("hunter2"), "{text}");

    // A host takes an ssh or usernamePassword credential only.
    let cloud = BY_KEY
        .replace("type: ssh", "type: aws")
        .replace("user: deploy", "profile: ci")
        .replace("      privateKey: \"${{ secret.deploy_key }}\"\n", "");
    fs::write(dir.join("site.yaml"), &cloud).unwrap();
    let out = meridian(&dir, &["validate", "site.yaml"]);
    assert!(!out.status.success());
    assert!(
        said(&out)
            .contains("a host takes an ssh or usernamePassword credential; \"deploy\" is aws"),
        "{}",
        said(&out)
    );

    // An incomplete one says what would complete it.
    let incomplete = BY_KEY.replace("      privateKey: \"${{ secret.deploy_key }}\"\n", "");
    fs::write(dir.join("site.yaml"), &incomplete).unwrap();
    let out = meridian(&dir, &["validate", "site.yaml"]);
    assert!(!out.status.success());
    assert!(
        said(&out).contains(
            "an ssh credential needs privateKey, or password, or agent: true; or from: \
             ambient, for one the tool finds itself"
        ),
        "{}",
        said(&out)
    );
}

#[test]
fn compile_writes_what_the_target_knows_of_the_credential_and_never_a_value() {
    let dir = by_key("compile");
    let out = meridian(&dir, &["compile", "site.yaml", "--out", "out"]);
    assert!(out.status.success(), "{}", said(&out));
    nothing_leaked(&out, &dir, &[]);
    let written: Vec<(String, String)> = files_under(&dir.join("out"))
        .into_iter()
        .map(|(path, bytes)| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        })
        .collect();
    assert!(
        written.iter().any(|(name, text)| name == "credential.txt"
            && text == "deploy (ssh, user deploy)\nsecret fields: privateKey\n"),
        "{written:#?}"
    );
}

#[test]
fn plan_and_apply_hand_the_credential_to_the_tool_in_a_private_file_and_nowhere_else() {
    let dir = by_key("handed");
    let sink = sink(&dir);
    for command in ["plan", "apply"] {
        let _ = fs::remove_file(&sink);
        let out = meridian_with(
            &dir,
            &[command, "site.yaml"],
            &[("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap())],
        );
        assert!(out.status.success(), "{command}: {}", said(&out));
        // The tool got the key, in a file readable by the user alone.
        let handed = fs::read_to_string(&sink).unwrap();
        assert_eq!(
            handed,
            format!("{{\"privateKey\":\"{}\"}}\n", KEY.replace('\n', "\\n")),
            "{command}"
        );
        let mode = fs::metadata(&sink).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{command}");
        // The plan says who the host is reached as, by name only.
        let text = said(&out);
        assert!(
            text.contains("on web01 as deploy (ssh, user deploy)"),
            "{command}: {text}"
        );
        assert!(
            text.contains("credential deploy (ssh, user deploy) handed over in a private file"),
            "{command}: {text}"
        );
        // And nothing else holds the key: not what was printed, not the
        // state directory, not the run record, not what the tool applied.
        nothing_leaked(&out, &dir, &["sink.json"]);
    }
    let work = dir.join(".meridian/resourceset/site/fake");
    let delivery = fs::read_to_string(work.join("credential-delivery.json")).unwrap();
    assert_eq!(
        delivery,
        "{\"ambient\":false,\"channel\":\"file\",\"flags\":{},\"name\":\"deploy\",\
         \"secretFields\":[\"privateKey\"],\"text\":{\"user\":\"deploy\"},\"type\":\"ssh\"}"
    );
}

#[test]
fn a_password_credential_is_handed_over_the_same_way() {
    let dir = scratch("password");
    let document = BY_KEY
        .replace("type: ssh", "type: usernamePassword")
        .replace("user: deploy", "user: admin")
        .replace(
            "privateKey: \"${{ secret.deploy_key }}\"",
            "password: \"${{ secret.deploy_password }}\"",
        );
    fs::write(dir.join("site.yaml"), document).unwrap();
    let sink = sink(&dir);
    let out = meridian_with(
        &dir,
        &["apply", "site.yaml"],
        &[("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(
        fs::read_to_string(&sink).unwrap(),
        format!("{{\"password\":\"{PASSWORD}\"}}\n")
    );
    assert!(
        said(&out).contains("as deploy (usernamePassword, user admin)"),
        "{}",
        said(&out)
    );
    nothing_leaked(&out, &dir, &["sink.json"]);
}

#[test]
fn an_ambient_credential_reaches_the_tool_by_name_and_hands_nothing_over() {
    let dir = scratch("ambient");
    let document = BY_KEY.replace(
        "      privateKey: \"${{ secret.deploy_key }}\"",
        "      from: ambient",
    );
    fs::write(dir.join("site.yaml"), document).unwrap();
    let sink = sink(&dir);
    let out = meridian_with(
        &dir,
        &["plan", "site.yaml"],
        &[
            ("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap()),
            ("DEPLOY_KEY", ""),
        ],
    );
    assert!(out.status.success(), "{}", said(&out));
    assert!(
        said(&out).contains("as deploy (ssh, user deploy, ambient)"),
        "{}",
        said(&out)
    );
    assert_eq!(fs::read_to_string(&sink).unwrap(), "{}\n");
}

#[test]
fn a_target_that_cannot_reach_a_host_as_a_credential_is_refused_before_anything_runs() {
    let dir = by_key("no-channel");
    let sink = sink(&dir);
    for command in ["compile", "plan", "apply"] {
        let out = meridian_with(
            &dir,
            &[command, "site.yaml"],
            &[
                ("FAKE_SECRETS", "none"),
                ("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap()),
            ],
        );
        assert!(!out.status.success(), "{command}: {}", said(&out));
        let text = said(&out);
        assert!(
            text.contains(
                "spec.targetHost names the credential \"deploy\", and target \"fake\" does not \
                 take one: it has no way to reach its host as that identity"
            ) && text.contains("Leave the credential out to use the tool's own"),
            "{command}: {text}"
        );
        assert!(!sink.exists(), "{command}");
        nothing_leaked(&out, &dir, &[]);
    }
    // Nothing was applied.
    assert!(!dir.join(".meridian/resourceset/site/fake/current").exists());
}

#[test]
fn a_credential_the_host_does_not_name_is_not_fetched_or_handed_over() {
    let dir = scratch("unused");
    let document = BY_KEY.replace("    credential: deploy\n", "");
    fs::write(dir.join("site.yaml"), document).unwrap();
    let sink = sink(&dir);
    let out = meridian_with(
        &dir,
        &["apply", "site.yaml"],
        &[("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", said(&out));
    assert!(!sink.exists());
    assert!(!said(&out).contains(" as deploy"), "{}", said(&out));
    nothing_leaked(&out, &dir, &[]);
}

// ---- a cloud endpoint ---------------------------------------------------

/// An Infrastructure document provisioning in a cloud reached as `prod`,
/// an AWS credential whose secret key is a secret read from the
/// environment.
const IN_A_CLOUD: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: net
spec:
  target: fake
  endpoint:
    type: cloud
    credential: prod
  secrets:
    - name: aws_secret_key
      from: env
      key: DEPLOY_PASSWORD
  credentials:
    - name: prod
      type: aws
      accessKeyId: AKIA1
      secretAccessKey: "${{ secret.aws_secret_key }}"
      region: eu-west-1
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
"#;

fn in_a_cloud(test: &str) -> PathBuf {
    let dir = scratch(test);
    fs::write(dir.join("net.yaml"), IN_A_CLOUD).unwrap();
    dir
}

#[test]
fn an_infrastructure_document_names_the_credential_its_cloud_is_reached_as() {
    let dir = in_a_cloud("cloud-validate");
    let out = meridian_with(&dir, &["validate", "net.yaml"], &[("DEPLOY_PASSWORD", "")]);
    assert!(out.status.success(), "{}", said(&out));

    // A cloud takes a cloud credential only, and the endpoint's type is one
    // the language has.
    let ssh = IN_A_CLOUD
        .replace("type: aws", "type: ssh")
        .replace("      accessKeyId: AKIA1\n", "      user: deploy\n")
        .replace("secretAccessKey:", "privateKey:")
        .replace("      region: eu-west-1\n", "");
    fs::write(dir.join("net.yaml"), &ssh).unwrap();
    let out = meridian(&dir, &["validate", "net.yaml"]);
    assert!(!out.status.success());
    assert!(
        said(&out).contains("a cloud takes an aws, azure or gcp credential; \"prod\" is ssh"),
        "{}",
        said(&out)
    );
    let daemon = IN_A_CLOUD.replace("type: cloud", "type: daemon");
    fs::write(dir.join("net.yaml"), &daemon).unwrap();
    let out = meridian(&dir, &["validate", "net.yaml"]);
    assert!(!out.status.success());
    assert!(
        said(&out).contains("unknown endpoint type \"daemon\" (expected cloud or cluster)"),
        "{}",
        said(&out)
    );
}

#[test]
fn plan_and_apply_hand_a_cloud_credential_to_the_tool_and_the_plan_line_says_how() {
    let dir = in_a_cloud("cloud-handed");
    let sink = sink(&dir);
    for command in ["plan", "apply"] {
        let _ = fs::remove_file(&sink);
        let out = meridian_with(
            &dir,
            &[command, "net.yaml"],
            &[("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap())],
        );
        assert!(out.status.success(), "{command}: {}", said(&out));
        let handed = fs::read_to_string(&sink).unwrap();
        assert_eq!(
            handed,
            format!("{{\"secretAccessKey\":\"{PASSWORD}\"}}\n"),
            "{command}"
        );
        // The plan says which identity reaches the cloud, and that it goes
        // the one way the providers take it: the tool's environment.
        let text = said(&out);
        assert!(
            text.contains(
                "net (Infrastructure, target fake as prod (aws, region eu-west-1), in the \
                 tool's environment)"
            ),
            "{command}: {text}"
        );
        nothing_leaked(&out, &dir, &["sink.json"]);
    }
    let work = dir.join(".meridian/infrastructure/net/fake");
    let delivery = fs::read_to_string(work.join("credential-delivery.json")).unwrap();
    assert_eq!(
        delivery,
        "{\"ambient\":false,\"channel\":\"file\",\"flags\":{},\"name\":\"prod\",\
         \"secretFields\":[\"secretAccessKey\"],\"text\":{\"accessKeyId\":\"AKIA1\",\
         \"region\":\"eu-west-1\"},\"type\":\"aws\"}"
    );
    // The compiled artifact names the credential and its plain fields, as
    // the test target does, and never the value.
    let compiled = fs::read_to_string(work.join("current/credential.txt")).unwrap();
    assert_eq!(
        compiled,
        "prod (aws, region eu-west-1)\nsecret fields: secretAccessKey\n"
    );
}

#[test]
fn a_target_that_cannot_hand_its_tool_a_cloud_credential_is_refused_before_anything_runs() {
    let dir = in_a_cloud("cloud-no-channel");
    let sink = sink(&dir);
    for command in ["compile", "plan", "apply"] {
        let out = meridian_with(
            &dir,
            &[command, "net.yaml"],
            &[
                ("FAKE_SECRETS", "none"),
                ("FAKE_CREDENTIAL_SINK", sink.to_str().unwrap()),
            ],
        );
        assert!(!out.status.success(), "{command}: {}", said(&out));
        let text = said(&out);
        assert!(
            text.contains(
                "spec.endpoint names the credential \"prod\" for a cloud, and target \"fake\" \
                 does not take one: it has no way to hand its tool that identity"
            ) && text.contains("Leave the endpoint out to use the tool's own"),
            "{command}: {text}"
        );
        assert!(!sink.exists(), "{command}");
        nothing_leaked(&out, &dir, &[]);
    }
    assert!(!dir
        .join(".meridian/infrastructure/net/fake/current")
        .exists());
}
