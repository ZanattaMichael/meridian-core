//! `meridian test` lifecycle cases, run as a process against the fake
//! target: provision, verify and destroy in order; destroy after a failed
//! provision, a failed verify and a timeout; `--keep`; a failed destroy
//! keeps the state; a target that cannot destroy is refused.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value as Json;

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
    - id: tag
      type: file
      params:
        content: "${{ meridian.run.id }}"
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
        content: "${{ data.motd }}${{ data.tail }}"
"#;

fn test_file(cases: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: Test\nmetadata:\n  name: site\nspec:\n  documents: [../site.yaml]\n  set: {{motd: welcome, tail: ''}}\n  cases:\n{cases}"
    )
}

const PASSING: &str = r#"    - name: web tier
      run: lifecycle
      phases:
        provision: {}
        verify: {drift: none}
        destroy: {}
      expect:
        web: {outcome: applied}
      assert:
        - stages.network.outputs.address == '10.0.0.5'
"#;

fn scratch(test: &str, cases: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("lifecycle")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    fs::write(dir.join("tests/site.test.yaml"), test_file(cases)).unwrap();
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

/// The one case of a JSON result.
fn case(output: &Output) -> Json {
    let result: Json =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(output)));
    result["files"][0]["cases"][0].clone()
}

/// Each phase of a case as (name, status).
fn phases(case: &Json) -> Vec<(String, String)> {
    case["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_owned(),
                p["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

fn case_dir(dir: &Path) -> PathBuf {
    dir.join(".meridian/tests/site/1-web-tier")
}

fn failures(case: &Json) -> String {
    case["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn phases_run_in_order_and_destroy_removes_everything() {
    let dir = scratch("passing", PASSING);
    let junit = dir.join("junit.xml");
    let out = meridian(&dir, &["test", "--junit", junit.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "tests/site.test.yaml (site)\n\
         \x20 ok    web tier (lifecycle, 2 documents)\n\
         \x20       provision: ok\n\
         \x20       verify: ok\n\
         \x20       destroy: ok\n\
         1 case: 1 passed\n"
    );
    // Destroyed, so nothing of the environment is left.
    assert!(!case_dir(&dir).exists());
    let xml = fs::read_to_string(&junit).unwrap();
    for phase in ["provision", "verify", "destroy"] {
        assert!(
            xml.contains(&format!("name=\"web tier: {phase}\"")),
            "{xml}"
        );
    }
    assert!(
        xml.contains("tests=\"3\" failures=\"0\" errors=\"0\" skipped=\"0\""),
        "{xml}"
    );

    let out = meridian(&dir, &["test", "--output", "json"]);
    let schema = meridian_plugin_testkit::repo_root().join("schemas/output/test.schema.json");
    let schema: Json = serde_json::from_str(&fs::read_to_string(schema).unwrap()).unwrap();
    let result: Json = serde_json::from_slice(&out.stdout).unwrap();
    let errors: Vec<String> = jsonschema::draft7::new(&schema)
        .unwrap()
        .iter_errors(&result)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let case = case(&out);
    assert_eq!(case["run"], "lifecycle");
    let id = case["runId"].as_str().unwrap();
    assert!(id.len() == 9 && id.starts_with('t'), "{id}");
    assert_eq!(
        phases(&case),
        pairs(&[
            ("provision", "passed"),
            ("verify", "passed"),
            ("destroy", "passed")
        ])
    );
}

#[test]
fn a_failed_provision_skips_verify_and_still_destroys() {
    let dir = scratch(
        "provision",
        "    - name: web tier\n      run: lifecycle\n      set: {motd: FA, tail: IL}\n",
    );
    let out = meridian(&dir, &["test", "--output", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let case = case(&out);
    assert_eq!(case["status"], "failed");
    assert_eq!(
        phases(&case),
        pairs(&[
            ("provision", "failed"),
            ("verify", "skipped"),
            ("destroy", "passed")
        ])
    );
    assert!(
        failures(&case).starts_with("provision: document \"web\" failed:"),
        "{}",
        failures(&case)
    );
    assert!(!case_dir(&dir).exists());
}

#[test]
fn a_failed_verify_still_destroys() {
    let dir = scratch(
        "verify",
        "    - name: web tier\n      run: lifecycle\n      assert: [\"stages.network.outputs.address == '10.0.0.6'\"]\n",
    );
    let out = meridian(&dir, &["test", "--output", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let case = case(&out);
    assert_eq!(
        phases(&case),
        pairs(&[
            ("provision", "passed"),
            ("verify", "failed"),
            ("destroy", "passed")
        ])
    );
    assert_eq!(
        failures(&case),
        "verify: assert stages.network.outputs.address == '10.0.0.6': false"
    );
    assert!(!case_dir(&dir).exists());
}

#[test]
fn a_timeout_stops_provision_and_destroy_runs() {
    let dir = scratch(
        "timeout",
        "    - name: web tier\n      run: lifecycle\n      timeout: 1s\n      set: {motd: HA, tail: NG}\n",
    );
    let out = meridian(&dir, &["test", "--output", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let case = case(&out);
    assert_eq!(
        phases(&case),
        pairs(&[
            ("provision", "failed"),
            ("verify", "skipped"),
            ("destroy", "passed")
        ])
    );
    assert!(
        failures(&case).contains("provision: timed out after 1s"),
        "{}",
        failures(&case)
    );
    assert!(!case_dir(&dir).exists());
}

#[test]
fn keep_leaves_the_environment_and_the_next_run_will_not_throw_it_away() {
    let dir = scratch("keep", PASSING);
    let out = meridian(&dir, &["test", "--keep"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.contains("destroy: skipped: kept by --keep: the environment's state is in"),
        "{said}"
    );
    let kept = case_dir(&dir);
    assert!(kept
        .join("infrastructure/network/fake/current/files/vm")
        .is_file());
    // The documents named what they made with the case's run id, and the
    // directory says which it was.
    let id = fs::read_to_string(kept.join("run-id")).unwrap();
    let id = id.trim();
    assert!(id.len() == 9 && id.starts_with('t'), "{id}");
    assert_eq!(
        fs::read_to_string(kept.join("infrastructure/network/fake/current/files/tag")).unwrap(),
        id
    );
    assert!(said.contains(&format!("its run id is {id}")), "{said}");

    let again = meridian(&dir, &["test", "--output", "json"]);
    assert_eq!(again.status.code(), Some(1), "{}", text(&again));
    let case = case(&again);
    assert_eq!(case["status"], "error");
    assert!(
        failures(&case).contains(&format!("--run-id {id}`")),
        "{}",
        failures(&case)
    );
    assert!(kept
        .join("infrastructure/network/fake/current/files/vm")
        .is_file());
}

#[test]
fn a_failed_destroy_fails_the_case_and_keeps_the_state() {
    let dir = scratch(
        "destroy",
        "    - name: web tier\n      run: lifecycle\n      set: {motd: LOCK, tail: ED}\n",
    );
    let out = meridian(&dir, &["test", "--output", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let case = case(&out);
    assert_eq!(
        phases(&case),
        pairs(&[
            ("provision", "passed"),
            ("verify", "passed"),
            ("destroy", "failed")
        ])
    );
    assert!(
        failures(&case).contains("which is kept"),
        "{}",
        failures(&case)
    );
    assert!(case_dir(&dir).is_dir());
}

#[test]
fn a_target_that_cannot_destroy_is_refused() {
    let dir = scratch("cannot", PASSING);
    let out = meridian_with(
        &dir,
        &[("FAKE_DESTROY", "off")],
        &["test", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let case = case(&out);
    assert_eq!(case["status"], "error");
    assert_eq!(
        failures(&case),
        "a lifecycle case destroys what it provisions, and the target \"fake\" cannot destroy"
    );
    assert!(!case_dir(&dir).exists());
}
