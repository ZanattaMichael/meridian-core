//! `--output json`: every command prints one object that its published
//! schema under `schemas/output/` accepts, and `plan --detailed-exitcode`
//! exits 2 when an apply would do something.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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
    - id: key
      type: file
      params:
        content: hunter2-very-secret
  outputs:
    - name: address
      value: "${vm.content}"
    - name: secret_key
      value: "${key.content}"
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
        content: "welcome\n"
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn scratch(test: &str, workflow: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("output-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), workflow).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .arg("--output")
        .arg("json")
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_OUTPUT")
        .output()
        .expect("meridian runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The one object on stdout, checked against the schema `name`, which is
/// the command's own name with a hyphen for its space.
fn object(output: &Output, name: &str) -> Value {
    let value = object_against(output, name);
    assert_eq!(value["command"], Value::from(name.replace('-', " ")));
    value
}

/// The run object, which five commands share, named after its mode.
fn run(output: &Output, mode: &str) -> Value {
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let value = object_against(output, "run");
    assert_eq!(value["command"], Value::from(mode), "{text}");
    assert_eq!(value["mode"], Value::from(mode), "{text}");
    value
}

/// The one object on stdout, checked against the schema `name`.
fn object_against(output: &Output, name: &str) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}):\n{text}"));
    let path = repo_root().join(format!("schemas/output/{name}.schema.json"));
    let schema: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema)
        .unwrap_or_else(|e| panic!("{} is not a valid schema: {e}", path.display()));
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        problems.is_empty(),
        "{name}.schema.json refuses the output: {problems:#?}\n{text}"
    );
    value
}

fn statuses(run: &Value) -> Vec<&str> {
    run["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["status"].as_str().unwrap())
        .collect()
}

#[test]
fn plan_and_apply_print_one_object_each_and_the_detailed_exit_code_says_whether_there_is_work() {
    let dir = scratch("lifecycle", WORKFLOW);

    let plan = meridian(&dir, &["plan", "site.yaml", "--detailed-exitcode"]);
    assert_eq!(plan.status.code(), Some(2), "{}", stderr(&plan));
    let value = run(&plan, "plan");
    assert_eq!(value["ok"], Value::Bool(true));
    assert_eq!(statuses(&value), ["planned", "deferred"]);
    assert_eq!(value["summary"]["hasChanges"], Value::Bool(true));
    assert_eq!(value["summary"]["changes"], Value::from(4));
    assert_eq!(value["stages"][0]["changes"][0]["action"], "create");
    assert_eq!(value["stages"][1]["waitingOn"]["name"], "network");
    assert_eq!(value["stages"][1]["waitingFor"], "address");
    // The report for people moved to stderr, as with --events json.
    assert!(
        stderr(&plan).contains("network (Infrastructure, target fake)\n"),
        "{}",
        stderr(&plan)
    );

    let apply = meridian(&dir, &["apply", "site.yaml"]);
    assert!(apply.status.success(), "{}", stderr(&apply));
    let value = run(&apply, "apply");
    assert_eq!(statuses(&value), ["applied", "applied"]);
    assert_eq!(value["summary"]["succeeded"], Value::Bool(true));
    assert_eq!(value["stages"][1]["host"], "10.0.0.5");
    let outputs = value["stages"][0]["outputs"].as_array().unwrap();
    let named = |name: &str| outputs.iter().find(|o| o["name"] == name).unwrap();
    assert_eq!(named("address")["value"], "10.0.0.5");
    assert_eq!(named("address")["sensitive"], Value::Bool(false));
    assert_eq!(named("secret_key")["sensitive"], Value::Bool(true));
    assert_eq!(named("secret_key")["value"], "(sensitive)");
    let text = String::from_utf8_lossy(&apply.stdout);
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!stderr(&apply).contains("hunter2"));
    assert!(value["stages"][0]["artifact"]["hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    // Nothing left to do: 0, and the object says so.
    let again = meridian(&dir, &["plan", "site.yaml", "--detailed-exitcode"]);
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    let value = run(&again, "plan");
    assert_eq!(value["summary"]["hasChanges"], Value::Bool(false));
    assert_eq!(value["stages"][0]["changes"], Value::Array(Vec::new()));
    assert_eq!(value["stages"][0]["unknown"], Value::Bool(false));

    // Without the flag a plan with changes still exits 0.
    let text_mode = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["plan", "site.yaml"])
        .current_dir(&dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .output()
        .unwrap();
    assert_eq!(text_mode.status.code(), Some(0));
    let with_flag = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["plan", "site.yaml", "--detailed-exitcode"])
        .current_dir(&dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .output()
        .unwrap();
    assert_eq!(with_flag.status.code(), Some(0));
    fs::write(
        dir.join("site.yaml"),
        WORKFLOW.replace("welcome", "changed"),
    )
    .unwrap();
    let with_flag = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["plan", "site.yaml", "--detailed-exitcode"])
        .current_dir(&dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .output()
        .unwrap();
    assert_eq!(with_flag.status.code(), Some(2), "{}", stderr(&with_flag));
    assert!(String::from_utf8_lossy(&with_flag.stdout).contains("~ update files/motd"));
}

#[test]
fn compile_drift_status_runs_and_a_failed_apply_as_json() {
    let dir = scratch("drift", WORKFLOW);

    let compile = meridian(&dir, &["compile", "site.yaml", "--out", "build"]);
    assert!(compile.status.success(), "{}", stderr(&compile));
    let value = run(&compile, "compile");
    assert!(value["run"].is_string());
    assert_eq!(statuses(&value), ["compiled", "deferred"]);
    assert!(value["stages"][0]["writtenTo"]
        .as_str()
        .unwrap()
        .ends_with("network"));
    assert!(dir.join("build/network").is_dir());

    let never = meridian(&dir, &["drift", "site.yaml"]);
    assert_eq!(never.status.code(), Some(0), "{}", stderr(&never));
    let value = run(&never, "drift");
    assert_eq!(value["stages"][0]["drift"], "never-applied");

    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    let tool_state = dir.join(".meridian/resourceset/web/fake/state.json");
    let text = fs::read_to_string(&tool_state).unwrap();
    fs::write(&tool_state, text.replace("welcome", "edited by hand")).unwrap();
    let drifted = meridian(&dir, &["drift", "site.yaml"]);
    assert_eq!(drifted.status.code(), Some(2), "{}", stderr(&drifted));
    let value = run(&drifted, "drift");
    // The check did its job, so it is ok; what it found is in the summary.
    assert_eq!(value["ok"], Value::Bool(true));
    assert_eq!(value["summary"]["drifted"], Value::from(1));
    assert_eq!(value["stages"][0]["drift"], "in-sync");
    assert_eq!(value["stages"][1]["drift"], "drifted");
    assert_eq!(value["stages"][1]["changes"][0]["resource"], "files/motd");

    let status = meridian(&dir, &["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let value = object(&status, "status");
    assert_eq!(value["run"]["mode"], "drift");
    assert_eq!(value["run"]["status"], "succeeded");
    assert_eq!(value["run"]["steps"][1]["name"], "web");
    assert_eq!(value["lock"], Value::Null);

    let runs = meridian(&dir, &["runs"]);
    assert!(runs.status.success(), "{}", stderr(&runs));
    let value = object(&runs, "runs");
    let modes: Vec<&str> = value["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["mode"].as_str().unwrap())
        .collect();
    assert_eq!(modes, ["drift", "apply", "drift"]);

    fs::write(
        dir.join("site.yaml"),
        WORKFLOW.replace("content: 10.0.0.5", "content: FAIL"),
    )
    .unwrap();
    let failed = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(failed.status.code(), Some(1));
    let value = run(&failed, "apply");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(statuses(&value), ["failed", "not-run"]);
    assert_eq!(value["stages"][0]["reason"], "tool-failed");
    assert!(value["stages"][0]["error"]
        .as_str()
        .unwrap()
        .contains("could not write files/vm"));
    assert_eq!(value["stages"][1]["becauseOf"]["name"], "network");
    assert_eq!(value["summary"]["failed"], Value::from(1));
}

#[test]
fn validate_lint_fmt_plugins_version_and_migrate_as_json() {
    let dir = scratch("tools", WORKFLOW);

    let valid = meridian(&dir, &["validate", "site.yaml"]);
    assert!(valid.status.success(), "{}", stderr(&valid));
    let value = object(&valid, "validate");
    assert_eq!(value["documents"], Value::from(2));

    fs::write(
        dir.join("bad.yaml"),
        WORKFLOW.replace("ref: network", "ref: nowhere"),
    )
    .unwrap();
    let invalid = meridian(&dir, &["validate", "bad.yaml"]);
    assert_eq!(invalid.status.code(), Some(1));
    let value = object(&invalid, "validate");
    assert_eq!(value["ok"], Value::Bool(false));
    let diagnostic = &value["diagnostics"][0];
    assert_eq!(diagnostic["file"], "bad.yaml");
    assert!(diagnostic["line"].as_u64().unwrap() > 0);
    assert!(diagnostic["message"].as_str().unwrap().contains("nowhere"));
    // Any command that reads documents prints the same when they do not read.
    let plan = meridian(&dir, &["plan", "bad.yaml"]);
    assert_eq!(plan.status.code(), Some(1));
    let value = object_against(&plan, "validate");
    assert_eq!(value["command"], "plan");
    assert_eq!(value["ok"], Value::Bool(false));

    let lint = meridian(&dir, &["lint", "site.yaml"]);
    assert!(lint.status.success(), "{}", stderr(&lint));
    let value = object(&lint, "lint");
    assert_eq!(value["summary"]["documents"], Value::from(2));
    assert_eq!(value["summary"]["pluginsFound"], Value::Bool(true));
    let lint = meridian(&dir, &["lint", "bad.yaml"]);
    assert_eq!(lint.status.code(), Some(1));
    let value = object(&lint, "lint");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["diagnostics"][0]["severity"], "error");
    assert!(value["summary"]["errors"].as_u64().unwrap() > 0);

    let fmt = meridian(&dir, &["fmt", "--check", "site.yaml"]);
    assert!(fmt.status.success(), "{}", stderr(&fmt));
    let value = object(&fmt, "fmt");
    assert_eq!(value["files"][0]["status"], "ok");
    fs::write(
        dir.join("loose.yaml"),
        WORKFLOW.replace("\n  resources:", "\n  resources:   "),
    )
    .unwrap();
    let fmt = meridian(&dir, &["fmt", "--check", "loose.yaml"]);
    assert_eq!(fmt.status.code(), Some(1));
    let value = object(&fmt, "fmt");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["files"][0]["status"], "not-formatted");
    let fmt = meridian(&dir, &["fmt", "loose.yaml"]);
    assert!(fmt.status.success());
    let value = object(&fmt, "fmt");
    assert_eq!(value["files"][0]["status"], "formatted");
    assert_eq!(value["summary"]["changed"], Value::from(1));

    let plugins = meridian(&dir, &["plugins"]);
    assert!(plugins.status.success(), "{}", stderr(&plugins));
    let value = object(&plugins, "plugins");
    let fake = &value["plugins"][0];
    assert_eq!(fake["name"], "fake");
    assert_eq!(fake["category"], "containerization");
    assert_eq!(fake["can"][0], "compile");
    assert_eq!(fake["signature"]["state"], "not-checked");
    assert_eq!(fake["imports"][0]["patterns"][0], "*.fake");
    assert_eq!(fake["protocol"], Value::from(2));

    let version = meridian(&dir, &["version"]);
    assert!(version.status.success());
    let value = object(&version, "version");
    assert_eq!(value["languages"][0]["name"], "meridian/v1");
    assert_eq!(value["languages"][0]["status"], "supported");

    let migrate = meridian(&dir, &["migrate", "site.yaml", "--check"]);
    assert!(migrate.status.success(), "{}", stderr(&migrate));
    let value = object(&migrate, "migrate");
    assert_eq!(value["files"][0]["status"], "current");
    assert_eq!(value["to"], "meridian/v1");
}

#[test]
fn signing_modules_secrets_and_rules_as_json() {
    let dir = scratch("more", WORKFLOW);

    let keygen = meridian(
        &dir,
        &[
            "signing", "keygen", "--secret", "k.key", "--public", "k.pub",
        ],
    );
    assert!(keygen.status.success(), "{}", stderr(&keygen));
    let value = object(&keygen, "signing-keygen");
    let key_id = value["keyId"].as_str().unwrap().to_owned();
    fs::write(dir.join("meridian-target-thing"), "binary").unwrap();
    let sign = meridian(
        &dir,
        &["signing", "sign", "--key", "k.key", "meridian-target-thing"],
    );
    assert!(sign.status.success(), "{}", stderr(&sign));
    let value = object(&sign, "signing-sign");
    assert_eq!(value["keyId"], Value::from(key_id.clone()));
    assert_eq!(value["signed"][0]["binary"], "meridian-target-thing");
    let verify = meridian(
        &dir,
        &[
            "signing",
            "verify",
            "--trusted-key",
            "k.pub",
            "meridian-target-thing",
        ],
    );
    assert!(verify.status.success(), "{}", stderr(&verify));
    let value = object(&verify, "signing-verify");
    assert_eq!(value["binaries"][0]["verified"], Value::Bool(true));
    assert_eq!(
        value["binaries"][0]["signature"]["keyId"],
        Value::from(key_id)
    );
    let untrusted = meridian(&dir, &["signing", "verify", "meridian-target-thing"]);
    assert_eq!(untrusted.status.code(), Some(1));
    let value = object(&untrusted, "signing-verify");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["binaries"][0]["signature"]["state"], "invalid");

    // A project that requires no modules: the lists are empty.
    fs::write(
        dir.join("meridian.yaml"),
        "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: test/site\nspec: {}\n",
    )
    .unwrap();
    let list = meridian(&dir, &["module", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let value = object(&list, "module-list");
    assert_eq!(value["modules"], Value::Array(Vec::new()));
    let verify = meridian(&dir, &["module", "verify"]);
    assert!(verify.status.success(), "{}", stderr(&verify));
    object(&verify, "module-verify");

    fs::write(
        dir.join("secrets.yaml"),
        r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: fake
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: db_password
      from: env
      key: OUTPUT_TEST_DB_PASSWORD
    - name: extra
      from: env
      key: OUTPUT_TEST_NOT_SET_ANYWHERE
      optional: true
  resources:
    - id: motd
      type: file
      params:
        content: "${secret.db_password}"
"#,
    )
    .unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["secret", "check", "secrets.yaml", "--output", "json"])
        .current_dir(&dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env("OUTPUT_TEST_DB_PASSWORD", "hunter2")
        .output()
        .unwrap();
    assert!(check.status.success(), "{}", stderr(&check));
    let value = object(&check, "secret-check");
    let secrets = &value["documents"][0]["secrets"];
    assert_eq!(secrets[0]["state"], "ok");
    assert_eq!(secrets[1]["state"], "absent-optional");
    assert_eq!(value["declared"], Value::from(2));
    assert!(!String::from_utf8_lossy(&check.stdout).contains("hunter2"));
    let missing = meridian(&dir, &["secret", "check", "secrets.yaml"]);
    assert_eq!(missing.status.code(), Some(1));
    let value = object(&missing, "secret-check");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["documents"][0]["secrets"][0]["state"], "missing");
    assert_eq!(value["unanswered"], Value::from(1));

    let rules = repo_root().join("examples/compile-rules");
    let web01 = [
        "rules",
        "check",
        "--hierarchy",
        "config/hierarchy.yaml",
        "--set",
        "env=prod",
        "--set",
        "role=web",
        "--set",
        "node=web01",
    ];
    let kept = meridian(&rules, &web01);
    assert!(kept.status.success(), "{}", stderr(&kept));
    let value = object(&kept, "rules-check");
    assert_eq!(value["rulesFile"], "config/rules.yaml");
    assert_eq!(value["violations"], Value::Array(Vec::new()));
    assert!(value["layers"].as_array().unwrap().len() > 1);
    let mut rogue = web01.to_vec();
    rogue[9] = "node=rogue";
    let broken = meridian(&rules, &rogue);
    assert_eq!(broken.status.code(), Some(1));
    let value = object(&broken, "rules-check");
    assert_eq!(value["ok"], Value::Bool(false));
    let violations = value["violations"].as_array().unwrap();
    assert!(!violations.is_empty());
    assert!(violations.iter().all(|v| v["rule"].is_string()));
}

#[test]
fn a_failure_before_a_result_is_an_object_too_and_events_cannot_share_stdout() {
    let dir = scratch("errors", WORKFLOW);

    let none = meridian(&dir, &["plan", "site.yaml", "-l", "tier=none"]);
    assert_eq!(none.status.code(), Some(1));
    let value = object_against(&none, "error");
    assert_eq!(value["command"], "plan");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no document's labels match"));
    assert!(stderr(&none).starts_with("error: no document's labels match"));

    let both = meridian(&dir, &["plan", "site.yaml", "--events", "json"]);
    assert_eq!(both.status.code(), Some(1));
    let value = object_against(&both, "error");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--events json"));

    // A refused run is reported in the run object, not as a bare error.
    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    let nothing = meridian(&dir, &["apply", "site.yaml", "--resume"]);
    assert_eq!(nothing.status.code(), Some(1), "{}", stderr(&nothing));
    let value = run(&nothing, "apply");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["refused"]["kind"], "resume");

    // The environment variable is the flag.
    let by_env = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["version"])
        .current_dir(&dir)
        .env("MERIDIAN_OUTPUT", "json")
        .output()
        .unwrap();
    assert!(by_env.status.success());
    object(&by_env, "version");
}

/// Every schema is a valid draft 7 schema, names itself after its file, and
/// is described on the reference page; and the page names no schema that
/// is not there.
#[test]
fn every_output_schema_is_valid_and_documented() {
    let dir = repo_root().join("schemas/output");
    let page = fs::read_to_string(repo_root().join("docs/reference/cli-json.md")).unwrap();
    let mut names = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(name.ends_with(".schema.json"), "{name}");
        let schema: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        jsonschema::draft7::new(&schema).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            schema["$id"],
            Value::from(format!(
                "https://github.com/ZanattaMichael/meridian-core/schemas/output/{name}"
            ))
        );
        assert!(
            page.contains(&format!("`schemas/output/{name}`")),
            "docs/reference/cli-json.md does not describe {name}"
        );
        names.push(name);
    }
    assert!(names.len() >= 20, "{names:?}");
    for named in page.match_indices("`schemas/output/") {
        let rest = &page[named.0 + "`schemas/output/".len()..];
        let name = &rest[..rest.find('`').unwrap()];
        // The directory itself is linked once; every file named is there.
        assert!(
            name.is_empty() || names.iter().any(|n| n == name),
            "the page names {name}"
        );
    }
}
