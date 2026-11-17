//! The running half, against a stand-in for `ansible-playbook`, so it is
//! tested on a machine with no Ansible installed.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner};
use meridian_target_ansible::AnsibleRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("playbook.yml", "---\n- name: web\n"),
        ("inventory.ini", "[meridian]\nweb01\n"),
    ])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web".into(),
        host: "web01".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that records its arguments and prints a canned run.
fn runner(dir: &Path, body: &str) -> AnsibleRunner {
    let program = stand_in(
        dir,
        "ansible-playbook",
        &format!(
            "echo \"$@\" > \"{}/args\"\npwd > \"{}/cwd\"\n{body}",
            dir.display(),
            dir.display()
        ),
    );
    AnsibleRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: vec!["--user".into(), "deploy".into()],
    }
}

const TWO_CHANGES: &str = "cat <<'OUT'
PLAY [web] *******

TASK [Gathering Facts] *******
ok: [web01]

TASK [nginx_pkg] *******
changed: [web01]

TASK [nginx_svc] *******
ok: [web01]

RUNNING HANDLER [restart nginx_svc] *******
changed: [web01]

PLAY RECAP *******
web01 : ok=4 changed=2 unreachable=0 failed=0
OUT";

#[test]
fn a_plan_is_a_check_run_and_reports_what_would_change() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, TWO_CHANGES)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();

    assert_eq!(
        fs::read_to_string(dir.join("args")).unwrap().trim(),
        "--inventory inventory.ini --check --diff --user deploy playbook.yml"
    );
    // The tool runs in the work directory, where the artifact was written.
    let cwd = fs::read_to_string(dir.join("cwd")).unwrap();
    let work = Path::new(cwd.trim());
    assert_eq!(
        fs::read_to_string(work.join("playbook.yml")).unwrap(),
        "---\n- name: web\n"
    );

    let changes: Vec<_> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        changes,
        [
            "update nginx_pkg: would change",
            "update restart nginx_svc: would change"
        ]
    );
    assert!(!plan.unknown && !plan.in_sync());
    assert!(log.text().contains(&"TASK [nginx_pkg] *******".to_owned()));
}

#[test]
fn an_apply_runs_for_real_and_reports_what_changed() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, TWO_CHANGES)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("args")).unwrap().trim(),
        "--inventory inventory.ini --user deploy playbook.yml"
    );
    assert_eq!(applied.changes.len(), 2);
    assert_eq!(applied.changes[0].action, ChangeAction::Update);
    assert_eq!(applied.changes[0].summary, "changed");
    assert!(applied.outputs.is_empty());
}

#[test]
fn a_run_that_changes_nothing_is_in_sync() {
    let dir = scratch(TMP, "in-sync");
    let plan = runner(&dir, "echo 'TASK [nginx_pkg] ***'\necho 'ok: [web01]'")
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync());
}

#[test]
fn a_task_skipped_in_check_mode_makes_the_plan_unknown() {
    let dir = scratch(TMP, "skipped");
    let plan = runner(&dir, "echo 'TASK [reindex] ***'\necho 'skipping: [web01]'")
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.unknown);
    assert!(!plan.in_sync());
}

#[test]
fn a_failed_apply_reports_the_task_and_what_had_already_changed() {
    let dir = scratch(TMP, "failed");
    let body = "echo 'TASK [nginx_pkg] ***'
echo 'changed: [web01]'
echo 'TASK [nginx_svc] ***'
echo 'fatal: [web01]: FAILED! => {\"msg\": \"Could not find the requested service nginx\"}'
exit 2";
    let err = runner(&dir, body)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "ansible-playbook reported a failed task");
    assert_eq!(err.exit_code, Some(2));
    assert!(err
        .output
        .contains("Could not find the requested service nginx"));
    assert!(err
        .command
        .ends_with("--inventory inventory.ini --user deploy playbook.yml"));
    assert_eq!(err.changes.len(), 1);
    assert_eq!(err.changes[0].resource, "nginx_pkg");
}

#[test]
fn an_unreachable_host_is_named_as_that() {
    let dir = scratch(TMP, "unreachable");
    let err = runner(&dir, "echo 'fatal: [web01]: UNREACHABLE!'\nexit 4")
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "ansible-playbook could not reach the host");
}

#[test]
fn a_missing_program_and_a_foreign_artifact_are_plain_errors() {
    let dir = scratch(TMP, "missing");
    let absent = AnsibleRunner {
        program: "meridian-no-such-ansible".into(),
        extra_args: Vec::new(),
    };
    let err = absent
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(err.msg.contains("it is not installed"), "{}", err.msg);

    let err = absent
        .plan(
            &context(&dir),
            &artifact(&[("main.tf", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no playbook.yml; it was not compiled by this plugin"
    );
}
