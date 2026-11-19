//! Which resource a tool failed on: shown on the terminal, named with its
//! cause word in the events, and kept, masked, in `runs/<id>/failures.jsonl`
//! on this machine only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// An output of the first stage the second's tool echoes in what it says
/// about its failure.
const PASSWORD: &str = "kq7Zp2-not-a-real-password-9fXw";

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
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("failures-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("SITE_DB_PASSWORD", PASSWORD)
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_OUTPUT")
        .env_remove("MERIDIAN_NO_LOGS")
        .output()
        .expect("meridian runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn only_run(dir: &Path) -> PathBuf {
    let runs: Vec<PathBuf> = fs::read_dir(dir.join(".meridian/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs[0].clone()
}

/// Every file under a directory, read as text.
fn everything_under(dir: &Path) -> String {
    let mut all = String::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            all.push_str(&everything_under(&path));
        } else {
            all.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
        }
    }
    all
}

#[test]
fn a_failed_resource_is_shown_named_in_the_events_and_kept_masked_on_this_machine() {
    let dir = scratch("shown");
    let out = meridian(&dir, &["apply", "site.yaml", "--events", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));

    // The terminal: endpoint, resource, cause, summary, where it is declared.
    let shown = String::from_utf8_lossy(&out.stderr);
    assert!(
        shown.contains(
            "10.0.0.5  banner  resource-failed  could not write files/banner: disk on fire  (site.yaml:"
        ),
        "{shown}"
    );

    // The event names the resource and the cause, and nothing the tool said.
    let events: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let failed: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "resource.failed")
        .collect();
    assert_eq!(failed.len(), 1, "{events:#?}");
    assert_eq!(failed[0]["step"], "web");
    assert_eq!(failed[0]["endpoint"], "10.0.0.5");
    assert_eq!(
        failed[0]["detail"],
        serde_json::json!({ "kind": "ResourceSet", "resource": "banner", "cause": "resource-failed" })
    );

    // failures.jsonl holds the rest, with the echoed secret masked.
    let run = only_run(&dir);
    let kept = fs::read_to_string(run.join("failures.jsonl")).unwrap();
    let line: Value = serde_json::from_str(kept.lines().next().unwrap()).unwrap();
    assert_eq!(line["resource"], "banner");
    assert_eq!(line["cause"], "resource-failed");
    assert_eq!(line["toolCode"], "EFIRE");
    assert_eq!(line["endpoint"], "10.0.0.5");
    assert!(
        line["at"].as_str().unwrap().starts_with("site.yaml:"),
        "{line}"
    );
    assert!(
        line["detail"].as_str().unwrap().contains("(sensitive)"),
        "{line}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(run.join("failures.jsonl"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The secret is in nothing Meridian keeps of the run, and not in what
    // it printed about the failure. (The tool's own working files hold it,
    // as the first stage's content.)
    assert!(!everything_under(&run).contains(PASSWORD));
    let web = &shown[shown
        .find("web (ResourceSet")
        .expect("the web stage is shown")..];
    assert!(!web.contains(PASSWORD), "{web}");
}

#[test]
fn json_output_lists_each_failure_and_a_bug_report_carries_the_file() {
    let dir = scratch("json");
    let out = meridian(&dir, &["apply", "site.yaml", "--output", "json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("schemas/output/run.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
    let web = value["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "web")
        .unwrap();
    assert_eq!(web["failures"][0]["resource"], "banner");
    assert_eq!(web["failures"][0]["cause"], "resource-failed");
    assert!(!web.to_string().contains(PASSWORD), "{web}");

    let packed = meridian(
        &dir,
        &["bug-report", "--out", "report.tar.gz", "--output", "json"],
    );
    assert!(packed.status.success(), "{}", text(&packed));
    let packed: Value = serde_json::from_slice(&packed.stdout).unwrap();
    assert!(
        packed["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "failures.jsonl"),
        "{packed}"
    );
}
