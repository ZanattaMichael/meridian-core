//! `--analyze`: a stage's compiled files are put through an analyzer
//! program, its findings are shown against the document, counted in the
//! record and written to reports, and a finding at `--fail-on` fails the
//! stage. Run as a process against a real plugin and a stand-in analyzer.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
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
        content: "vm on 10.0.0.5"
    - id: logs
      type: file
      params:
        content: "bucket logs is public"
"#;

/// An analyzer that reports every line holding `public` as a high finding
/// and every line holding `vm` as a low one, and keeps its settings.
const ANALYZER: &str = r#"#!/bin/sh
cat > /dev/null
printf '%s\n' "${MERIDIAN_ANALYZE_NOTE:-}" > "$MERIDIAN_TEST_NOTES"
findings=""
for file in $(find . -type f ! -name .meridian-files | sort); do
  path=${file#./}
  line=$(grep -nF public "$file" | head -1 | cut -d: -f1)
  [ -n "$line" ] && findings="$findings${findings:+,}{\"rule\":\"PUBLIC-BUCKET\",\"severity\":\"high\",\"message\":\"The bucket is public\",\"file\":\"$path\",\"line\":$line,\"url\":\"https://example.test/public\"}"
  line=$(grep -nF vm "$file" | head -1 | cut -d: -f1)
  [ -n "$line" ] && findings="$findings${findings:+,}{\"rule\":\"NAMED-VM\",\"severity\":\"low\",\"message\":\"A machine is named\",\"file\":\"$path\",\"line\":$line}"
done
printf '{"findings":[%s],"measures":[{"name":"monthly-cost","value":12.5,"unit":"USD"}]}\n' "$findings"
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("analyze")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::write(dir.join("site.yaml"), WORKFLOW).unwrap();
    let program = dir.join("bin/meridian-analyze-lines");
    fs::write(&program, ANALYZER).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(args)
        .current_dir(dir)
        .env(
            "MERIDIAN_PLUGIN_DIR",
            format!(
                "{}:{}",
                meridian_target_fake::plugin_dir().display(),
                dir.join("bin").display()
            ),
        )
        .env("MERIDIAN_TEST_NOTES", dir.join("notes"))
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .env_remove("MERIDIAN_OUTPUT")
        .env_remove("MERIDIAN_ANALYZERS")
        .env_remove("MERIDIAN_FAIL_ON")
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The one JSON object on stdout, checked against the run schema.
fn run_object(output: &Output) -> Value {
    let text = stdout(output);
    let value: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}):\n{text}"));
    let path = repo_root().join("schemas/output/run.schema.json");
    let schema: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        problems.is_empty(),
        "run.schema.json refuses the output: {problems:#?}\n{text}"
    );
    value
}

#[test]
fn findings_are_shown_against_the_document_and_fail_the_stage_at_the_threshold() {
    let dir = scratch("compile");
    let output = meridian(
        &dir,
        &[
            "compile",
            "site.yaml",
            "--analyze",
            "lines",
            "--analyze-config",
            "lines.note=hello",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("  analysis (lines): 1 high, 1 low\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "    high PUBLIC-BUCKET at site.yaml:12 (files/logs:1): The bucket is public\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("    low NAMED-VM at site.yaml:8 (files/vm:1): A machine is named\n"),
        "{text}"
    );
    assert!(text.contains("    monthly-cost: 12.5 USD\n"), "{text}");
    assert!(
        text.contains(
            "  FAILED: an analyzer found something at severity high or above, which this run \
             fails on (--fail-on high):\n      - high PUBLIC-BUCKET (lines) at site.yaml:12 \
             (files/logs:1): The bucket is public\n"
        ),
        "{text}"
    );
    // The setting reached the program as an environment variable.
    assert_eq!(
        fs::read_to_string(dir.join("notes")).unwrap().trim(),
        "hello"
    );

    // Below the threshold, the same findings are only shown.
    let output = meridian(
        &dir,
        &[
            "compile",
            "site.yaml",
            "--analyze",
            "lines",
            "--fail-on",
            "critical",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("high PUBLIC-BUCKET at site.yaml:12"));
    let output = meridian(
        &dir,
        &[
            "compile",
            "site.yaml",
            "--analyze",
            "lines",
            "--fail-on",
            "none",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let output = meridian(
        &dir,
        &[
            "compile",
            "site.yaml",
            "--analyze",
            "lines",
            "--fail-on",
            "low",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).contains("- low NAMED-VM (lines)"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_baseline_accepts_a_finding_and_the_record_keeps_every_one() {
    let dir = scratch("baseline");
    fs::write(
        dir.join("accepted.yaml"),
        "accepted:\n  - rule: PUBLIC-BUCKET\n    resource: logs\n    reason: a public bucket on purpose\n",
    )
    .unwrap();
    let output = meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--analyze",
            "lines",
            "--analyze-baseline",
            "accepted.yaml",
            "--report",
            "sarif=out/findings.sarif",
            "--report",
            "markdown=out/summary.md",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("  analysis (lines): 1 low, 1 accepted\n"),
        "{text}"
    );
    assert!(
        text.contains("    accepted high PUBLIC-BUCKET at site.yaml:12 (files/logs:1): The bucket is public\n"),
        "{text}"
    );

    // Every finding is an event, with the rule and the place.
    let events = meridian(&dir, &["events", "--type", "finding.reported"]);
    let events = stdout(&events);
    assert!(
        events.contains("\"rule\":\"PUBLIC-BUCKET\"") && events.contains("\"accepted\":true"),
        "{events}"
    );
    assert!(events.contains("\"documentLine\":12"), "{events}");
    let passed = meridian(&dir, &["events", "--type", "analysis.*"]);
    let passed = stdout(&passed);
    assert!(
        passed.contains("\"type\":\"analysis.passed\"") && passed.contains("\"accepted\":1"),
        "{passed}"
    );

    // The SARIF report points each finding at the document, and marks the
    // accepted one as suppressed.
    let sarif: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("out/findings.sarif")).unwrap()).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    let public = results
        .iter()
        .find(|r| r["ruleId"] == "PUBLIC-BUCKET")
        .unwrap();
    assert_eq!(
        public["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "site.yaml"
    );
    assert_eq!(
        public["locations"][0]["physicalLocation"]["region"]["startLine"],
        12
    );
    assert_eq!(public["suppressions"][0]["kind"], "external");
    let summary = fs::read_to_string(dir.join("out/summary.md")).unwrap();
    assert!(
        summary.contains("### Findings") && summary.contains("PUBLIC-BUCKET"),
        "{summary}"
    );

    // The same run, as a machine reads it.
    let output = meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--analyze",
            "lines",
            "--analyze-baseline",
            "accepted.yaml",
            "--output",
            "json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let run = run_object(&output);
    let analyses = run["stages"][0]["analyses"].as_array().unwrap();
    assert_eq!(analyses.len(), 1);
    assert_eq!(analyses[0]["analyzer"], "lines");
    assert_eq!(analyses[0]["asked"], true);
    assert_eq!(analyses[0]["counts"]["low"], 1);
    assert_eq!(analyses[0]["counts"]["accepted"], 1);
    let findings = analyses[0]["findings"].as_array().unwrap();
    assert_eq!(findings[0]["rule"], "PUBLIC-BUCKET");
    assert_eq!(findings[0]["accepted"], true);
    assert_eq!(findings[0]["document"], "site.yaml");
    assert_eq!(findings[0]["documentLine"], 12);
    assert_eq!(findings[0]["resource"], "logs");
    assert_eq!(analyses[0]["measures"][0]["value"], 12.5);
    assert_eq!(run["stages"][0]["status"], "planned");

    // A failing run, as a machine reads it.
    let output = meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--analyze",
            "lines",
            "--output",
            "json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let run = run_object(&output);
    assert_eq!(run["stages"][0]["status"], "failed");
    assert_eq!(run["stages"][0]["reason"], "analysis-failed");
    assert_eq!(run["ok"], false);
}

#[test]
fn a_missing_analyzer_a_bad_setting_or_the_wrong_command_is_refused_first() {
    let dir = scratch("refused");
    for (args, error) in [
        (
            vec!["plan", "site.yaml", "--analyze", "scanner"],
            "error: no analyzer \"scanner\": there is no program named meridian-analyze-scanner in ",
        ),
        (
            vec!["plan", "site.yaml", "--analyze", "lines", "--analyze-config", "lines=x"],
            "error: \"lines=x\" is not an analyzer setting; write it as NAME.KEY=VALUE\n",
        ),
        (
            vec!["plan", "site.yaml", "--analyze", "lines", "--analyze-config", "other.k=x"],
            "error: --analyze-config other.k=x: no analyzer \"other\" was named with --analyze\n",
        ),
        (
            vec!["plan", "site.yaml", "--analyze-config", "lines.k=x"],
            "error: --analyze-config and --analyze-baseline go with --analyze, and no analyzer was named\n",
        ),
        (
            vec!["drift", "site.yaml", "--analyze", "lines"],
            "error: --analyze is run by compile, plan and apply, and drift analyzes nothing; remove it\n",
        ),
        (
            vec!["plan", "site.yaml", "--analyze", "lines", "--analyze-baseline", "none.yaml"],
            "error: reading the baseline none.yaml: ",
        ),
    ] {
        let output = meridian(&dir, &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&output).starts_with(error),
            "{args:?} said {:?}",
            stderr(&output)
        );
    }
    let output = meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--analyze",
            "lines",
            "--fail-on",
            "loud",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("is not a severity to fail on"),
        "{}",
        stderr(&output)
    );

    // An analyzer that cannot be started fails the stage, and says so.
    fs::write(
        dir.join("bin/meridian-analyze-lines"),
        "#!/bin/sh\nexit 7\n",
    )
    .unwrap();
    let output = meridian(&dir, &["plan", "site.yaml", "--analyze", "lines"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(
        text.contains("  analysis (lines): could not look\n"),
        "{text}"
    );
    assert!(
        text.contains("FAILED: the analyzer \"lines\" could not look at this stage, and a scanner that was not run has not passed anything: "),
        "{text}"
    );
    let status = meridian(&dir, &["status"]);
    assert!(
        stdout(&status).contains("failed (analyzer-failed)"),
        "{}",
        stdout(&status)
    );
}
