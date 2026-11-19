//! The running half, against a stand-in for `ansible-playbook`, so it is
//! tested on a machine with no Ansible installed.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, secrets, stand_in, KeptLog};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner, SecretRef};
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
    assert_eq!(err.failures.len(), 1);
    let failure = &err.failures[0];
    assert_eq!(
        (failure.resource.as_str(), failure.endpoint.as_str()),
        ("nginx_svc", "web01")
    );
    assert_eq!(failure.cause.as_str(), "resource-failed");
    assert_eq!(
        failure.summary,
        "Could not find the requested service nginx"
    );
}

#[test]
fn an_unreachable_host_is_named_as_that() {
    let dir = scratch(TMP, "unreachable");
    let err = runner(&dir, "echo 'fatal: [web01]: UNREACHABLE!'\nexit 4")
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "ansible-playbook could not reach the host");
    assert_eq!(err.failures[0].cause.as_str(), "unreachable");
    assert_eq!(err.failures[0].endpoint, "web01");
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

#[test]
fn secrets_reach_ansible_playbook_in_a_private_file_and_nowhere_else() {
    let dir = scratch(TMP, "secrets");
    // The stand-in keeps a copy of the extra-vars file it was pointed at,
    // and of its environment.
    let body = format!(
        "env > \"{dir}/env\"\nfor a in \"$@\"; do case \"$a\" in @*) cp \"${{a#@}}\" \"{dir}/vars\"; stat -c %a \"${{a#@}}\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"${{a#@}}\" > \"{dir}/mode\";; esac; done\n{TWO_CHANGES}",
        dir = dir.display()
    );
    let runner = runner(&dir, &body);
    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2")]);
    let log = KeptLog::default();
    runner.apply(&context, &compiled(), &log).unwrap();

    // The tool read the value from the file, under the variable's name.
    assert_eq!(
        fs::read_to_string(dir.join("vars")).unwrap(),
        "{\"meridian_secret_db_password\":\"hunter2\"}\n"
    );
    // Only the owner may read the file. Windows has no Unix mode to read.
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    // The command line named the file and nothing more, the environment
    // held nothing, and the file is gone now the run is over.
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(args.contains("--extra-vars @"), "{args}");
    assert!(!args.contains("hunter2"), "{args}");
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("hunter2"));
    let path = args
        .split_whitespace()
        .find_map(|a| a.strip_prefix('@'))
        .unwrap();
    assert!(!Path::new(path).exists(), "{path} was left behind");
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
}

#[test]
fn a_run_without_secrets_passes_no_extra_vars() {
    let dir = scratch(TMP, "no-secrets");
    runner(&dir, TWO_CHANGES)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(!args.contains("--extra-vars"), "{args}");
}

// ---- the credential a host is reached with -----------------------------

/// A credential of `kind` with `user` and the secret fields given, held as
/// a run holds one.
fn credential(kind: &str, user: &str, fields: &[(&str, &str)]) -> meridian_sdk::HeldCredential {
    let declared = meridian_sdk::Credential {
        name: "deploy".to_owned(),
        kind: kind.to_owned(),
        ambient: false,
        text: [("user".to_owned(), user.to_owned())].into_iter().collect(),
        flags: Default::default(),
        secret_fields: fields.iter().map(|(f, _)| (*f).to_owned()).collect(),
    };
    meridian_sdk::HeldCredential::from_values(
        declared,
        fields
            .iter()
            .map(|(field, value)| {
                (
                    (*field).to_owned(),
                    meridian_sdk::SecretValue::from_reader(value.as_bytes(), 4096).unwrap(),
                )
            })
            .collect(),
    )
}

/// A stand-in that keeps the extra-vars file, the private key file and its
/// mode, its environment, and then prints a canned run.
fn recording_runner(dir: &Path) -> AnsibleRunner {
    let body = format!(
        "env > \"{dir}/env\"
while [ $# -gt 0 ]; do
  case \"$1\" in
    @*) cp \"${{1#@}}\" \"{dir}/vars\";;
    --private-key) cp \"$2\" \"{dir}/key\"; stat -c %a \"$2\" > \"{dir}/key-mode\" 2>/dev/null || stat -f %Lp \"$2\" > \"{dir}/key-mode\"; shift;;
  esac
  shift
done
{TWO_CHANGES}",
        dir = dir.display()
    );
    runner(dir, &body)
}

const KEY: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-9fXw\n-----END OPENSSH PRIVATE KEY-----";

#[test]
fn a_private_key_reaches_ansible_playbook_in_a_private_file_named_on_the_command_line() {
    let dir = scratch(TMP, "credential-key");
    let runner = recording_runner(&dir);
    let mut context = context(&dir);
    context.credential = Some(credential("ssh", "deploy", &[("privateKey", KEY)]));
    let log = KeptLog::default();
    runner.apply(&context, &compiled(), &log).unwrap();

    // The key, with the line break OpenSSH wants, in a file only the owner
    // may read, and no extra-vars file since there is no password.
    assert_eq!(
        fs::read_to_string(dir.join("key")).unwrap(),
        format!("{KEY}\n")
    );
    #[cfg(unix)]
    assert_eq!(
        fs::read_to_string(dir.join("key-mode")).unwrap().trim(),
        "600"
    );
    assert!(!dir.join("vars").exists());
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(args.contains("--private-key "), "{args}");
    assert!(!args.contains("--extra-vars"), "{args}");
    assert!(!args.contains("not-a-real-key"), "{args}");
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("not-a-real-key"));
    assert!(!log.text().iter().any(|l| l.contains("not-a-real-key")));
    // The file is gone now the run is over.
    let path = args
        .split_whitespace()
        .skip_while(|a| *a != "--private-key")
        .nth(1)
        .unwrap();
    assert!(!Path::new(path).exists(), "{path} was left behind");
}

#[test]
fn a_password_reaches_ansible_playbook_in_the_private_extra_vars_file_beside_the_secrets() {
    let dir = scratch(TMP, "credential-password");
    let runner = recording_runner(&dir);
    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2")]);
    context.credential = Some(credential(
        "usernamePassword",
        "admin",
        &[("password", "pw-not-real-Q9\"")],
    ));
    let log = KeptLog::default();
    runner.plan(&context, &compiled(), &log).unwrap();

    assert_eq!(
        fs::read_to_string(dir.join("vars")).unwrap(),
        "{\"meridian_secret_db_password\":\"hunter2\",\"ansible_password\":\"pw-not-real-Q9\\\"\"}\n"
    );
    assert!(!dir.join("key").exists());
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(args.contains("--extra-vars @"), "{args}");
    assert!(!args.contains("--private-key"), "{args}");
    for shown in [&args, &fs::read_to_string(dir.join("env")).unwrap()] {
        assert!(
            !shown.contains("not-real") && !shown.contains("hunter2"),
            "{shown}"
        );
    }
    assert!(!log.text().iter().any(|l| l.contains("not-real")));
    let path = args
        .split_whitespace()
        .find_map(|a| a.strip_prefix('@'))
        .unwrap();
    assert!(!Path::new(path).exists(), "{path} was left behind");
}

#[test]
fn an_agent_or_ambient_credential_hands_nothing_over() {
    let dir = scratch(TMP, "credential-agent");
    let runner = recording_runner(&dir);
    let mut context = context(&dir);
    context.credential = Some(credential("ssh", "deploy", &[]));
    runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert!(
        !args.contains("--private-key") && !args.contains("--extra-vars"),
        "{args}"
    );
    assert!(!dir.join("key").exists() && !dir.join("vars").exists());
}

#[test]
fn a_debug_level_adds_ansibles_verbosity_and_level_0_adds_nothing() {
    for (level, flag) in [
        (0, None),
        (1, Some("-v")),
        (2, Some("-vvv")),
        (3, Some("-vvvv")),
    ] {
        let dir = scratch(TMP, &format!("debug-{level}"));
        let context = RunContext {
            debug_level: level,
            ..context(&dir)
        };
        runner(&dir, TWO_CHANGES)
            .plan(&context, &compiled(), &KeptLog::default())
            .unwrap();
        let args = fs::read_to_string(dir.join("args")).unwrap();
        let words: Vec<&str> = args.split_whitespace().collect();
        let verbose: Vec<&&str> = words.iter().filter(|w| w.starts_with("-v")).collect();
        match flag {
            None => assert!(verbose.is_empty(), "{args}"),
            Some(flag) => assert_eq!(verbose, [&flag], "{args}"),
        }
        // The playbook is still the last word.
        assert_eq!(words.last(), Some(&"playbook.yml"), "{args}");
    }
}

const PLAYBOOK: &str = "---
- name: web
  hosts: web01
  become: true
  tasks:
    - name: nginx_pkg
      ansible.builtin.package:
        name: nginx
        state: present
    - name: nginx_conf
      ansible.builtin.copy:
        dest: /etc/nginx/nginx.conf
        content: hi
      notify:
        - restart nginx_svc
  handlers:
    - name: restart nginx_svc
      ansible.builtin.service:
        name: nginx
        state: restarted
";

const ONE_TASK: &str = r#"cat <<'OUT'
PLAY [web] *******

TASK [Gathering Facts] *******
ok: [web01]

TASK [nginx_conf] *******
ok: [web01] => {"changed": false, "checksum": "c22b5f9178342609428d6f51b2c5af4c0bde6a42", "dest": "/etc/nginx/nginx.conf", "mode": "0644", "owner": "root", "state": "file", "invocation": {"module_args": {}}}

PLAY RECAP *******
web01 : ok=2 changed=0 unreachable=0 failed=0
OUT"#;

fn inspected() -> Artifact {
    artifact(&[
        ("playbook.yml", PLAYBOOK),
        ("inventory.ini", "[meridian]\nweb01\n"),
    ])
}

#[test]
fn inspect_runs_the_one_task_in_check_mode_and_reads_its_result() {
    let dir = scratch(TMP, "inspect");
    let read = runner(&dir, ONE_TASK)
        .inspect(&context(&dir), &inspected(), "nginx_conf", "web01")
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("args")).unwrap().trim(),
        "--inventory inventory.ini --check --diff --user deploy -v inspect.yml"
    );
    assert_eq!(
        fs::read_to_string(dir.join("work/inspect.yml")).unwrap(),
        "---
- name: web
  hosts: web01
  become: true
  tasks:
    - name: nginx_conf
      ansible.builtin.copy:
        dest: /etc/nginx/nginx.conf
        content: hi
      notify:
        - restart nginx_svc
"
    );
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(true));
    assert_eq!(
        read.actual.get("mode"),
        Some(&meridian_sdk::Value::from("0644"))
    );
    assert!(!read.actual.contains_key("changed"));
    assert!(!read.actual.contains_key("invocation"));
    assert_eq!(
        read.command,
        "ansible-playbook --check --diff -v playbook.yml, task nginx_conf only"
    );
}

#[test]
fn inspect_reads_a_change_check_mode_would_make_as_not_as_declared() {
    let dir = scratch(TMP, "inspect-changed");
    let body = "cat <<'OUT'
TASK [nginx_pkg] *******
changed: [web01] => {\"changed\": true, \"msg\": \"\"}
OUT";
    let read = runner(&dir, body)
        .inspect(&context(&dir), &inspected(), "nginx_pkg", "web01")
        .unwrap();
    assert_eq!(read.in_desired_state, Some(false));
    assert!(!read.actual.contains_key("state"));

    let ok = "cat <<'OUT'
TASK [nginx_pkg] *******
ok: [web01] => {\"changed\": false, \"msg\": \"\"}
OUT";
    let read = runner(&dir, ok)
        .inspect(&context(&dir), &inspected(), "nginx_pkg", "web01")
        .unwrap();
    // Nothing to change: the state the task gives its module holds.
    assert_eq!(
        read.actual.get("state"),
        Some(&meridian_sdk::Value::from("present"))
    );
}

#[test]
fn inspect_names_a_failed_task_and_an_unknown_one() {
    let dir = scratch(TMP, "inspect-failed");
    let body = "cat <<'OUT'
TASK [nginx_pkg] *******
fatal: [web01]: FAILED! => {\"changed\": false, \"msg\": \"No package matching 'nginx' is available\"}
OUT
exit 2";
    let error = runner(&dir, body)
        .inspect(&context(&dir), &inspected(), "nginx_pkg", "web01")
        .unwrap_err();
    assert!(
        error
            .msg
            .contains("No package matching 'nginx' is available"),
        "{}",
        error.msg
    );
    let error = runner(&dir, ONE_TASK)
        .inspect(&context(&dir), &inspected(), "nothing", "web01")
        .unwrap_err();
    assert_eq!(error.msg, "playbook.yml has no task \"nothing\"");
}
