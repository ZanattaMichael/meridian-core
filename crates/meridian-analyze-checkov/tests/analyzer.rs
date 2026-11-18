//! The analyzer, run as Meridian runs it, against a stand-in for `checkov`
//! that keeps how it was called and answers as `checkov -o json` does.
//! `tests/real.rs` does the same against the real thing.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use meridian_analyze::Input;
use meridian_analyze_checkov::{arguments, frameworks, translate};

/// What `checkov -d . -o json --framework terraform_json` prints for a
/// bucket with no logging and no versioning, shortened to the fields the
/// analyzer reads plus a few it does not.
const REPORT: &str = r#"{
  "check_type": "terraform_json",
  "results": {
    "passed_checks": [
      {"check_id": "CKV_AWS_19", "check_name": "Ensure the S3 bucket has server-side-encryption enabled",
       "check_result": {"result": "PASSED"}, "file_path": "/main.tf.json", "file_line_range": [3, 12],
       "resource": "aws_s3_bucket.logs"}
    ],
    "failed_checks": [
      {"check_id": "CKV_AWS_18", "bc_check_id": "BC_AWS_S3_13", "check_name": "Ensure the S3 bucket has access logging enabled",
       "check_result": {"result": "FAILED", "evaluated_keys": ["logging"]}, "file_path": "/main.tf.json",
       "file_abs_path": "/tmp/stage/main.tf.json", "file_line_range": [3, 12], "resource": "aws_s3_bucket.logs",
       "guideline": "https://docs.prismacloud.io/en/enterprise-edition/policy-reference/aws-policies/s3-policies/s3-13-enable-logging",
       "severity": null},
      {"check_id": "CKV_AWS_21", "check_name": "Ensure all data stored in the S3 bucket have versioning enabled",
       "check_result": {"result": "FAILED"}, "file_path": "/main.tf.json", "file_line_range": [3, 12],
       "resource": "aws_s3_bucket.logs", "guideline": null, "severity": "LOW"}
    ],
    "skipped_checks": [],
    "parsing_errors": []
  },
  "summary": {"passed": 1, "failed": 2, "skipped": 0, "parsing_errors": 0, "resource_count": 1, "checkov_version": "3.2.0"}
}"#;

const CHECKOV: &str = r#"#!/bin/sh
here=$(dirname "$0")
printf '%s\n' "$*" > "$here/args"
case "$MERIDIAN_TEST_CASE" in
  nothing) exit 0 ;;
  broken) echo "checkov: error: no such framework" >&2; exit 2 ;;
  *) cat "$here/report.json"; exit 1 ;;
esac
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("analyzer")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("stage")).unwrap();
    fs::write(dir.join("report.json"), REPORT).unwrap();
    let checkov = dir.join("checkov");
    fs::write(&checkov, CHECKOV).unwrap();
    fs::set_permissions(&checkov, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn input(dir: &Path, target: &str, settings: &str) -> String {
    format!(
        r#"{{"run":{{"id":"r","mode":"plan"}},"stage":{{"name":"site","kind":"Infrastructure","target":"{target}","category":"infrastructure-as-code","host":"","labels":{{}}}},"artifact":{{"dir":"{}","files":["main.tf.json"]}},"settings":{{{settings}}}}}"#,
        dir.join("stage").display()
    )
}

fn analyzer(dir: &Path, case: &str, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian-analyze-checkov"))
        .current_dir(dir.join("stage"))
        .env("MERIDIAN_CHECKOV", dir.join("checkov"))
        .env("MERIDIAN_TEST_CASE", case)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_failed_check_is_a_finding_at_the_default_severity_with_its_resource() {
    let dir = scratch("findings");
    let output = analyzer(&dir, "report", &input(&dir, "terraform", ""));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        answer.trim(),
        r#"{"findings":[{"file":"main.tf.json","line":3,"message":"Ensure the S3 bucket has access logging enabled","resource":"logs","rule":"CKV_AWS_18","severity":"high","url":"https://docs.prismacloud.io/en/enterprise-edition/policy-reference/aws-policies/s3-policies/s3-13-enable-logging"},{"file":"main.tf.json","line":3,"message":"Ensure all data stored in the S3 bucket have versioning enabled","resource":"logs","rule":"CKV_AWS_21","severity":"low","url":""}],"measures":[]}"#
    );
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert_eq!(
        args.trim(),
        "-d . -o json --quiet --compact --soft-fail --framework terraform_json"
    );
}

#[test]
fn settings_choose_the_frameworks_the_checks_and_the_default_severity() {
    let dir = scratch("settings");
    let output = analyzer(
        &dir,
        "report",
        &input(
            &dir,
            "pulumi",
            r#""framework":"terraform,kubernetes","skip-check":"CKV_AWS_21","default-severity":"medium","args":"--download-external-modules false""#,
        ),
    );
    assert!(output.status.success());
    let answer = String::from_utf8(output.stdout).unwrap();
    assert!(
        answer.contains(r#""rule":"CKV_AWS_18","severity":"medium""#),
        "{answer}"
    );
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert_eq!(
        args.trim(),
        "-d . -o json --quiet --compact --soft-fail --framework terraform kubernetes --skip-check CKV_AWS_21 --download-external-modules false"
    );

    // The environment carries a setting too.
    let output = Command::new(env!("CARGO_BIN_EXE_meridian-analyze-checkov"))
        .current_dir(dir.join("stage"))
        .env("MERIDIAN_CHECKOV", dir.join("checkov"))
        .env("MERIDIAN_TEST_CASE", "report")
        .env("MERIDIAN_ANALYZE_SKIP_CHECK", "CKV_AWS_18")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input(&dir, "terraform", "").as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    assert!(fs::read_to_string(dir.join("args"))
        .unwrap()
        .contains("--skip-check CKV_AWS_18"));
}

#[test]
fn nothing_scanned_is_no_finding_and_a_broken_checkov_is_an_error() {
    let dir = scratch("nothing");
    let output = analyzer(&dir, "nothing", &input(&dir, "salt", ""));
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        r#"{"findings":[],"measures":[]}"#
    );
    // No framework is chosen for a target Checkov has none for.
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(!args.contains("--framework"), "{args}");

    let output = analyzer(&dir, "broken", &input(&dir, "terraform", ""));
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains("checkov exited with status 2: checkov: error: no such framework"),
        "{said}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_meridian-analyze-checkov"))
        .env("MERIDIAN_CHECKOV", "/nonexistent/checkov")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input(&dir, "terraform", "").as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("name its program in MERIDIAN_CHECKOV")
    );
}

#[test]
fn a_report_of_several_frameworks_is_a_list_and_reads_the_same() {
    let input = Input::parse(input(Path::new("/tmp/x"), "terraform", "").as_bytes()).unwrap();
    let one = translate(REPORT.as_bytes(), &input).unwrap();
    let many = translate(format!("[{REPORT},{REPORT}]").as_bytes(), &input).unwrap();
    assert_eq!(one.findings.len(), 2);
    assert_eq!(many.findings.len(), 4);
    assert_eq!(one.findings[0].rule, "CKV_AWS_18");
    assert_eq!(one.findings[0].resource, "logs");
    assert!(translate(b"not json", &input).is_err());
    assert_eq!(frameworks("helm"), ["helm", "kubernetes"]);
    assert!(frameworks("chef").is_empty());
    assert!(arguments(&input).contains(&"--soft-fail".to_owned()));
}
