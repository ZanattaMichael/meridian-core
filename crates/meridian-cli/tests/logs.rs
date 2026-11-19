//! Tool logs: each step's tool output is kept, masked, under
//! `runs/<id>/logs/` in the state directory; `meridian logs` reads it back
//! and `meridian bug-report` packs a run into one archive.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// The text of a secret the second stage fetches, which is also an output
/// of the first stage, so the tool of the second echoes it in its inputs.
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
    - id: motd
      type: file
      when: secret.db_password != ''
      params:
        content: "welcome\n"
    - id: banner
      type: file
      params:
        content: "MOTD_BANNER"
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn scratch(test: &str, site: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("logs-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), site).unwrap();
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The one run kept under the state directory.
fn only_run(dir: &Path) -> String {
    let mut runs: Vec<String> = fs::read_dir(dir.join(".meridian/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.remove(0)
}

fn logs_dir(dir: &Path, run: &str) -> PathBuf {
    dir.join(".meridian/runs").join(run).join("logs")
}

/// The one object on stdout, checked against `schemas/output/<name>`.
fn checked(output: &Output, name: &str) -> Value {
    let text = stdout(output);
    let value: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}):\n{text}"));
    let path = repo_root().join(format!("schemas/output/{name}.schema.json"));
    let schema: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}\n{text}");
    assert_eq!(value["command"], name);
    value
}

#[test]
fn each_step_keeps_its_tool_lines_in_order_with_secrets_masked() {
    let dir = scratch("kept", SITE);
    let out = meridian(&dir, &["apply", "site.yaml"]);
    assert!(out.status.success(), "{}", text(&out));
    let run = only_run(&dir);
    let logs = logs_dir(&dir, &run);

    // One file per step, the ResourceSet's named after the machine.
    let network = fs::read_to_string(logs.join("infrastructure/network.log")).unwrap();
    let web = fs::read_to_string(logs.join("resourceset/web@10.0.0.5.log")).unwrap();
    let lines: Vec<&str> = web.lines().collect();
    // `<time> <stream> <text>`, in the order the tool printed them.
    let shown: Vec<String> = lines
        .iter()
        .map(|l| l.splitn(3, ' ').skip(1).collect::<Vec<_>>().join(" "))
        .collect();
    let inputs = shown
        .iter()
        .position(|l| l.starts_with("stderr inputs:"))
        .unwrap();
    let banner = shown
        .iter()
        .position(|l| l == "stdout create files/banner")
        .unwrap();
    let motd = shown
        .iter()
        .position(|l| l == "stdout create files/motd")
        .unwrap();
    assert!(inputs < banner && banner < motd, "{web}");
    assert!(
        lines
            .iter()
            .all(|l| l.as_bytes()[4] == b'-' && l.contains('Z')),
        "{web}"
    );

    // The tool echoed the secret; the file holds the mask.
    assert!(shown[inputs].contains("(sensitive)"), "{web}");
    assert!(!web.contains(PASSWORD), "{web}");
    assert!(network.contains("stdout create files/vm"), "{network}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(logs.join("resourceset/web@10.0.0.5.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn logs_filters_by_step_stream_text_and_tail() {
    let dir = scratch("filters", SITE);
    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());

    // No step failed, so every step is shown.
    let all = stdout(&meridian(&dir, &["logs"]));
    assert!(
        all.contains("== network (Infrastructure, target fake): succeeded"),
        "{all}"
    );
    assert!(
        all.contains("== web (ResourceSet, target fake on 10.0.0.5): succeeded"),
        "{all}"
    );

    let web = stdout(&meridian(
        &dir,
        &["logs", "--step", "web", "--stream", "stdout"],
    ));
    assert!(!web.contains("== network"), "{web}");
    assert!(web.contains(" stdout create files/motd"), "{web}");
    assert!(!web.contains(" stderr "), "{web}");

    let grep = stdout(&meridian(
        &dir,
        &["logs", "--step", "web", "--grep", "ban+er"],
    ));
    let body: Vec<&str> = grep.lines().skip(2).collect();
    assert_eq!(body.len(), 1, "{grep}");
    assert!(body[0].ends_with(" stdout create files/banner"), "{grep}");

    let tail = stdout(&meridian(
        &dir,
        &["logs", "--endpoint", "10.0.0.5", "--tail", "1"],
    ));
    assert_eq!(tail.lines().count(), 3, "{tail}");

    let missing = meridian(&dir, &["logs", "--step", "nothing"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        text(&missing).contains("has no step nothing"),
        "{}",
        text(&missing)
    );

    let json = meridian(&dir, &["logs", "--step", "web", "--output", "json"]);
    let value = checked(&json, "logs");
    assert_eq!(value["steps"][0]["endpoint"], "10.0.0.5");
    assert!(value["steps"][0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["text"] == "create files/motd" && l["stream"] == "stdout"));
}

#[test]
fn with_no_arguments_logs_shows_the_failed_step_and_why() {
    let site = SITE.replace("MOTD_BANNER", "FAIL");
    let dir = scratch("failed", &site);
    let out = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let shown = stdout(&meridian(&dir, &["logs"]));
    assert!(!shown.contains("== network"), "{shown}");
    assert!(
        shown.contains("== web (ResourceSet, target fake on 10.0.0.5): failed (tool-failed)"),
        "{shown}"
    );
    let why = shown
        .lines()
        .find(|l| l.contains(" meridian step failed (tool-failed): "))
        .unwrap_or_else(|| panic!("{shown}"));
    assert!(why.contains("could not write files/banner"), "{why}");
}

#[test]
fn no_logs_keeps_nothing_and_the_limit_cuts_the_middle() {
    let dir = scratch("off", SITE);
    assert!(meridian(&dir, &["apply", "site.yaml", "--no-logs"])
        .status
        .success());
    let run = only_run(&dir);
    assert!(!logs_dir(&dir, &run).exists());
    let shown = stdout(&meridian(&dir, &["logs"]));
    assert!(
        shown.contains("no tool output was kept on this machine"),
        "{shown}"
    );

    let dir = scratch("limit", SITE);
    let out = meridian(&dir, &["apply", "site.yaml", "--log-limit", "200"]);
    assert!(out.status.success(), "{}", text(&out));
    let run = only_run(&dir);
    let web =
        fs::read_to_string(logs_dir(&dir, &run).join("resourceset/web@10.0.0.5.log")).unwrap();
    assert!(web.contains(" meridian cut "), "{web}");
    assert!(
        web.trim_end().ends_with("stdout create files/motd"),
        "{web}"
    );
}

#[test]
fn a_bug_report_packs_the_run_masked_and_sends_nothing() {
    let dir = scratch("bundle", SITE);
    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    let run = only_run(&dir);

    let out = meridian(
        &dir,
        &["bug-report", "--out", "report.tar.gz", "--output", "json"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let value = checked(&out, "bug-report");
    assert_eq!(value["run"], run.as_str());
    assert_eq!(value["out"], "report.tar.gz");

    let bytes = fs::read(dir.join("report.tar.gz")).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    let mut names = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().display().to_string();
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        assert!(!content.contains(PASSWORD), "{name} holds the secret");
        if name.ends_with("plugins.json") {
            assert!(content.contains("\"name\": \"fake\""), "{content}");
        }
        names.push(name);
    }
    let root = format!("meridian-bug-report-{run}");
    for expected in [
        "run.json",
        "events.jsonl",
        "logs/infrastructure/network.log",
        "logs/resourceset/web@10.0.0.5.log",
        "version.txt",
        "plugins.json",
    ] {
        assert!(
            names.contains(&format!("{root}/{expected}")),
            "{expected}: {names:?}"
        );
    }
    let files: Vec<String> = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| format!("{root}/{}", f.as_str().unwrap()))
        .collect();
    assert_eq!(files, names);
}
