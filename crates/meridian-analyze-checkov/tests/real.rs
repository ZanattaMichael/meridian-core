//! The analyzer against a real `checkov`, over a Terraform file Meridian
//! could have compiled. It does nothing unless `MERIDIAN_TEST_REAL_CHECKOV`
//! names the program; the `analyzers` workflow runs it.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use meridian_value::{decode_value, Value};

/// A bucket as the Terraform plugin emits one, with nothing Checkov likes.
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
#[ignore = "needs a real checkov; set MERIDIAN_TEST_REAL_CHECKOV to its program"]
fn a_real_checkov_reports_the_bucket_against_the_resource() {
    let Some(checkov) = std::env::var("MERIDIAN_TEST_REAL_CHECKOV")
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian-analyze-checkov"))
        .current_dir(&dir)
        .env("MERIDIAN_CHECKOV", checkov)
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
        "checkov found nothing in {MAIN_TF_JSON}"
    );
    let logging = findings
        .iter()
        .find(|f| f.as_map().unwrap()["rule"] == Value::from("CKV_AWS_18"))
        .expect("CKV_AWS_18, access logging, fires on a bare bucket");
    let logging = logging.as_map().unwrap();
    assert_eq!(logging["resource"], Value::from("logs"));
    assert_eq!(logging["file"], Value::from("main.tf.json"));
    assert_eq!(logging["severity"], Value::from("high"));
    assert!(matches!(logging["line"], Value::Uint(n) if n > 0));
}
