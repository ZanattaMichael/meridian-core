//! The running half, against a stand-in for the tool: a shell script that
//! records how it was called and prints what the real tool would. The suite
//! therefore needs nothing installed but Rust.

use std::fs;
use std::path::Path;

use meridian_plugin_template::TemplateRunner;
use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("manifest.tsv", "present\t/etc/motd\t0644\tcontent/motd\n"),
        ("content/motd", "Managed by Meridian.\n"),
    ])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "site".into(),
        host: "web01".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that appends each call to `calls` and then runs `body`.
fn runner(dir: &Path, body: &str) -> TemplateRunner {
    let program = stand_in(
        dir,
        "filesync",
        &format!("echo \"$@\" >> \"{}/calls\"\n{body}", dir.display()),
    );
    TemplateRunner {
        program: program.to_str().unwrap().to_owned(),
    }
}

#[test]
fn a_plan_runs_the_tool_on_the_written_artifact_and_reads_its_report() {
    let dir = scratch(TMP, "plan");
    let runner = runner(
        &dir,
        "test -f content/motd || exit 9\necho 'create /etc/motd'\necho 'delete /etc/issue.net'",
    );
    let log = KeptLog::default();

    let plan = runner.plan(&context(&dir), &compiled(), &log).unwrap();

    assert_eq!(
        fs::read_to_string(dir.join("calls")).unwrap(),
        "plan manifest.tsv\n"
    );
    assert_eq!(plan.changes.len(), 2);
    assert_eq!(plan.changes[0].action, ChangeAction::Create);
    assert_eq!(plan.changes[0].resource, "/etc/motd");
    assert_eq!(plan.changes[1].action, ChangeAction::Delete);
    assert!(!plan.in_sync());
    // What the tool printed reached the operator as it appeared.
    assert_eq!(log.text(), ["create /etc/motd", "delete /etc/issue.net"]);
}

#[test]
fn a_tool_that_prints_nothing_is_in_sync() {
    let dir = scratch(TMP, "in-sync");
    let plan = runner(&dir, "")
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync());
}

#[test]
fn an_apply_reports_what_the_tool_did() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, "echo 'update /etc/motd'")
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("calls")).unwrap(),
        "apply manifest.tsv\n"
    );
    assert_eq!(applied.changes[0].action, ChangeAction::Update);
    assert!(applied.outputs.is_empty());
}

#[test]
fn a_failing_tool_is_an_error_that_says_what_was_run() {
    let dir = scratch(TMP, "fails");
    let err = runner(&dir, "echo 'permission denied' >&2\nexit 3")
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "filesync apply failed");
    assert_eq!(err.exit_code, Some(3));
    assert!(
        // The program is shown by its full path, which on Windows is quoted
        // and ends in .exe.
        err.command.contains("filesync") && err.command.ends_with(" apply manifest.tsv"),
        "{}",
        err.command
    );
    assert!(err.output.contains("permission denied"), "{}", err.output);
}

#[test]
fn a_report_the_plugin_cannot_read_is_a_failure_not_no_change() {
    let dir = scratch(TMP, "unreadable");
    let err = runner(&dir, "echo 'something else happened'")
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(err.msg.contains("does not understand"), "{}", err.msg);
}
