//! Reports and sinks, run as a process against a real plugin: every format
//! is written from one run, a recorded run reports the same, a sink is sent
//! the events, and no value ever reaches a report or a sink.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A secret in a resource, in a sensitive output and in the tool's chatter.
const SECRET: &str = "hunter2-very-secret";

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
        content: "MOTD"
"#;

fn scratch(test: &str, motd: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("reporting")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), WORKFLOW.replace("MOTD", motd)).unwrap();
    dir
}

/// The plugin directories: the fake target, and the reference sinks shipped
/// in `examples/sinks`.
fn plugin_dirs() -> String {
    let sinks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sinks");
    format!(
        "{}:{}",
        meridian_target_fake::plugin_dir().display(),
        sinks.display()
    )
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", plugin_dirs())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .env_remove("MERIDIAN_SINKS")
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn read(dir: &Path, file: &str) -> String {
    fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Writes an executable script into `dir` and returns its path.
fn script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[test]
fn an_apply_writes_every_report_format_and_none_holds_a_value() {
    let dir = scratch("formats", "welcome");
    let applied = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--report",
            "json=out/report.json",
            "--report",
            "junit=out/report.xml",
            "--report",
            "markdown=out/report.md",
            "--report",
            "html=out/report.html",
        ],
    );
    assert!(applied.status.success(), "{}", stderr(&applied));
    for format in ["json", "junit", "markdown", "html"] {
        assert!(
            stderr(&applied).contains(&format!("report: wrote {format} to out/report.")),
            "{}",
            stderr(&applied)
        );
    }

    let json = read(&dir, "out/report.json");
    for expected in [
        "\"schema\": \"meridian.report/v1\"",
        "\"mode\": \"apply\"",
        "\"status\": \"succeeded\"",
        "\"command\": \"apply site.yaml\"",
        "\"name\": \"network\"",
        "\"endpoint\": \"10.0.0.5\"",
        "\"name\": \"secret_key\"",
        "\"sensitive\": true",
        "\"phase\": \"changed\"",
    ] {
        assert!(
            json.contains(expected),
            "{expected} is missing from:\n{json}"
        );
    }

    let junit = read(&dir, "out/report.xml");
    assert!(junit.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"meridian apply\" tests=\"2\" failures=\"0\" errors=\"0\" skipped=\"0\""), "{junit}");
    assert!(
        junit.contains("<testcase classname=\"meridian.fake\" name=\"Infrastructure network\""),
        "{junit}"
    );
    assert!(
        junit.contains("<testcase classname=\"meridian.fake\" name=\"ResourceSet web\""),
        "{junit}"
    );

    let markdown = read(&dir, "out/report.md");
    assert!(
        markdown.starts_with("## ✓ Meridian apply succeeded: 2 steps, "),
        "{markdown}"
    );
    assert!(
        markdown.contains("| ResourceSet web | fake | 10.0.0.5 | ✓ succeeded | 1 changed |"),
        "{markdown}"
    );
    assert!(
        markdown.contains("- output <code>secret_key</code> (sensitive)"),
        "{markdown}"
    );

    let html = read(&dir, "out/report.html");
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(
        html.contains("<h1 class=\"ok\">Meridian apply succeeded: 2 steps, "),
        "{html}"
    );

    // The secret is in a resource, in an output and in what the tool
    // printed. It is in no report.
    for file in ["report.json", "report.xml", "report.md", "report.html"] {
        let text = read(&dir, &format!("out/{file}"));
        assert!(!text.contains(SECRET), "{file} holds the secret:\n{text}");
    }

    // The recorded run reports the same as the live one did.
    let recorded = meridian(&dir, &["report", "--format", "json"]);
    assert!(recorded.status.success(), "{}", stderr(&recorded));
    assert_eq!(stdout(&recorded), json);
    let page = meridian(&dir, &["report", "--format", "junit", "--out", "again.xml"]);
    assert!(page.status.success(), "{}", stderr(&page));
    assert_eq!(read(&dir, "again.xml"), junit);
}

#[test]
fn a_failed_apply_is_a_failed_test_case_and_says_what_to_do_next() {
    let dir = scratch("failed", "FAIL welcome");
    let failed = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--report",
            "junit=report.xml",
            "--report",
            "md=report.md",
        ],
    );
    assert_eq!(failed.status.code(), Some(1));

    let junit = read(&dir, "report.xml");
    assert!(
        junit.contains("tests=\"2\" failures=\"1\" errors=\"0\" skipped=\"0\""),
        "{junit}"
    );
    assert!(
        junit.contains(
            "<failure type=\"tool-failed\" message=\"failed: the tool reported a failure\"/>"
        ),
        "{junit}"
    );
    // What the tool said stays on the terminal: a tool can quote a value.
    let terminal = format!("{}{}", stdout(&failed), stderr(&failed));
    assert!(terminal.contains("disk on fire"), "{terminal}");
    assert!(!junit.contains("disk on fire"), "{junit}");

    let markdown = read(&dir, "report.md");
    assert!(
        markdown.starts_with("## ✗ Meridian apply failed: 2 steps, 1 failed"),
        "{markdown}"
    );
    assert!(
        markdown.contains("- **ResourceSet web** failed: the tool reported a failure"),
        "{markdown}"
    );
    assert!(markdown.contains("`meridian apply --resume`"), "{markdown}");
}

#[test]
fn a_plan_reports_what_would_change() {
    let dir = scratch("plan", "welcome");
    let planned = meridian(&dir, &["plan", "site.yaml", "--report", "json=plan.json"]);
    assert!(planned.status.success(), "{}", stderr(&planned));
    let json = read(&dir, "plan.json");
    assert!(json.contains("\"mode\": \"plan\""), "{json}");
    assert!(json.contains("\"phase\": \"planned\""), "{json}");
    // The second step waits on infrastructure that a plan does not create.
    assert!(json.contains("\"status\": \"deferred\""), "{json}");
    assert!(json.contains("\"waitingFor\": \"address\""), "{json}");
}

#[test]
fn a_report_is_made_from_a_file_of_events_and_from_standard_input() {
    let dir = scratch("from", "welcome");
    let applied = meridian(&dir, &["apply", "site.yaml", "--events", "json"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    fs::write(dir.join("events.jsonl"), stdout(&applied)).unwrap();

    let from_file = meridian(&dir, &["report", "--from", "events.jsonl"]);
    assert!(from_file.status.success(), "{}", stderr(&from_file));
    assert!(
        stdout(&from_file).starts_with("## ✓ Meridian apply succeeded: 2 steps, "),
        "{}",
        stdout(&from_file)
    );

    // The same events on standard input give the same report.
    let mut piped = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["report", "--from", "-"])
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut piped.stdin.take().unwrap(), &applied.stdout).unwrap();
    let piped = piped.wait_with_output().unwrap();
    assert_eq!(stdout(&piped), stdout(&from_file));

    fs::write(dir.join("empty.jsonl"), "not events\n").unwrap();
    let empty = meridian(&dir, &["report", "--from", "empty.jsonl"]);
    assert_eq!(empty.status.code(), Some(1));
    assert_eq!(
        stderr(&empty),
        "error: empty.jsonl holds no events; it should be what `--events json` wrote\n"
    );
}

#[test]
fn a_bad_report_or_sink_is_refused_before_anything_runs() {
    let dir = scratch("refused", "welcome");
    for (args, error) in [
        (
            vec!["--report", "pdf=report.pdf"],
            "error: \"pdf\" is not a report format; the formats are json, junit, markdown and html\n",
        ),
        (
            vec!["--report", "junit"],
            "error: --report \"junit\" is not FORMAT=FILE, as in junit=meridian.xml\n",
        ),
        (
            vec!["--sink", "pager"],
            "no sink \"pager\": there is no program named meridian-notify-pager in ",
        ),
        (
            vec!["--sink", "command"],
            "error: the command sink needs the program to run: --sink-config command.program=PATH\n",
        ),
        (
            vec!["--sink", "file", "--sink-config", "flie.path=x"],
            "error: --sink-config flie.path is for a sink that was not asked for; add --sink flie\n",
        ),
        (
            vec!["--sink", "file", "--sink-config", "file.on=step failed"],
            "error: sink \"file\": \"step failed\" is not an event type; give a type such as step.failed, a family such as step.*, or *\n",
        ),
    ] {
        let mut all = vec!["apply", "site.yaml"];
        all.extend(args);
        let refused = meridian(&dir, &all);
        assert_eq!(refused.status.code(), Some(1), "{all:?}");
        assert!(stderr(&refused).contains(error), "{all:?}: {}", stderr(&refused));
        // Nothing ran, so nothing was recorded.
        assert!(!dir.join(".meridian/runs").exists(), "{all:?}");
    }
}

#[test]
fn a_sink_is_sent_the_events_it_asked_for_and_never_a_value() {
    let dir = scratch("sink", "welcome");
    let applied = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--sink",
            "file",
            "--sink-config",
            "file.path=all.jsonl",
            "--sink",
            "command",
            "--sink-config",
            &format!(
                "command.program={}",
                script(&dir, "steps.sh", "cat > steps.jsonl")
            ),
            "--sink-config",
            "command.on=step.*,run.succeeded",
        ],
    );
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert!(
        !stderr(&applied).contains("warning"),
        "{}",
        stderr(&applied)
    );

    // The sink saw exactly what the record holds, in order.
    let recorded = meridian(&dir, &["events"]);
    let all = read(&dir, "all.jsonl");
    assert_eq!(all, stdout(&recorded));
    assert!(!all.contains(SECRET), "{all}");

    let steps = read(&dir, "steps.jsonl");
    let kinds: Vec<&str> = steps
        .lines()
        .map(|line| {
            let at = line.find("\"type\":\"").unwrap() + 8;
            &line[at..at + line[at..].find('"').unwrap()]
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "step.started",
            "step.succeeded",
            "step.started",
            "step.succeeded",
            "run.succeeded"
        ]
    );
}

#[test]
fn a_sink_that_fails_is_a_warning_unless_it_is_required() {
    let dir = scratch("sink-fails", "welcome");
    let broken = script(
        &dir,
        "broken.sh",
        "cat > /dev/null\necho 'the pager is down' >&2\nexit 3",
    );
    let program = format!("command.program={broken}");

    let warned = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--sink",
            "command",
            "--sink-config",
            &program,
        ],
    );
    assert!(warned.status.success(), "{}", stderr(&warned));
    assert!(
        stderr(&warned).contains(
            "warning: sink \"command\" failed: it exited with status 3: the pager is down\n"
        ),
        "{}",
        stderr(&warned)
    );

    let required = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--sink",
            "command",
            "--sink-config",
            &program,
            "--sink-config",
            "command.required=true",
        ],
    );
    assert_eq!(required.status.code(), Some(1));
    assert!(
        stderr(&required).contains(
            "error: sink \"command\" failed: it exited with status 3: the pager is down\n"
        ),
        "{}",
        stderr(&required)
    );
}

#[test]
fn a_sink_that_hangs_is_stopped_and_does_not_hold_the_run() {
    let dir = scratch("sink-hangs", "welcome");
    let hangs = script(&dir, "hangs.sh", "cat > /dev/null\nexec sleep 600");
    let started = std::time::Instant::now();
    let applied = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--sink",
            "command",
            "--sink-config",
            &format!("command.program={hangs}"),
            "--sink-timeout",
            "1s",
        ],
    );
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert!(started.elapsed().as_secs() < 60);
    assert!(
        stderr(&applied).contains(
            "warning: sink \"command\" failed: it was still running 1 s after the run's last \
             event, and was stopped\n"
        ),
        "{}",
        stderr(&applied)
    );
}
