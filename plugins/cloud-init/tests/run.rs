//! The running half, against stand-ins for `cloud-init` and `ssh`, so it is
//! tested on a machine with neither a cloud-init to check with nor a machine
//! to wait for.

use std::fs;
use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner, Value};
use meridian_target_cloud_init::{CloudInitRunner, Wait};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled(user_data: &str) -> Artifact {
    artifact(&[
        ("user-data", user_data),
        ("meta-data", "instance-id: meridian-web\n"),
    ])
}

const USER_DATA: &str = "#cloud-config\npackages:\n  - nginx\n";

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web".into(),
        host: "web01.example.com".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that records its arguments and directory under `name`, then
/// runs `body`.
fn recording(dir: &Path, name: &str, body: &str) -> String {
    let program = stand_in(
        dir,
        name,
        &format!(
            "echo \"$@\" >> \"{0}/{name}.args\"\npwd > \"{0}/{name}.cwd\"\n{body}",
            dir.display()
        ),
    );
    program.to_str().unwrap().to_owned()
}

/// A runner whose cloud-init accepts everything, or does what `body` says.
fn runner(dir: &Path, body: &str) -> CloudInitRunner {
    CloudInitRunner {
        program: recording(dir, "cloud-init", body),
        validate: true,
        wait: None,
        misconfigured: None,
    }
}

fn waiting(dir: &Path, ssh_body: &str) -> CloudInitRunner {
    CloudInitRunner {
        wait: Some(Wait {
            program: recording(dir, "ssh", ssh_body),
            args: vec!["-l".into(), "ubuntu".into()],
        }),
        ..runner(dir, "echo 'Valid schema user-data'")
    }
}

fn seed(dir: &Path) -> PathBuf {
    // The stand-in reports the directory by its real path, so the seed is
    // compared the same way.
    dir.join("work").join("seed")
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn a_plan_checks_the_user_data_and_writes_no_seed() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, "echo 'Valid schema user-data'")
        .plan(&context(&dir), &compiled(USER_DATA), &log)
        .unwrap();

    assert_eq!(
        read(dir.join("cloud-init.args")).trim(),
        "schema --config-file user-data"
    );
    // cloud-init is run where the candidate was written, and that is not
    // the seed.
    let cwd = read(dir.join("cloud-init.cwd"));
    let cwd = Path::new(cwd.trim());
    assert!(cwd.ends_with("work/check"), "{}", cwd.display());
    assert_eq!(read(cwd.join("user-data")), USER_DATA);
    assert!(!seed(&dir).exists());

    let changes: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        changes,
        [format!(
            "create user-data: seed would be written to {}; a machine applies it on its first boot",
            seed(&dir).display()
        )]
    );
    assert!(!plan.unknown && !plan.in_sync());
    assert_eq!(log.text(), ["Valid schema user-data"]);
}

#[test]
fn an_apply_writes_the_seed_and_says_where_it_is() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, "")
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap();

    assert_eq!(read(seed(&dir).join("user-data")), USER_DATA);
    assert_eq!(
        read(seed(&dir).join("meta-data")),
        "instance-id: meridian-web\n"
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].action, ChangeAction::Create);
    assert_eq!(applied.changes[0].resource, "user-data");
    assert!(
        applied.changes[0].summary.starts_with("seed written to "),
        "{}",
        applied.changes[0].summary
    );

    let outputs: Vec<(&str, &Value, bool)> = applied
        .outputs
        .iter()
        .map(|o| (o.name.as_str(), &o.value, o.sensitive))
        .collect();
    let path = |p: PathBuf| Value::String(p.to_string_lossy().into_owned());
    assert_eq!(
        outputs,
        [
            ("seed_dir", &path(seed(&dir)), false),
            ("user_data_file", &path(seed(&dir).join("user-data")), false),
        ]
    );
}

#[test]
fn a_seed_that_is_current_is_in_sync_and_one_that_is_not_is_replaced() {
    let dir = scratch(TMP, "in-sync");
    let runner = runner(&dir, "");
    let log = KeptLog::default();
    runner
        .apply(&context(&dir), &compiled(USER_DATA), &log)
        .unwrap();

    // The same document again: nothing to do, and it is still checked.
    let plan = runner
        .plan(&context(&dir), &compiled(USER_DATA), &log)
        .unwrap();
    assert!(plan.in_sync());
    let again = runner
        .apply(&context(&dir), &compiled(USER_DATA), &log)
        .unwrap();
    assert!(again.changes.is_empty());
    assert_eq!(again.outputs.len(), 2);
    assert_eq!(read(dir.join("cloud-init.args")).lines().count(), 3);

    // A changed document replaces the seed, and a plan says so first.
    let changed = compiled("#cloud-config\npackages:\n  - nginx\n  - curl\n");
    let plan = runner.plan(&context(&dir), &changed, &log).unwrap();
    assert_eq!(plan.changes[0].action, ChangeAction::Update);
    assert!(
        plan.changes[0]
            .summary
            .starts_with("seed would be replaced in "),
        "{}",
        plan.changes[0].summary
    );
    assert_eq!(read(seed(&dir).join("user-data")), USER_DATA);

    let applied = runner.apply(&context(&dir), &changed, &log).unwrap();
    assert_eq!(applied.changes[0].action, ChangeAction::Update);
    assert!(read(seed(&dir).join("user-data")).contains("curl"));

    // The meta-data alone changing counts too.
    let renamed = artifact(&[
        (
            "user-data",
            "#cloud-config\npackages:\n  - nginx\n  - curl\n",
        ),
        ("meta-data", "instance-id: meridian-other\n"),
    ]);
    let plan = runner.plan(&context(&dir), &renamed, &log).unwrap();
    assert_eq!(plan.changes.len(), 1);
}

#[test]
fn user_data_cloud_init_rejects_never_becomes_the_seed() {
    let dir = scratch(TMP, "rejected");
    let body =
        "echo 'Error: Cloud config schema errors: packages: 4 is not of type array' >&2\nexit 1";
    let runner = runner(&dir, body);
    for apply in [false, true] {
        let err = if apply {
            runner
                .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
                .unwrap_err()
        } else {
            runner
                .plan(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
                .unwrap_err()
        };
        assert_eq!(err.msg, "cloud-init rejected the user-data");
        assert_eq!(err.exit_code, Some(1));
        assert!(
            err.output.contains("is not of type array"),
            "{}",
            err.output
        );
        assert!(
            err.command.ends_with("schema --config-file user-data"),
            "{}",
            err.command
        );
        assert!(err.changes.is_empty());
    }
    assert!(!seed(&dir).exists());
}

#[test]
fn a_missing_cloud_init_says_how_to_go_on_and_the_check_can_be_turned_off() {
    let dir = scratch(TMP, "missing");
    let mut runner = CloudInitRunner {
        program: "meridian-no-such-cloud-init".into(),
        validate: true,
        wait: None,
        misconfigured: None,
    };
    let err = runner
        .plan(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg
            .starts_with("could not run meridian-no-such-cloud-init: it is not installed"),
        "{}",
        err.msg
    );
    assert!(
        err.msg.ends_with(
            "install it, name it with MERIDIAN_CLOUD_INIT, or set \
             MERIDIAN_CLOUD_INIT_VALIDATE=off to go without the check"
        ),
        "{}",
        err.msg
    );

    runner.validate = false;
    let applied = runner
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(read(seed(&dir).join("user-data")), USER_DATA);
}

#[test]
fn a_foreign_artifact_and_a_meaningless_setting_are_plain_errors() {
    let dir = scratch(TMP, "foreign");
    let mut runner = runner(&dir, "");
    let err = runner
        .plan(
            &context(&dir),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no user-data; it was not compiled by this plugin"
    );

    runner.misconfigured = Some("MERIDIAN_CLOUD_INIT_WAIT is \"telnet\"".into());
    let err = runner
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "MERIDIAN_CLOUD_INIT_WAIT is \"telnet\"");
    assert!(!dir.join("cloud-init.args").exists());
}

#[test]
fn outputs_are_read_from_the_seed_without_applying() {
    let dir = scratch(TMP, "outputs");
    let runner = runner(&dir, "");
    let err = runner.outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "no seed has been written yet; apply the document first"
    );

    let applied = runner
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap();
    assert_eq!(runner.outputs(&context(&dir)).unwrap(), applied.outputs);
}

const BOOT_DONE: &str = "cat <<'OUT'
status: done
extended_status: done
boot_status_code: enabled-by-generator
detail: DataSourceNoCloud [seed=/dev/sr0]
errors: []
OUT";

#[test]
fn an_apply_can_wait_for_the_first_boot_to_finish() {
    let dir = scratch(TMP, "wait");
    let log = KeptLog::default();
    let applied = waiting(&dir, BOOT_DONE)
        .apply(&context(&dir), &compiled(USER_DATA), &log)
        .unwrap();

    assert_eq!(
        read(dir.join("ssh.args")).trim(),
        "-o BatchMode=yes -l ubuntu web01.example.com cloud-init status --wait --long"
    );
    assert_eq!(applied.changes.len(), 1);
    assert!(log.text().contains(&"status: done".to_owned()));
}

#[test]
fn a_plan_never_waits() {
    let dir = scratch(TMP, "plan-no-wait");
    waiting(&dir, BOOT_DONE)
        .plan(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap();
    assert!(!dir.join("ssh.args").exists());
}

#[test]
fn a_failed_first_boot_fails_the_apply_and_keeps_what_was_done() {
    let dir = scratch(TMP, "boot-failed");
    let body = "echo 'status: error'\necho 'errors:'\necho \"  - ('scripts_user', RuntimeError('Runparts: 1 failures'))\"\nexit 1";
    let err = waiting(&dir, body)
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "cloud-init on web01.example.com reports that the first boot failed"
    );
    assert_eq!(err.exit_code, Some(1));
    assert!(
        err.output.contains("Runparts: 1 failures"),
        "{}",
        err.output
    );
    assert!(
        err.command
            .ends_with("web01.example.com cloud-init status --wait --long"),
        "{}",
        err.command
    );
    // The seed was written before the wait began.
    assert_eq!(err.changes.len(), 1);
    assert_eq!(err.changes[0].action, ChangeAction::Create);
}

#[test]
fn a_boot_that_finished_with_recoverable_errors_passes_and_says_so() {
    let dir = scratch(TMP, "boot-degraded");
    let log = KeptLog::default();
    waiting(
        &dir,
        "echo 'status: done'\necho 'extended_status: degraded done'\nexit 2",
    )
    .apply(&context(&dir), &compiled(USER_DATA), &log)
    .unwrap();
    assert_eq!(
        log.text().last().unwrap(),
        "cloud-init on web01.example.com finished the first boot with recoverable errors"
    );
}

#[test]
fn a_host_that_cannot_be_reached_or_was_never_named_is_said_plainly() {
    let dir = scratch(TMP, "unreachable");
    let runner = waiting(
        &dir,
        "echo 'ssh: connect to host web01.example.com port 22: Connection refused' >&2\nexit 255",
    );
    let err = runner
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "could not reach web01.example.com over ssh");
    assert!(err.output.contains("Connection refused"));

    let err = waiting(&dir, "exit 7")
        .apply(&context(&dir), &compiled(USER_DATA), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "waiting for cloud-init on web01.example.com failed"
    );

    let nowhere = RunContext {
        host: String::new(),
        ..context(&dir)
    };
    let err = runner
        .apply(&nowhere, &compiled(USER_DATA), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "MERIDIAN_CLOUD_INIT_WAIT=ssh needs a host to wait for, and this document resolved none"
    );
}
