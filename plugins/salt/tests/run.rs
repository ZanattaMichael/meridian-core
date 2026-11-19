//! The running half, against stand-ins for `salt-call` and `salt-ssh`, so it
//! is tested on a machine with no Salt installed. The canned results are the
//! JSON a real Salt 3008 printed for the same states.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    agent_stand_ins, artifact, credential, scratch, secrets, stand_in, KeptLog,
};
use meridian_sdk::{AgentPrograms, Artifact, ChangeAction, RunContext, Runner, SecretRef};
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
        agent: AgentPrograms::default(),
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
        agent: AgentPrograms::default(),
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

#[test]
fn a_host_salt_ssh_would_read_as_an_option_is_refused_before_it_runs() {
    // Issue 134: the core refuses such a host first; the runner is the last
    // thing between it and the command line and the roster.
    let dir = scratch(TMP, "hostile-host");
    let err = runner(&dir, "exit 9")
        .plan(
            &context(&dir, "-oProxyCommand=touch /tmp/pwned"),
            &compiled(),
            &KeptLog::default(),
        )
        .expect_err("refused");
    assert!(
        err.to_string()
            .contains("salt-ssh cannot be given this host"),
        "{err}"
    );
    assert!(!dir.join("args").exists());
}

// ---- secrets ------------------------------------------------------------

/// A stand-in body that keeps what a tool given `--pillar-root` could read:
/// the pillar file and its mode, the top file beside it, and the
/// environment, then prints `result`.
fn keeping_pillar(dir: &Path, result: &str) -> String {
    format!(
        "env > \"{dir}/env\"
while [ $# -gt 0 ]; do
  case \"$1\" in
    --pillar-root)
      echo \"$2\" > \"{dir}/pillar-path\"
      cp \"$2/meridian.sls\" \"{dir}/meridian.sls\"
      cp \"$2/top.sls\" \"{dir}/top.sls\"
      stat -c %a \"$2/meridian.sls\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$2/meridian.sls\" > \"{dir}/mode\"
      shift;;
  esac
  shift
done
{result}",
        dir = dir.display()
    )
}

fn with_secret(dir: &Path, host: &str) -> RunContext {
    let mut context = context(dir, host);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2\"\\")]);
    context
}

#[test]
fn secrets_reach_salt_call_as_a_pillar_root_only_the_user_can_read_and_nowhere_else() {
    let dir = scratch(TMP, "secrets-local");
    let log = KeptLog::default();
    runner(&dir, &keeping_pillar(&dir, &print(APPLIED)))
        .apply(&with_secret(&dir, "localhost"), &compiled(), &log)
        .unwrap();

    assert_eq!(
        fs::read_to_string(dir.join("meridian.sls")).unwrap(),
        "#!yaml\n\"meridian_secret_db_password\": \"hunter2\\\"\\\\\"\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("top.sls")).unwrap(),
        "#!yaml\nbase:\n  '*':\n    - meridian\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    let args = args(&dir);
    assert!(args.contains("--pillar-root "), "{args}");
    assert!(!args.contains("hunter2"), "{args}");
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("hunter2"));
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
    let pillar_dir = fs::read_to_string(dir.join("pillar-path")).unwrap();
    assert!(
        !Path::new(pillar_dir.trim()).exists(),
        "{pillar_dir} was left behind"
    );
}

#[test]
fn over_ssh_the_master_configuration_names_the_pillar_root_for_the_run() {
    let dir = scratch(TMP, "secrets-ssh");
    runner(&dir, &keeping_pillar(&dir, &print(r#"{"web01": {}}"#)))
        .apply(
            &with_secret(&dir, "web01"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    // salt-ssh takes no --pillar-root; the master configuration in the work
    // directory names the private directory instead, and that directory is
    // gone now the run is over.
    let args = args(&dir);
    assert!(!args.contains("--pillar-root"), "{args}");
    assert!(!args.contains("hunter2"), "{args}");
    let master = fs::read_to_string(Path::new(&work(&dir)).join("ssh/master")).unwrap();
    let roots = master
        .split_once("pillar_roots:\n  base:\n    - ")
        .map(|(_, rest)| rest.trim().trim_matches('"').replace("\\\\", "\\"))
        .unwrap_or_else(|| panic!("{master}"));
    assert!(!master.contains("hunter2"), "{master}");
    assert!(!Path::new(&roots).exists(), "{roots} was left behind");
}

#[test]
fn a_run_without_secrets_has_no_pillar_root() {
    let dir = scratch(TMP, "no-secrets");
    runner(&dir, &print(DRY_RUN))
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(!args(&dir).contains("pillar"), "{}", args(&dir));
    let ssh = runner(&dir, &print(r#"{"web01": {}}"#));
    ssh.plan(&context(&dir, "web01"), &compiled(), &KeptLog::default())
        .unwrap();
    let master = fs::read_to_string(Path::new(&work(&dir)).join("ssh/master")).unwrap();
    assert!(!master.contains("pillar_roots"), "{master}");
}

// ---- credentials ----------------------------------------------------------

const KEY: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-9fXw\n-----END OPENSSH PRIVATE KEY-----";

/// A stand-in body for `salt-ssh` that keeps its environment and a copy of
/// the roster it is given, with the roster's mode, then prints `result`.
fn keeping_roster(dir: &Path, result: &str) -> String {
    format!(
        "env > \"{dir}/env\"
while [ $# -gt 0 ]; do
  case \"$1\" in
    --roster-file)
      echo \"$2\" > \"{dir}/roster-path\"
      cp \"$2\" \"{dir}/roster\"
      stat -c %a \"$2\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$2\" > \"{dir}/mode\"
      shift;;
  esac
  shift
done
{result}",
        dir = dir.display()
    )
}

fn remote_result() -> String {
    print(&DRY_RUN.replacen("\"local\"", "\"web01.example.com\"", 1))
}

#[test]
fn a_credentials_key_reaches_salt_ssh_through_an_agent_and_the_roster_names_the_user() {
    let dir = scratch(TMP, "credential-agent");
    let mut salt = runner(&dir, &keeping_roster(&dir, &remote_result()));
    salt.agent = agent_stand_ins(&dir);
    let mut context = context(&dir, "web01.example.com");
    context.credential = Some(credential(
        "ssh",
        "deploy",
        &[("privateKey", KEY), ("passphrase", "open sesame")],
    ));
    let log = KeptLog::default();
    let plan = salt.plan(&context, &compiled(), &log).unwrap();
    assert_eq!(plan.changes.len(), 4);

    // The roster is in the work directory, as without a credential, and
    // says who to sign in as and that the key is in an agent.
    let work = work(&dir);
    assert_eq!(
        fs::read_to_string(dir.join("roster-path"))
            .unwrap()
            .trim()
            .replace('\\', "/"),
        format!("{work}/ssh/roster")
    );
    assert_eq!(
        fs::read_to_string(dir.join("roster")).unwrap(),
        "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\
         \"web01.example.com\":\n  host: \"web01.example.com\"\n  user: \"deploy\"\n  priv: agent-forwarding\n"
    );
    // salt-ssh was told the agent's socket and nothing else of the credential.
    let agent_args = fs::read_to_string(dir.join("ssh-agent.args")).unwrap();
    let socket = agent_args
        .trim()
        .strip_prefix("-D -a ")
        .expect("ssh-agent -D -a <socket>");
    let env = fs::read_to_string(dir.join("env")).unwrap();
    assert!(env.contains(&format!("SSH_AUTH_SOCK={socket}")), "{env}");
    assert!(
        !env.contains("not-a-real-key") && !env.contains("sesame"),
        "{env}"
    );
    assert!(!args(&dir).contains("deploy"), "{}", args(&dir));
    // The key and its passphrase went to ssh-add, from standard input and
    // the askpass program.
    assert_eq!(
        fs::read_to_string(dir.join("ssh-add.args")).unwrap().trim(),
        "-"
    );
    assert_eq!(
        fs::read_to_string(dir.join("added-key")).unwrap(),
        format!("{KEY}\n")
    );
    assert_eq!(
        fs::read_to_string(dir.join("added-passphrase")).unwrap(),
        "open sesame\n"
    );
    // Nothing printed holds them, and the agent's directory is gone.
    assert!(!log
        .text()
        .iter()
        .any(|l| l.contains("not-a-real-key") || l.contains("sesame")));
    assert!(!Path::new(socket).parent().unwrap().exists());
}

#[test]
fn a_credentials_password_is_in_a_roster_only_the_user_can_read_and_not_in_the_work_directory() {
    let dir = scratch(TMP, "credential-password");
    let mut salt = runner(&dir, &keeping_roster(&dir, &remote_result()));
    salt.agent = agent_stand_ins(&dir);
    let mut context = context(&dir, "web01.example.com");
    context.credential = Some(credential(
        "usernamePassword",
        "deploy",
        &[("password", "hunter2\"\\")],
    ));
    let log = KeptLog::default();
    salt.plan(&context, &compiled(), &log).unwrap();

    assert_eq!(
        fs::read_to_string(dir.join("roster")).unwrap(),
        "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\
         \"web01.example.com\":\n  host: \"web01.example.com\"\n  user: \"deploy\"\n  passwd: \"hunter2\\\"\\\\\"\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    let roster_path = fs::read_to_string(dir.join("roster-path")).unwrap();
    let roster_path = Path::new(roster_path.trim());
    let work = Path::new(&work(&dir)).to_path_buf();
    assert!(!roster_path.starts_with(&work), "{}", roster_path.display());
    assert!(
        !roster_path.exists(),
        "{} was left behind",
        roster_path.display()
    );
    assert!(!work.join("ssh/roster").exists());
    nothing_under_holds(&work, "hunter2");
    // No key, so no agent; the password is in no argument, variable or line.
    assert!(!dir.join("ssh-agent.args").exists());
    let shown = format!(
        "{}\n{}\n{}",
        args(&dir),
        fs::read_to_string(dir.join("env")).unwrap(),
        log.text().join("\n")
    );
    assert!(!shown.contains("hunter2"), "{shown}");
}

#[test]
fn a_credential_with_agent_true_uses_the_users_own_agent() {
    let dir = scratch(TMP, "credential-own-agent");
    let mut salt = runner(&dir, &keeping_roster(&dir, &remote_result()));
    salt.agent = agent_stand_ins(&dir);
    let mut context = context(&dir, "web01.example.com");
    let mut held = credential("ssh", "deploy", &[]);
    held.declared.flags.insert("agent".to_owned(), true);
    context.credential = Some(held);
    salt.plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    assert!(fs::read_to_string(dir.join("roster"))
        .unwrap()
        .ends_with("  user: \"deploy\"\n  priv: agent-forwarding\n"));
    assert!(!dir.join("ssh-agent.args").exists());
}

#[test]
fn a_credential_for_this_machine_is_refused() {
    let dir = scratch(TMP, "credential-local");
    let mut context = context(&dir, "localhost");
    context.credential = Some(credential("ssh", "deploy", &[("privateKey", KEY)]));
    let err = runner(&dir, &print(DRY_RUN))
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.contains(
            "the host is this machine, which salt-call configures as the user running Meridian"
        ),
        "{}",
        err.msg
    );
    assert!(!dir.join("args").exists(), "salt-call was started");
}

/// Nothing under `dir` holds `value`.
fn nothing_under_holds(dir: &Path, value: &str) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            nothing_under_holds(&path, value);
        } else if let Ok(text) = fs::read_to_string(&path) {
            assert!(!text.contains(value), "{} holds the secret", path.display());
        }
    }
}

#[test]
fn inspect_runs_one_state_by_its_id_in_test_mode() {
    let dir = scratch(TMP, "inspect");
    let result = r#"{"local": {"file_|-app_conf_|-/etc/app.conf_|-managed": {"name": "/etc/app.conf", "changes": {}, "result": true, "comment": "The file /etc/app.conf is in the correct state", "__id__": "app_conf"}}}"#;
    let read = runner(&dir, &print(result))
        .inspect(
            &context(&dir, "localhost"),
            &compiled(),
            "app_conf",
            "localhost",
        )
        .unwrap();
    let work = work(&dir);
    assert_eq!(
        args(&dir),
        format!(
            "--local --file-root {work}/states --out json --no-color --retcode-passthrough \
             --log-level quiet state.sls_id app_conf web test=True"
        )
    );
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(true));
    assert_eq!(
        read.command,
        "salt-call state.sls_id app_conf web test=True"
    );

    let failed = r#"{"local": {"pkg_|-p_|-p_|-installed": {"result": false, "comment": "No such package", "__id__": "p"}}}"#;
    let error = runner(&dir, &format!("{}\nexit 2", print(failed)))
        .inspect(&context(&dir, "localhost"), &compiled(), "p", "localhost")
        .unwrap_err();
    assert_eq!(error.msg, "salt-call could not read p: No such package");
}
