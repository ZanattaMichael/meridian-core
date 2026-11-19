//! `meridian destroy` and `meridian plan --destroy`, run as a process
//! against the fake target: what each stage's tool made is removed, last
//! stage first; a target that cannot destroy is left in place; nothing is
//! destroyed without an answer.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value as Json;

const WORKFLOW: &str = r#"apiVersion: meridian/v1
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
  outputs:
    - name: address
      value: "${vm.content}"
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
  resources:
    - id: motd
      type: file
      params:
        content: "MOTD"
"#;

fn scratch(test: &str, motd: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("destroy")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), WORKFLOW.replace("MOTD", motd)).unwrap();
    dir
}

fn meridian_with(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env(
            "MERIDIAN_PLUGIN_DIR",
            meridian_target_fake::plugin_dir().display().to_string(),
        )
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .stdin(Stdio::null())
        .args(args)
        .current_dir(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    meridian_with(dir, &[], args)
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\n{}",
        output.status.code(),
        text(output)
    );
    text(output)
}

fn json(output: &Output) -> Json {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(output)))
}

/// Each stage of a JSON result as (name, status).
fn stages(result: &Json) -> Vec<(String, String)> {
    result["stages"]
        .as_array()
        .expect("stages")
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_owned(),
                s["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn work(dir: &Path, kind: &str, name: &str) -> PathBuf {
    dir.join(".meridian").join(kind).join(name).join("fake")
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn destroy_removes_what_was_applied_last_stage_first() {
    let dir = scratch("removes", "welcome");
    ok(&meridian(&dir, &["apply", "site.yaml"]));
    let motd = work(&dir, "resourceset", "web").join("current/files/motd");
    assert!(motd.is_file());

    let destroyed = meridian(&dir, &["destroy", "--yes", "site.yaml", "--output", "json"]);
    let result = json(&destroyed);
    assert!(destroyed.status.success(), "{}", text(&destroyed));
    assert_eq!(
        stages(&result),
        pairs(&[("web", "destroyed"), ("network", "destroyed")])
    );
    assert_eq!(result["stages"][0]["changes"][0]["action"], "delete");
    assert!(!motd.exists());
    assert!(!work(&dir, "infrastructure", "network")
        .join("current/files/vm")
        .exists());

    // Forgotten: drift has nothing to compare, and a second destroy keeps
    // both stages as never applied.
    let drift = ok(&meridian(&dir, &["drift", "site.yaml"]));
    assert!(drift.contains("never applied"), "{drift}");
    let again = json(&meridian(
        &dir,
        &["destroy", "--yes", "site.yaml", "--output", "json"],
    ));
    assert_eq!(
        stages(&again),
        pairs(&[("web", "kept"), ("network", "kept")])
    );
    assert_eq!(again["stages"][0]["kept"], "never-applied");
}

#[test]
fn plan_destroy_previews_and_changes_nothing() {
    let dir = scratch("preview", "welcome");
    ok(&meridian(&dir, &["apply", "site.yaml"]));

    let said = ok(&meridian(&dir, &["plan", "--destroy", "site.yaml"]));
    let web = said.find("web").expect("web is shown");
    let network = said.find("network").expect("network is shown");
    assert!(web < network, "last stage first:\n{said}");
    assert!(said.contains("- delete files/motd"), "{said}");
    assert!(work(&dir, "resourceset", "web")
        .join("current/files/motd")
        .is_file());

    // A plain drift still finds both stages applied and in sync.
    let drift = ok(&meridian(&dir, &["drift", "site.yaml"]));
    assert!(drift.contains("drift: 0 of 2 stages drifted"), "{drift}");
}

#[test]
fn nothing_is_destroyed_without_an_answer() {
    let dir = scratch("unanswered", "welcome");
    ok(&meridian(&dir, &["apply", "site.yaml"]));

    let refused = meridian(&dir, &["destroy", "site.yaml"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains("`meridian destroy --yes site.yaml` destroys it"),
        "{}",
        text(&refused)
    );
    assert!(work(&dir, "resourceset", "web")
        .join("current/files/motd")
        .is_file());
}

#[test]
fn a_target_that_cannot_destroy_is_left_in_place() {
    let dir = scratch("cannot", "welcome");
    ok(&meridian(&dir, &["apply", "site.yaml"]));

    let kept = meridian_with(
        &dir,
        &[("FAKE_DESTROY", "off")],
        &["destroy", "--yes", "site.yaml"],
    );
    let said = ok(&kept);
    assert!(
        said.contains("kept: target \"fake\" cannot destroy"),
        "{said}"
    );
    assert!(work(&dir, "resourceset", "web")
        .join("current/files/motd")
        .is_file());
}

#[test]
fn a_failed_destroy_stops_and_keeps_the_stage() {
    let dir = scratch("fails", "LOCKED welcome");
    ok(&meridian(&dir, &["apply", "site.yaml"]));

    let failed = meridian(&dir, &["destroy", "--yes", "site.yaml", "--output", "json"]);
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed));
    let result = json(&failed);
    assert_eq!(
        stages(&result),
        pairs(&[("web", "failed"), ("network", "not-run")])
    );
    assert_eq!(result["stages"][0]["reason"], "tool-failed");
    // Still recorded as applied, so a second destroy tries again.
    let drift = ok(&meridian(&dir, &["drift", "site.yaml"]));
    assert!(!drift.contains("never applied"), "{drift}");
    assert!(work(&dir, "infrastructure", "network")
        .join("current/files/vm")
        .is_file());
}

#[test]
fn destroy_takes_no_policy_resume_or_plan_file() {
    let dir = scratch("refusals", "welcome");
    fs::write(dir.join("policy.rego"), "package meridian\n").unwrap();
    let policy = meridian(
        &dir,
        &["plan", "--destroy", "site.yaml", "--policy", "policy.rego"],
    );
    assert_eq!(policy.status.code(), Some(1));
    assert!(
        text(&policy).contains("plan --destroy asks no policy"),
        "{}",
        text(&policy)
    );
    let out = meridian(&dir, &["plan", "--destroy", "site.yaml", "--out", "p.json"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}
