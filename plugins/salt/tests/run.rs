//! The running half, against stand-ins for `salt-call` and `salt-ssh`, so it
//! is tested on a machine with no Salt installed. The canned results are the
//! JSON a real Salt 3008 printed for the same states.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner};
use meridian_target_salt::{SaltRunner, Transport};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("states/web.sls", "#!yaml\n"),
        ("states/top.sls", "#!yaml\n"),
    ])
}

fn context(dir: &Path, host: &str) -> RunContext {
    RunContext {
        document: "web".into(),
        host: host.into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A runner whose two programs are stand-ins that record their arguments and
/// print a canned result.
fn runner(dir: &Path, body: &str) -> SaltRunner {
    let script = format!(
        "echo \"$@\" > \"{}/args\"\npwd > \"{}/cwd\"\n{body}",
        dir.display(),
        dir.display()
    );
    SaltRunner {
        salt_call: stand_in(dir, "salt-call", &script)
            .to_str()
            .unwrap()
            .to_owned(),
        salt_ssh: stand_in(dir, "salt-ssh", &script)
            .to_str()
            .unwrap()
            .to_owned(),
        transport: None,
        extra_args: vec!["--log-level".into(), "quiet".into()],
    }
}

fn args(dir: &Path) -> String {
    // Compared with forward slashes, so one expectation serves every system.
    fs::read_to_string(dir.join("args"))
        .unwrap()
        .trim()
        .replace('\\', "/")
}

/// The work directory as the runner hands it to the tool: absolute.
fn work(dir: &Path) -> String {
    meridian_sdk::tool::absolute_path(&dir.join("work"))
        .to_str()
        .unwrap()
        .replace('\\', "/")
}

fn print(json: &str) -> String {
    format!("cat <<'OUT'\n{json}\nOUT")
}

const DRY_RUN: &str = r#"{"local": {
  "pkg_|-nginx_pkg_|-nginx_|-installed": {"name": "nginx", "changes": {"nginx": {"new": "installed", "old": ""}},
    "result": null, "comment": "The following packages would be installed/updated: nginx",
    "__sls__": "web", "__run_num__": 0, "__id__": "nginx_pkg"},
  "service_|-nginx_svc_|-nginx_|-running": {"name": "nginx", "changes": {}, "result": null,
    "comment": "Service is set to be started", "__sls__": "web", "__run_num__": 2, "__id__": "nginx_svc"},
  "file_|-nginx_conf_|-/etc/nginx/nginx.conf_|-managed": {"name": "/etc/nginx/nginx.conf",
    "changes": {"newfile": "/etc/nginx/nginx.conf"}, "result": null,
    "comment": "The file /etc/nginx/nginx.conf is set to be changed\nNote: No changes made, actual changes may be different due to other states.",
    "__sls__": "web", "__run_num__": 1, "__id__": "nginx_conf"},
  "user_|-legacy_user_|-old_|-absent": {"name": "old", "changes": {}, "result": true,
    "comment": "User old is not present", "__sls__": "web", "__run_num__": 3, "__id__": "legacy_user"},
  "file_|-stale_|-/etc/old.conf_|-absent": {"name": "/etc/old.conf", "changes": {}, "result": null,
    "comment": "File /etc/old.conf is set for removal", "__sls__": "web", "__run_num__": 4, "__id__": "stale"}
}}"#;

const APPLIED: &str = r#"{"local": {
  "file_|-conf_|-/etc/app.conf_|-managed": {"name": "/etc/app.conf", "changes": {"diff": "New file", "mode": "0644"},
    "result": true, "comment": "File /etc/app.conf updated", "__run_num__": 0, "__id__": "conf"},
  "service_|-svc_|-app_|-running": {"name": "app", "changes": {"app": true}, "result": true,
    "comment": "Service restarted", "__run_num__": 1, "__id__": "svc"},
  "cmd_|-run job_|-/bin/reindex_|-run": {"name": "/bin/reindex", "changes": {}, "result": true,
    "comment": "State was not run because none of the onchanges reqs changed", "__run_num__": 3, "__id__": "run job"},
  "pkg_|-old_|-telnet_|-removed": {"name": "telnet", "changes": {"telnet": {"new": "", "old": "0.17"}}, "result": true,
    "comment": "All targeted packages were removed.", "__run_num__": 2, "__id__": "old"}
}}"#;

#[test]
fn a_plan_is_a_test_run_and_reports_what_would_change_in_run_order() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, &print(DRY_RUN))
        .plan(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap();

    let work = work(&dir);
    assert_eq!(
        args(&dir),
        format!(
            "--local --file-root {work}/states --out json --no-color --retcode-passthrough \
             --log-level quiet state.apply web test=True"
        )
    );
    // The tool runs in the work directory, where the artifact was written.
    assert_eq!(
        fs::read_to_string(dir.join("cwd"))
            .unwrap()
            .trim()
            .replace('\\', "/"),
        work
    );
    assert_eq!(
        fs::read_to_string(Path::new(&work).join("states/web.sls")).unwrap(),
        "#!yaml\n"
    );
    // Nothing of salt-ssh's is written for a local run.
    assert!(!Path::new(&work).join("ssh").exists());

    let changes: Vec<_> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        changes,
        [
            "update nginx_pkg: The following packages would be installed/updated: nginx",
            "update nginx_conf: The file /etc/nginx/nginx.conf is set to be changed",
            "update nginx_svc: Service is set to be started",
            "delete stale: File /etc/old.conf is set for removal",
        ]
    );
    assert!(!plan.unknown && !plan.in_sync());

    // The result is data, so what reaches the log is a line per state.
    assert_eq!(
        log.text(),
        [
            "nginx_pkg (pkg.installed): would change: The following packages would be installed/updated: nginx",
            "nginx_conf (file.managed): would change: The file /etc/nginx/nginx.conf is set to be changed",
            "nginx_svc (service.running): would change: Service is set to be started",
            "legacy_user (user.absent): ok: User old is not present",
            "stale (file.absent): would change: File /etc/old.conf is set for removal",
        ]
    );
}

#[test]
fn an_apply_runs_for_real_and_reports_what_changed() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, &print(APPLIED))
        .apply(&context(&dir, ""), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(args(&dir).ends_with("--log-level quiet state.apply web"));
    let changes: Vec<_> = applied.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        changes,
        [
            "update conf: File /etc/app.conf updated",
            "update svc: Service restarted",
            "delete old: All targeted packages were removed.",
        ]
    );
    assert_eq!(applied.changes[2].action, ChangeAction::Delete);
    assert!(applied.outputs.is_empty());
}

#[test]
fn a_run_that_changes_nothing_is_in_sync() {
    let dir = scratch(TMP, "in-sync");
    let body = print(
        r#"{"local": {"pkg_|-p_|-nginx_|-installed": {"changes": {}, "result": true,
            "comment": "All specified packages are already installed", "__run_num__": 0, "__id__": "p"}}}"#,
    );
    let plan = runner(&dir, &body)
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(plan.in_sync());

    // A document with no resources applies to an empty result.
    let plan = runner(&dir, &print(r#"{"local": {}}"#))
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(plan.in_sync());
}

#[test]
fn a_failed_apply_reports_the_state_and_what_had_already_changed() {
    let dir = scratch(TMP, "failed");
    let body = format!(
        "{}\necho '[ERROR   ] The named service app is not available' >&2\nexit 2",
        print(
            r#"{"local": {
  "file_|-conf_|-/etc/app.conf_|-managed": {"changes": {"diff": "New file"}, "result": true,
    "comment": "File /etc/app.conf updated", "__run_num__": 0, "__id__": "conf"},
  "service_|-svc_|-app_|-running": {"changes": {}, "result": false,
    "comment": "The named service app is not available", "__run_num__": 1, "__id__": "svc"},
  "cmd_|-job_|-/bin/reindex_|-run": {"changes": {}, "result": false,
    "comment": "One or more requisite failed: web.svc", "__run_num__": 2, "__id__": "job"}
}}"#
        )
    );
    let log = KeptLog::default();
    let err = runner(&dir, &body)
        .apply(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap_err();
    assert_eq!(err.msg, "salt-call reported 2 failed states");
    assert_eq!(err.exit_code, Some(2));
    assert_eq!(
        err.output,
        "svc: The named service app is not available\njob: One or more requisite failed: web.svc\n"
    );
    assert!(err.command.ends_with("state.apply web"), "{}", err.command);
    assert_eq!(err.changes.len(), 1);
    assert_eq!(err.changes[0].resource, "conf");
    // What the tool wrote to its error stream is relayed as it came.
    assert!(log
        .text()
        .contains(&"[ERROR   ] The named service app is not available".to_owned()));
    assert!(log.text().contains(
        &"svc (service.running): failed: The named service app is not available".to_owned()
    ));

    // A failed plan carries no changes: nothing was done.
    let err = runner(&dir, &body)
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(err.changes.is_empty());
}

#[test]
fn failed_states_are_a_failure_even_when_the_tool_exits_cleanly() {
    let dir = scratch(TMP, "failed-clean-exit");
    let body = print(
        r#"{"local": {"cmd_|-job_|-/bin/false_|-run": {"changes": {"retcode": 1}, "result": false,
            "comment": "Command \"/bin/false\" run", "__run_num__": 0, "__id__": "job"}}}"#,
    );
    let err = runner(&dir, &body)
        .apply(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "salt-call reported a failed state");
    assert_eq!(err.exit_code, Some(0));
}

#[test]
fn a_state_file_salt_cannot_render_is_named_as_that() {
    let dir = scratch(TMP, "render");
    let body = format!(
        "{}\nexit 1",
        print(
            r#"{"local": ["Rendering SLS 'base:web' failed: Jinja syntax error: unexpected '}'; line 9"]}"#
        )
    );
    let err = runner(&dir, &body)
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "salt-call could not compile the state file");
    assert_eq!(
        err.output,
        "Rendering SLS 'base:web' failed: Jinja syntax error: unexpected '}'; line 9\n"
    );
}

#[test]
fn another_host_is_reached_with_salt_ssh() {
    let dir = scratch(TMP, "ssh");
    let body = print(&DRY_RUN.replacen("\"local\"", "\"web01.example.com\"", 1));
    let plan = runner(&dir, &body)
        .plan(
            &context(&dir, "web01.example.com"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(plan.changes.len(), 4);

    let work = work(&dir);
    assert_eq!(
        args(&dir),
        format!(
            "--config-dir {work}/ssh --roster-file {work}/ssh/roster --out json --no-color \
             --log-level quiet web01.example.com state.apply web test=True"
        )
    );
    assert_eq!(
        fs::read_to_string(Path::new(&work).join("ssh/roster")).unwrap(),
        "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\
         \"web01.example.com\":\n  host: \"web01.example.com\"\n"
    );
    assert_eq!(
        // A Windows path is written with its backslashes escaped.
        fs::read_to_string(Path::new(&work).join("ssh/master"))
            .unwrap()
            .replace("\\\\", "/"),
        format!(
            "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\
             root_dir: \"{work}/ssh/root\"\nfile_roots:\n  base:\n    - \"{work}/states\"\n"
        )
    );
}

#[test]
fn a_host_salt_ssh_cannot_reach_is_named_as_that() {
    let dir = scratch(TMP, "ssh-unreachable");
    for answer in [
        r#"{"web01": "ssh: connect to host web01 port 22: Connection refused"}"#,
        r#"{"web01": {"stdout": "", "stderr": "Permission denied (publickey).", "retcode": 255}}"#,
    ] {
        let err = runner(&dir, &format!("{}\nexit 255", print(answer)))
            .apply(&context(&dir, "web01"), &compiled(), &KeptLog::default())
            .unwrap_err();
        assert_eq!(err.msg, "salt-ssh could not run on the host");
        assert!(
            err.output.contains("Connection refused") || err.output.contains("Permission denied"),
            "{}",
            err.output
        );
    }
}

#[test]
fn the_transport_can_be_told() {
    let dir = scratch(TMP, "told");
    let mut local = runner(&dir, &print(r#"{"local": {}}"#));
    local.transport = Some(Transport::Local);
    local
        .plan(
            &context(&dir, "web01.example.com"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(args(&dir).starts_with("--local "), "{}", args(&dir));

    let mut ssh = runner(&dir, &print(r#"{"localhost": {}}"#));
    ssh.transport = Some(Transport::Ssh);
    ssh.plan(
        &context(&dir, "localhost"),
        &compiled(),
        &KeptLog::default(),
    )
    .unwrap();
    assert!(args(&dir).starts_with("--config-dir "), "{}", args(&dir));

    // There is nothing to connect to without a host.
    let err = ssh
        .plan(&context(&dir, ""), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "salt-ssh needs a host to connect to, and the document names none"
    );
}

#[test]
fn a_tool_that_prints_no_result_fails_with_what_it_said() {
    let dir = scratch(TMP, "no-json");
    let err = runner(&dir, "echo 'salt-call: error: no such option' >&2\nexit 2")
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "salt-call failed");
    assert_eq!(err.exit_code, Some(2));
    assert!(err.output.contains("no such option"), "{}", err.output);
}

#[test]
fn a_missing_program_and_a_foreign_artifact_are_plain_errors() {
    let dir = scratch(TMP, "missing");
    let absent = SaltRunner {
        salt_call: "meridian-no-such-salt-call".into(),
        salt_ssh: "meridian-no-such-salt-ssh".into(),
        transport: None,
        extra_args: Vec::new(),
    };
    let err = absent
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(err.msg.contains("it is not installed"), "{}", err.msg);

    let err = absent
        .plan(
            &context(&dir, "localhost"),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no states/top.sls and one state file beside it; it was not compiled by this plugin"
    );
}

#[test]
fn a_state_removed_from_the_artifact_is_removed_from_the_file_root() {
    let dir = scratch(TMP, "stale");
    let runner = runner(&dir, &print(r#"{"local": {}}"#));
    let ctx = context(&dir, "localhost");
    runner.plan(&ctx, &compiled(), &KeptLog::default()).unwrap();
    let renamed = artifact(&[
        ("states/app.sls", "#!yaml\n"),
        ("states/top.sls", "#!yaml\n"),
    ]);
    runner.plan(&ctx, &renamed, &KeptLog::default()).unwrap();
    assert!(args(&dir).ends_with("state.apply app test=True"));
    assert!(!dir.join("work/states/web.sls").exists());
    assert!(dir.join("work/states/app.sls").exists());
}
