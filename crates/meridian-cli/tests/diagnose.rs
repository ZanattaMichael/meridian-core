//! `meridian diagnose`: the failed step first, its resource and cause, the
//! steps not run because of it, and the commands to run next; read from the
//! record only, without a lock or a plugin.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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
        content: 10.0.0.5
    - id: shared
      type: file
      params:
        content: kq7Zp2-not-a-real-password-9fXw
  outputs:
    - name: address
      value: "${vm.content}"
    - name: shared
      value: "${shared.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: infrastructure
    ref: network
    output: address
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  resources:
    - id: banner
      type: file
      params:
        content: "FAIL"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: app
spec:
  target: fake
  resources:
    - id: page
      type: file
      params:
        content: hello
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("diagnose-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian_with(dir: &Path, plugins: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("SITE_DB_PASSWORD", "not-a-real-password")
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", plugins)
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_OUTPUT")
        .env_remove("MERIDIAN_NO_LOGS")
        .output()
        .expect("meridian runs")
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    meridian_with(dir, &meridian_target_fake::plugin_dir(), args)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The id of the only run.
fn run_id(dir: &Path) -> String {
    let runs: Vec<String> = fs::read_dir(dir.join(".meridian/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs[0].clone()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The one object on stdout, checked against `schemas/output/diagnose`.
fn checked(output: &Output) -> Value {
    let text = stdout(output);
    let value: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}):\n{text}"));
    let path = repo_root().join("schemas/output/diagnose.schema.json");
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
fn the_failed_step_comes_first_with_its_resource_cause_and_next_commands() {
    let dir = scratch("text");
    let applied = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(applied.status.code(), Some(1));
    let id = run_id(&dir);

    let out = meridian(&dir, &["diagnose"]);
    assert_eq!(out.status.code(), Some(0));
    let expected = format!(
        "run {id}: apply, failed (1 of 3 steps failed; 1 not run because of it)\n\
         \n\
         web  fake  10.0.0.5  resource-failed\n\
         \x20 banner  could not write files/banner: disk on fire  site.yaml:37\n\
         \x20 next: meridian logs {id} --step web --endpoint 10.0.0.5\n\
         \x20 next: meridian apply site.yaml --tool-debug --tool-debug-step web --resume={id}\n\
         \n\
         not run: app (earlier-step-failed: web)\n"
    );
    assert_eq!(stdout(&out), expected);

    // The commands it names are ones meridian takes.
    let logs = meridian(
        &dir,
        &["logs", &id, "--step", "web", "--endpoint", "10.0.0.5"],
    );
    assert_eq!(
        logs.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&logs.stderr)
    );

    // Narrowed to what did not fail, it says so.
    let none = stdout(&meridian(&dir, &["diagnose", "--cause", "unreachable"]));
    assert!(none.contains("nothing failed that matches"), "{none}");
}

#[test]
fn the_json_names_the_run_the_entries_and_the_steps_not_run() {
    let dir = scratch("json");
    meridian(&dir, &["apply", "site.yaml"]);
    let id = run_id(&dir);
    let out = meridian(&dir, &["diagnose", "--output", "json"]);
    let value = checked(&out);
    assert_eq!(value["command"], "diagnose");
    assert_eq!(value["ok"], true);
    assert_eq!(value["run"]["id"], id.as_str());
    assert_eq!(value["run"]["status"], "failed");
    let step = &value["failed"][0];
    assert_eq!(step["name"], "web");
    assert_eq!(step["reason"], "tool-failed");
    let entry = &step["entries"][0];
    assert_eq!(entry["cause"], "resource-failed");
    assert_eq!(entry["endpoints"], serde_json::json!(["10.0.0.5"]));
    assert_eq!(entry["failures"][0]["resource"], "banner");
    assert_eq!(entry["failures"][0]["toolCode"], "EFIRE");
    assert_eq!(entry["next"].as_array().unwrap().len(), 2);
    assert_eq!(value["skipped"][0]["name"], "app");
    assert_eq!(value["skipped"][0]["becauseOf"], "web");
    assert_eq!(value["keptHere"], true);
}

#[test]
fn it_reads_a_locked_state_without_a_plugin_and_takes_no_lock() {
    let dir = scratch("lock");
    meridian(&dir, &["apply", "site.yaml"]);
    let lock = dir.join(".meridian/lock");
    let held = r#"{"command":"apply site.yaml","host":"build-07","id":"2026-11-17T09-14-03Z-7f3a","pid":4242,"since":"2026-11-17T09:14:03.000Z","user":"mike"}"#;
    fs::write(&lock, held).unwrap();
    let empty = dir.join("no-plugins");
    fs::create_dir_all(&empty).unwrap();
    let out = meridian_with(&dir, &empty, &["diagnose"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout(&out).contains("web  fake  10.0.0.5  resource-failed"));
    assert_eq!(
        fs::read_to_string(&lock).unwrap(),
        held,
        "the lock is untouched"
    );
}

#[test]
fn with_no_failed_run_there_is_nothing_to_diagnose() {
    let dir = scratch("none");
    let out = meridian(&dir, &["diagnose"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).starts_with("no failed run is recorded"),
        "{}",
        stdout(&out)
    );
    let json = checked(&meridian(&dir, &["diagnose", "--output", "json"]));
    assert_eq!(json["run"], Value::Null);
}
