//! Meridian-run notifies (decision 0055): a notify the fake target's tool
//! cannot express (`touch`), run by its plugin after the stage applies.

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
  resources:
    - id: config
      type: file
      params:
        content: v1
      notifies:
        - resource: page
          action: touch
    - id: page
      type: file
      params:
        content: hello
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("notify-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    meridian_naming(dir, "ids", args)
}

/// `changes` is how the fake names a change: `ids` traces it to the
/// resource, so it triggers a notify; `files` does not.
fn meridian_naming(dir: &Path, changes: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_OUTPUT")
        .env_remove("FAKE_ACTS")
        .env("FAKE_CHANGES", changes)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn both(output: &Output) -> String {
    format!(
        "{}{}",
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn acts(dir: &Path) -> String {
    fs::read_to_string(dir.join(".meridian/resourceset/web/fake/acts.log")).unwrap_or_default()
}

fn edit(dir: &Path, from: &str, to: &str) {
    let path = dir.join("site.yaml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace(from, to)).unwrap();
}

fn state(dir: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join(".meridian/state.json")).unwrap()).unwrap()
}

#[test]
fn a_compile_says_meridian_runs_the_notify() {
    let dir = scratch("compile");
    let out = meridian(&dir, &["compile", "site.yaml"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert!(
        both(&out)
            .contains("Meridian runs touch on page after the stage applies, when config changes"),
        "{}",
        both(&out)
    );
}

#[test]
fn a_plan_lists_the_action_and_an_apply_runs_it_once() {
    let dir = scratch("apply");
    let plan = meridian(&dir, &["plan", "site.yaml"]);
    assert!(
        stdout(&plan).contains("  would touch page (config changes)\n"),
        "{}",
        both(&plan)
    );

    let out = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert!(
        stdout(&out).contains("  ran touch page: fake-tool touch files/page\n"),
        "{}",
        both(&out)
    );
    assert_eq!(acts(&dir), "touch page\n");

    // Nothing changed: nothing is triggered.
    let again = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(again.status.code(), Some(0), "{}", both(&again));
    assert_eq!(acts(&dir), "touch page\n");

    // A change to the notified resource alone triggers nothing.
    edit(&dir, "content: hello", "content: bye");
    meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(acts(&dir), "touch page\n");

    // A change to the notifier does.
    edit(&dir, "content: v1", "content: v2");
    meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(acts(&dir), "touch page\ntouch page\n");
}

#[test]
fn the_json_lists_each_action_with_its_status() {
    let dir = scratch("json");
    let out = meridian(&dir, &["apply", "site.yaml", "--output", "json"]);
    let value: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/output/run.schema.json");
    let schema: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(
        value["stages"][0]["actions"],
        serde_json::json!([{
            "notifier": "config",
            "resource": "page",
            "action": "touch",
            "status": "done",
            "command": "fake-tool touch files/page",
        }])
    );
}

#[test]
fn a_failed_action_fails_the_stage_and_the_next_apply_runs_it() {
    let dir = scratch("failed");
    edit(&dir, "content: hello", "content: NOACT");
    let out = meridian(&dir, &["apply", "site.yaml"]);
    assert_ne!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        text.contains("FAILED: the stage applied, and touch on page failed"),
        "{text}"
    );
    assert!(
        text.contains("  touch page failed; it is pending\n"),
        "{text}"
    );
    assert!(text.contains("resource-failed"), "{text}");
    let pending = &state(&dir)["resourceset/web"]["pending"];
    assert_eq!(pending[0]["resource"], "page");
    assert_eq!(acts(&dir), "");

    // The notifier is unchanged; the pending action still runs.
    edit(&dir, "content: NOACT", "content: hello");
    let plan = meridian(&dir, &["plan", "site.yaml"]);
    assert!(
        stdout(&plan).contains("would touch page (config changes)"),
        "{}",
        both(&plan)
    );
    let again = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(again.status.code(), Some(0), "{}", both(&again));
    assert_eq!(acts(&dir), "touch page\n");
    assert!(state(&dir)["resourceset/web"].get("pending").is_none());
}

#[test]
fn an_apply_from_a_saved_plan_runs_the_planned_action() {
    let dir = scratch("saved");
    let plan = meridian(&dir, &["plan", "site.yaml", "--out", "plan.json"]);
    assert_eq!(plan.status.code(), Some(0), "{}", both(&plan));
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("plan.json")).unwrap()).unwrap();
    assert_eq!(saved["stages"][0]["actions"][0]["status"], "would");
    let out = meridian(&dir, &["apply", "--plan", "plan.json"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(acts(&dir), "touch page\n");
}

#[test]
fn an_action_the_saved_plan_did_not_list_is_not_run() {
    let dir = scratch("unplanned");
    // The plan's changes do not trace to the notifier, so it lists no
    // action; the apply's do.
    let plan = meridian_naming(&dir, "files", &["plan", "site.yaml", "--out", "plan.json"]);
    assert_eq!(plan.status.code(), Some(0), "{}", both(&plan));
    let out = meridian(&dir, &["apply", "--plan", "plan.json"]);
    assert_ne!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        text.contains("triggered touch page the plan did not list"),
        "{text}"
    );
    assert!(
        text.contains("  touch page pending (config changes)"),
        "{text}"
    );
    assert_eq!(acts(&dir), "");
    assert_eq!(
        state(&dir)["resourceset/web"]["pending"][0]["action"],
        "touch"
    );
}
