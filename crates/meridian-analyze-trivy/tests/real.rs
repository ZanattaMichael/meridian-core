//! The analyzer against a real `trivy`, over a Terraform file Meridian
//! could have compiled. It does nothing unless `MERIDIAN_TEST_REAL_TRIVY`
//! names the program; the `analyzers` workflow runs it.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use meridian_value::{decode_value, Value};

/// A bucket as the Terraform plugin emits one, with nothing Trivy likes.
const MAIN_TF_JSON: &str = r#"{
  "resource": {
    "aws_s3_bucket": {
      "logs": {
        "bucket": "meridian-example-logs"
      }
    }
  }
}
"#;

#[test]
#[ignore = "needs a real trivy; set MERIDIAN_TEST_REAL_TRIVY to its program"]
fn a_real_trivy_reports_the_bucket_against_the_resource() {
    let Some(trivy) = std::env::var("MERIDIAN_TEST_REAL_TRIVY")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("real");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("main.tf.json"), MAIN_TF_JSON).unwrap();
    let input = format!(
        r#"{{"run":{{"id":"r","mode":"plan"}},"stage":{{"name":"site","kind":"Infrastructure","target":"terraform","category":"infrastructure-as-code","host":"","labels":{{}}}},"artifact":{{"dir":"{}","files":["main.tf.json"]}},"settings":{{}}}}"#,
        dir.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian-analyze-trivy"))
        .current_dir(&dir)
        .env("MERIDIAN_TRIVY", trivy)
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
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answer = decode_value(&output.stdout).unwrap();
    let findings = answer.as_map().unwrap()["findings"]
        .as_list()
        .unwrap()
        .to_vec();
    assert!(
        !findings.is_empty(),
        "trivy found nothing in {MAIN_TF_JSON}"
    );
    let bucket = findings
        .iter()
        .map(|f| f.as_map().unwrap())
        .find(|f| f["resource"] == Value::from("logs"))
        .expect("a finding names the bucket by the document's id");
    assert_eq!(bucket["file"], Value::from("main.tf.json"));
    assert!(matches!(bucket["line"], Value::Int(n) if n > 0));
    assert!(
        bucket["rule"].as_str().unwrap().contains("AWS-"),
        "unexpected rule {:?}",
        bucket["rule"]
    );
}
