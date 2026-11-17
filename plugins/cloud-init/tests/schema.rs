//! Every user-data this plugin's tests record, checked by cloud-init itself.
//!
//! The other tests compare bytes with bytes. This one asks the tool that
//! will read them. It needs a real `cloud-init`, which most machines that
//! build Meridian do not have, so without one it says so and passes; the
//! `cloud-init` workflow installs one and sets
//! `MERIDIAN_TEST_REAL_CLOUD_INIT`, which turns a missing tool into a
//! failure.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_testkit::{artifact, scratch, KeptLog};
use meridian_sdk::{RunContext, Runner};
use meridian_target_cloud_init::{CloudInitRunner, PROGRAM_VARIABLE};

const REQUIRED_VARIABLE: &str = "MERIDIAN_TEST_REAL_CLOUD_INIT";

/// The real cloud-init to test with, if this machine has one.
fn cloud_init() -> Option<String> {
    let program = std::env::var(PROGRAM_VARIABLE).unwrap_or_else(|_| "cloud-init".to_owned());
    let found = Command::new(&program)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if found {
        return Some(program);
    }
    assert!(
        std::env::var_os(REQUIRED_VARIABLE).is_none(),
        "{REQUIRED_VARIABLE} is set, and {program} could not be run"
    );
    eprintln!("skipped: {program} is not installed on this machine");
    None
}

fn recorded_user_data() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "user-data"))
        .collect();
    found.sort();
    assert!(found.len() >= 5, "{found:?}");
    found
}

fn schema(program: &str, file: &Path) -> std::process::Output {
    Command::new(program)
        .args(["schema", "--config-file"])
        .arg(file)
        .output()
        .expect("cloud-init runs")
}

#[test]
fn cloud_init_accepts_every_recorded_user_data() {
    let Some(program) = cloud_init() else { return };
    for file in recorded_user_data() {
        let out = schema(&program, &file);
        assert!(
            out.status.success(),
            "cloud-init rejected {}:\n{}{}",
            file.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The control: if cloud-init accepted anything at all, the test above would
/// prove nothing.
#[test]
fn cloud_init_rejects_user_data_that_is_wrong() {
    let Some(program) = cloud_init() else { return };
    let dir = scratch(env!("CARGO_TARGET_TMPDIR"), "schema-control");
    let file = dir.join("user-data");
    fs::write(&file, "#cloud-config\npackages: 4\nwrite_files: nothing\n").unwrap();
    let out = schema(&program, &file);
    assert!(
        !out.status.success(),
        "cloud-init accepted user-data whose packages is a number:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn the_runner_checks_with_the_real_tool() {
    let Some(program) = cloud_init() else { return };
    let dir = scratch(env!("CARGO_TARGET_TMPDIR"), "schema-runner");
    let runner = CloudInitRunner {
        program,
        validate: true,
        wait: None,
        misconfigured: None,
    };
    let context = RunContext {
        document: "everything".into(),
        host: "192.0.2.10".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };
    let recorded = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let user_data = fs::read_to_string(recorded.join("all_features.user-data")).unwrap();
    let meta_data = fs::read_to_string(recorded.join("all_features.meta-data")).unwrap();

    let good = artifact(&[("user-data", &user_data), ("meta-data", &meta_data)]);
    let applied = runner.apply(&context, &good, &KeptLog::default()).unwrap();
    assert_eq!(applied.changes.len(), 1);

    let bad = artifact(&[
        ("user-data", "#cloud-config\npackages: 4\n"),
        ("meta-data", &meta_data),
    ]);
    let err = runner
        .plan(&context, &bad, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "cloud-init rejected the user-data");
    // The seed still holds what was accepted.
    assert_eq!(
        fs::read_to_string(dir.join("work/seed/user-data")).unwrap(),
        user_data
    );
}
