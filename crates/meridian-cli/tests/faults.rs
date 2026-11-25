//! `meridian lab fault`, run as a process against the fake target: a proven
//! fault passes and destroys its environment, one that does not surface
//! fails the proof, and a wrong expectation names what the run reported.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SITE: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: site
spec:
  target: fake
  resources:
    - id: motd
      type: file
      params:
        content: "${{ data.text }}"
"#;

const TEST: &str = r#"apiVersion: meridian/v1
kind: Test
metadata:
  name: env
spec:
  documents: [site.yaml]
  data: [site.data.yaml]
  cases:
    - name: fresh
      run: lifecycle
"#;

fn scratch(test: &str, inject: &str, cause: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("faults")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    fs::write(dir.join("site.test.yaml"), TEST).unwrap();
    fs::write(dir.join("site.data.yaml"), "text: welcome\n").unwrap();
    let fault = format!(
        "\
apiVersion: meridian/v1
kind: Fault
metadata:
  name: content-turns-fail
spec:
  applies: {{ category: infrastructure }}
  inject:
    command: [{inject}]
  expect:
    run: failed
    cause: {cause}
    resource: data.json
    diagnose: {{ first: data.json }}
"
    );
    fs::write(dir.join("content-turns-fail.fault.yaml"), fault).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env(
            "MERIDIAN_PLUGIN_DIR",
            meridian_target_fake::plugin_dir().display().to_string(),
        )
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_RUN_ID")
        .stdin(Stdio::null())
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_proven_fault_passes_and_leaves_nothing() {
    let dir_str = scratch_dir("proven").display().to_string();
    let dir = scratch(
        "proven",
        &format!("sh, -c, \"printf 'text: FAIL\\\\n' > {dir_str}/site.data.yaml\""),
        "resource-failed",
    );
    let out = meridian(
        &dir,
        &[
            "lab",
            "fault",
            "content-turns-fail.fault.yaml",
            "--environment",
            "site.test.yaml",
        ],
    );
    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("check"), "{shown}");
    assert!(!shown.contains("FAIL "), "{shown}");
    // The proof destroyed its environment: the case's state is gone.
    assert!(
        !dir.join(".meridian/faults/content-turns-fail/env/fresh")
            .exists(),
        "{shown}"
    );
}

#[test]
fn a_fault_that_does_not_surface_fails_the_proof() {
    let dir = scratch("hidden", "sh, -c, \"true\"", "resource-failed");
    let out = meridian(
        &dir,
        &[
            "lab",
            "fault",
            "content-turns-fail.fault.yaml",
            "--environment",
            "site.test.yaml",
        ],
    );
    let shown = text(&out);
    assert!(!out.status.success(), "{shown}");
    assert!(shown.contains("the fault did not surface"), "{shown}");
}

#[test]
fn a_wrong_expectation_names_what_the_run_reported() {
    let dir_str = scratch_dir("wrong-cause").display().to_string();
    let dir = scratch(
        "wrong-cause",
        &format!("sh, -c, \"printf 'text: FAIL\\\\n' > {dir_str}/site.data.yaml\""),
        "unavailable",
    );
    let out = meridian(
        &dir,
        &[
            "lab",
            "fault",
            "content-turns-fail.fault.yaml",
            "--environment",
            "site.test.yaml",
        ],
    );
    let shown = text(&out);
    assert!(!out.status.success(), "{shown}");
    assert!(shown.contains("wanted cause"), "{shown}");
    assert!(shown.contains("resource-failed"), "{shown}");
}

// The scratch helper names the data file the fault edits with an absolute
// path: the injection command runs against the proof's state directory, and
// the data file lives beside the documents.
fn scratch_dir(test: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("faults")
        .join(test)
}
