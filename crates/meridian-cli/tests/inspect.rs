//! `meridian inspect`: one resource as its tool reads it now, beside what
//! the document declares; read-only, without a lock, with secrets masked.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const SITE: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: inline
    host: web1
  secrets:
    - name: token
      from: env
      key: INSPECT_TEST_TOKEN
  resources:
    - id: page
      type: file
      params:
        content: hello
    - id: secret_page
      type: file
      params:
        content: "token=${{ secret.token }}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: other
spec:
  target: fake
  targetHost:
    from: inline
    host: web2
  resources:
    - id: twice
      type: file
      params:
        content: a
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: third
spec:
  target: fake
  targetHost:
    from: inline
    host: web3
  resources:
    - id: twice
      type: file
      params:
        content: b
"#;

const TOKEN: &str = "zq4Lm8-not-a-real-token-77";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("inspect-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian_env(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("INSPECT_TEST_TOKEN", TOKEN)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_OUTPUT")
        .args(args)
        .current_dir(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    meridian_env(dir, &[], args)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn applied(test: &str) -> PathBuf {
    let dir = scratch(test);
    let out = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    dir
}

/// The one object on stdout, checked against `schemas/output/inspect`.
fn checked(output: &Output) -> Value {
    let text = stdout(output);
    let value: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}):\n{text}"));
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/output/inspect.schema.json");
    let schema: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}\n{text}");
    value
}

#[test]
fn it_shows_desired_and_actual_and_names_what_differs() {
    let dir = applied("text");
    let same = meridian(&dir, &["inspect", "page", "site.yaml"]);
    assert_eq!(same.status.code(), Some(0), "{}", stderr(&same));
    assert_eq!(
        stdout(&same),
        "page (file) in web, target fake on web1\n\
         \x20 read with: fake-tool show files/page --on web1\n\
         \x20 found; the tool says it is as declared\n\
         \n\
         \x20 property  desired                   actual\n\
         \x20 content   \"hello\"                   \"hello\"\n"
    );

    fs::write(
        dir.join("site.yaml"),
        SITE.replace("content: hello", "content: bye"),
    )
    .unwrap();
    let differs = stdout(&meridian(&dir, &["inspect", "page", "site.yaml"]));
    assert!(
        differs.contains("the tool says it differs from the document"),
        "{differs}"
    );
    assert!(
        differs.contains("\"bye\"                     \"hello\"  (differs)"),
        "{differs}"
    );
    assert!(differs.ends_with("  differs: content\n"), "{differs}");
}

#[test]
fn the_json_carries_desired_actual_and_the_differences() {
    let dir = applied("json");
    fs::write(
        dir.join("site.yaml"),
        SITE.replace("content: hello", "content: bye"),
    )
    .unwrap();
    let out = meridian(&dir, &["inspect", "page", "site.yaml", "--output", "json"]);
    let value = checked(&out);
    assert_eq!(value["command"], "inspect");
    assert_eq!(value["ok"], true);
    assert_eq!(value["step"]["name"], "web");
    assert_eq!(value["target"], "fake");
    let read = &value["inspection"];
    assert_eq!(read["endpoint"], "web1");
    assert_eq!(read["found"], true);
    assert_eq!(read["inDesiredState"], false);
    assert_eq!(read["command"], "fake-tool show files/page --on web1");
    assert_eq!(read["desired"]["content"], "bye");
    assert_eq!(read["actual"]["content"], "hello");
    assert_eq!(read["differs"], serde_json::json!(["content"]));
}

#[test]
fn a_resource_never_applied_is_not_found() {
    let dir = scratch("absent");
    let out = meridian(&dir, &["inspect", "page", "site.yaml", "--output", "json"]);
    let value = checked(&out);
    assert_eq!(value["inspection"]["found"], false);
    assert_eq!(value["inspection"]["inDesiredState"], false);
    assert_eq!(value["inspection"]["actual"], serde_json::json!({}));
    assert_eq!(value["inspection"]["notCompared"][0]["why"], "not-reported");
}

#[test]
fn it_changes_nothing_and_takes_no_lock() {
    let dir = applied("readonly");
    let state = dir.join(".meridian/resourceset/web/fake/state.json");
    let before = fs::read(&state).unwrap();
    let runs = || fs::read_dir(dir.join(".meridian/runs")).unwrap().count();
    let runs_before = runs();
    // Another run holds the lock; reading one resource does not need it.
    let lock = dir.join(".meridian/lock");
    let held = r#"{"command":"apply site.yaml","host":"build-07","id":"2026-11-17T09-14-03Z-7f3a","pid":4242,"since":"2026-11-17T09:14:03.000Z","user":"ci"}"#;
    fs::write(&lock, held).unwrap();
    fs::write(
        dir.join("site.yaml"),
        SITE.replace("content: hello", "content: bye"),
    )
    .unwrap();

    let out = meridian(&dir, &["inspect", "page", "site.yaml"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        fs::read(&state).unwrap(),
        before,
        "the tool's state changed"
    );
    assert_eq!(runs(), runs_before, "a run record was written");
    assert_eq!(fs::read_to_string(&lock).unwrap(), held);
}

#[test]
fn a_secret_is_masked_and_not_compared() {
    let dir = applied("secret");
    for args in [
        &["inspect", "secret_page", "site.yaml"][..],
        &["inspect", "secret_page", "site.yaml", "--output", "json"][..],
    ] {
        let out = meridian(&dir, args);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert!(!stdout(&out).contains(TOKEN), "{}", stdout(&out));
        assert!(!stderr(&out).contains(TOKEN), "{}", stderr(&out));
    }
    let value = checked(&meridian(
        &dir,
        &["inspect", "secret_page", "site.yaml", "--output", "json"],
    ));
    let read = &value["inspection"];
    assert_eq!(read["desired"]["content"], "(sensitive)");
    assert_eq!(read["actual"]["content"], "token=(sensitive)");
    assert_eq!(read["notCompared"][0]["param"], "content");
    assert_eq!(read["notCompared"][0]["why"], "sensitive");
}

#[test]
fn a_target_that_cannot_inspect_is_refused_with_the_rule() {
    let dir = applied("unsupported");
    let out = meridian_env(
        &dir,
        &[("FAKE_INSPECT", "off")],
        &["inspect", "page", "site.yaml"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("target \"fake\" cannot read one resource (unsupported-inspect)"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn the_step_and_the_endpoint_are_checked() {
    let dir = applied("checks");
    let unknown = meridian(&dir, &["inspect", "nothing", "site.yaml"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stderr(&unknown).contains("no document declares a resource \"nothing\""));

    let twice = meridian(&dir, &["inspect", "twice", "site.yaml"]);
    assert_eq!(twice.status.code(), Some(1));
    assert!(
        stderr(&twice).contains("other, third declare a resource \"twice\"; name one with --step"),
        "{}",
        stderr(&twice)
    );
    let named = meridian(&dir, &["inspect", "twice", "site.yaml", "--step", "third"]);
    assert_eq!(named.status.code(), Some(0), "{}", stderr(&named));
    assert!(stdout(&named).starts_with("twice (file) in third, target fake on web3\n"));

    let elsewhere = meridian(
        &dir,
        &["inspect", "page", "site.yaml", "--endpoint", "web9"],
    );
    assert_eq!(elsewhere.status.code(), Some(1));
    assert!(stderr(&elsewhere).contains("web configures web1, not web9"));
}

#[test]
fn a_tool_that_fails_to_read_fails_the_command() {
    let dir = scratch("fails");
    fs::write(
        dir.join("site.yaml"),
        SITE.replace("content: hello", "content: FAIL"),
    )
    .unwrap();
    let out = meridian(&dir, &["inspect", "page", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("fake-tool cannot read files/page"),
        "{}",
        stderr(&out)
    );
}
