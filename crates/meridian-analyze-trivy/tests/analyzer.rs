//! The analyzer, run as Meridian runs it, against a stand-in for `trivy`
//! that keeps how it was called and answers as `trivy config --format
//! json` does. `tests/real.rs` does the same against the real thing.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use meridian_analyze::Input;
use meridian_analyze_trivy::{arguments, translate};

/// What `trivy config --format json` prints for a bucket without
/// encryption, shortened to the fields the analyzer reads plus a few it
/// does not.
const REPORT: &str = r#"{
  "SchemaVersion": 2,
  "CreatedAt": "2026-11-18T12:00:00Z",
  "ArtifactName": ".",
  "ArtifactType": "filesystem",
  "Results": [
    {
      "Target": "main.tf.json",
      "Class": "config",
      "Type": "terraform",
      "MisconfSummary": {"Successes": 3, "Failures": 2},
      "Misconfigurations": [
        {
          "Type": "Terraform Security Check", "ID": "AVD-AWS-0088", "AVDID": "AVD-AWS-0088",
          "Title": "Unencrypted S3 bucket.",
          "Description": "S3 Buckets should be encrypted to protect the data that is stored within them if access is compromised.",
          "Message": "Bucket does not have encryption enabled",
          "Namespace": "builtin.aws.s3.aws0088", "Query": "data.builtin.aws.s3.aws0088.deny",
          "Resolution": "Configure bucket encryption.", "Severity": "HIGH",
          "PrimaryURL": "https://avd.aquasec.com/misconfig/avd-aws-0088",
          "Status": "FAIL",
          "CauseMetadata": {"Resource": "aws_s3_bucket.logs", "Provider": "AWS", "Service": "s3", "StartLine": 4, "EndLine": 6}
        },
        {
          "Type": "Terraform Security Check", "ID": "AVD-AWS-0089", "Title": "S3 Bucket Logging",
          "Message": "Bucket has logging disabled", "Severity": "LOW", "Status": "FAIL",
          "PrimaryURL": "https://avd.aquasec.com/misconfig/avd-aws-0089",
          "CauseMetadata": {"Resource": "aws_s3_bucket.logs", "StartLine": 4, "EndLine": 6}
        },
        {
          "Type": "Terraform Security Check", "ID": "AVD-AWS-0090", "Title": "S3 Data is Versioned",
          "Message": "Bucket does not have versioning enabled", "Severity": "MEDIUM", "Status": "PASS",
          "CauseMetadata": {"Resource": "aws_s3_bucket.logs"}
        }
      ]
    }
  ]
}"#;

const TRIVY: &str = r#"#!/bin/sh
here=$(dirname "$0")
printf '%s\n' "$*" > "$here/args"
case "$MERIDIAN_TEST_CASE" in
  nothing) echo '{"SchemaVersion": 2, "Results": null}' ;;
  broken) echo "FATAL image scan error" >&2; exit 1 ;;
  *) cat "$here/report.json" ;;
esac
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("analyzer")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("stage")).unwrap();
    fs::write(dir.join("report.json"), REPORT).unwrap();
    let trivy = dir.join("trivy");
    fs::write(&trivy, TRIVY).unwrap();
    fs::set_permissions(&trivy, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn input(dir: &Path, settings: &str) -> String {
    format!(
        r#"{{"run":{{"id":"r","mode":"plan"}},"stage":{{"name":"site","kind":"Infrastructure","target":"terraform","category":"infrastructure-as-code","host":"","labels":{{}}}},"artifact":{{"dir":"{}","files":["main.tf.json"]}},"settings":{{{settings}}}}}"#,
        dir.join("stage").display()
    )
}

fn analyzer(dir: &Path, case: &str, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian-analyze-trivy"))
        .current_dir(dir.join("stage"))
        .env("MERIDIAN_TRIVY", dir.join("trivy"))
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
fn a_failed_misconfiguration_is_a_finding_with_trivys_severity_and_the_resource() {
    let dir = scratch("findings");
    let output = analyzer(&dir, "report", &input(&dir, ""));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        answer.trim(),
        r#"{"findings":[{"file":"main.tf.json","line":4,"message":"Unencrypted S3 bucket.: Bucket does not have encryption enabled","resource":"logs","rule":"AVD-AWS-0088","severity":"high","url":"https://avd.aquasec.com/misconfig/avd-aws-0088"},{"file":"main.tf.json","line":4,"message":"S3 Bucket Logging: Bucket has logging disabled","resource":"logs","rule":"AVD-AWS-0089","severity":"low","url":"https://avd.aquasec.com/misconfig/avd-aws-0089"}],"measures":[]}"#
    );
    assert_eq!(
        fs::read_to_string(dir.join("args")).unwrap().trim(),
        "config --format json --quiet ."
    );
}

#[test]
fn settings_filter_severities_skip_checks_and_add_arguments() {
    let dir = scratch("settings");
    let output = analyzer(
        &dir,
        "report",
        &input(
            &dir,
            r#""severity":"HIGH,CRITICAL","skip-check":"AVD-AWS-0089","args":"--tf-vars vars.tfvars""#,
        ),
    );
    assert!(output.status.success());
    let answer = String::from_utf8(output.stdout).unwrap();
    assert!(
        answer.contains("AVD-AWS-0088") && !answer.contains("AVD-AWS-0089"),
        "{answer}"
    );
    assert_eq!(
        fs::read_to_string(dir.join("args")).unwrap().trim(),
        "config --format json --quiet --severity HIGH,CRITICAL --tf-vars vars.tfvars ."
    );
}

#[test]
fn nothing_found_is_no_finding_and_a_broken_trivy_is_an_error() {
    let dir = scratch("nothing");
    let output = analyzer(&dir, "nothing", &input(&dir, ""));
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        r#"{"findings":[],"measures":[]}"#
    );
    let output = analyzer(&dir, "broken", &input(&dir, ""));
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains("trivy exited with status 1: FATAL image scan error"),
        "{said}"
    );
}

#[test]
fn the_translation_reads_only_failures() {
    let input = Input::parse(input(Path::new("/tmp/x"), "").as_bytes()).unwrap();
    let answer = translate(REPORT.as_bytes(), &input).unwrap();
    assert_eq!(
        answer.findings.len(),
        2,
        "the passed check is not a finding"
    );
    assert_eq!(answer.findings[0].severity, "high");
    assert_eq!(answer.findings[0].resource, "logs");
    assert!(translate(b"{", &input).is_err());
    assert!(translate(b"", &input).unwrap().findings.is_empty());
    assert_eq!(arguments(&input).last().unwrap(), ".");
}
