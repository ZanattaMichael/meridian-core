//! The plugin against a real `ansible-playbook`.
//!
//! These tests run Ansible for real, against this machine over the `local`
//! connection: they play playbooks the plugin compiled, managing files under
//! a scratch directory, which needs no root and no SSH. They run when
//! `MERIDIAN_TEST_REAL_ANSIBLE=1`, as the `ansible` workflow sets after
//! installing `ansible-core`:
//!
//! ```sh
//! MERIDIAN_TEST_REAL_ANSIBLE=1 cargo test -p meridian-target-ansible --test real
//! ```
//!
//! Without the variable they pass without doing anything. With it, an
//! `ansible-playbook` that does not run is a failure.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use meridian_plugin_testkit::{graph_from_text, scratch, secret_in_param, secrets, KeptLog};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, Plan, RunContext, RunError, Runner,
    SchemaRequest, SchemaSource,
};
use meridian_target_ansible::{Ansible, AnsibleRunner, AnsibleSchemas};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// The runner over the real program, or `None` when these tests are not
/// asked for.
fn ansible() -> Option<AnsibleRunner> {
    if std::env::var("MERIDIAN_TEST_REAL_ANSIBLE").ok().as_deref() != Some("1") {
        eprintln!(
            "skipped: set MERIDIAN_TEST_REAL_ANSIBLE=1 to run against a real ansible-playbook"
        );
        return None;
    }
    let mut runner = AnsibleRunner::from_env();
    let runs = Command::new(&runner.program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        runs,
        "MERIDIAN_TEST_REAL_ANSIBLE=1, and {:?} does not run",
        runner.program
    );
    // The document's host is this machine, reached without SSH.
    runner.extra_args = vec!["--connection".into(), "local".into()];
    Some(runner)
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "site".into(),
        host: "localhost".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn compile(resources: &str) -> Artifact {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n{resources}"
    );
    Ansible
        .emit(
            &graph_from_text("site.yaml", &text, "ansible"),
            &Data::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn plan(runner: &AnsibleRunner, dir: &Path, artifact: &Artifact) -> Plan {
    let log = KeptLog::default();
    runner
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn apply(runner: &AnsibleRunner, dir: &Path, artifact: &Artifact) -> Applied {
    let log = KeptLog::default();
    runner
        .apply(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn changes(list: &[Change]) -> Vec<(&str, ChangeAction)> {
    list.iter()
        .map(|c| (c.resource.as_str(), c.action))
        .collect()
}

/// A directory, a file in it, and a command that runs once.
fn site(root: &Path, content: &str, mode: &str) -> Artifact {
    let root = root.display();
    compile(&format!(
        "    - id: app_dir\n      type: file\n      state: directory\n      params:\n        path: {root}/app\n        mode: \"0755\"\n    - id: app_conf\n      type: file\n      params:\n        path: {root}/app/app.conf\n        content: \"{content}\"\n        mode: \"{mode}\"\n      dependsOn:\n        - resource: app_dir\n    - id: stamp\n      type: exec\n      params:\n        command: /bin/sh -c 'echo applied > {root}/app/stamp'\n        creates: {root}/app/stamp\n      dependsOn:\n        - resource: app_conf\n"
    ))
}

#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_plan_changes_nothing_an_apply_converges_and_a_second_plan_is_in_sync() {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-converge");
    let conf = dir.join("app/app.conf");
    let first = site(&dir, "port = 80\\n", "0640");

    // Check mode says what would change, in the document's order, and
    // changes nothing.
    let preview = plan(&runner, &dir, &first);
    assert!(!preview.unknown);
    assert_eq!(
        changes(&preview.changes),
        [
            ("app_dir", ChangeAction::Update),
            ("app_conf", ChangeAction::Update),
            ("stamp", ChangeAction::Update),
        ]
    );
    assert!(!dir.join("app").exists(), "a plan creates nothing");

    let applied = apply(&runner, &dir, &first);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert!(applied.outputs.is_empty(), "Ansible reports no outputs");
    assert_eq!(fs::read_to_string(&conf).unwrap(), "port = 80\n");
    assert_eq!(
        fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::read_to_string(dir.join("app/stamp")).unwrap(),
        "applied\n"
    );

    // Converged: nothing left to change, and the guarded command is not
    // run again.
    let again = plan(&runner, &dir, &first);
    assert!(again.changes.is_empty(), "{:?}", again.changes);
    assert!(!again.unknown);
    let applied = apply(&runner, &dir, &first);
    assert!(applied.changes.is_empty(), "{:?}", applied.changes);

    // New content and a new mode are one change to the one resource.
    let second = site(&dir, "port = 8080\\n", "0600");
    let preview = plan(&runner, &dir, &second);
    assert_eq!(
        changes(&preview.changes),
        [("app_conf", ChangeAction::Update)]
    );
    assert_eq!(
        fs::read_to_string(&conf).unwrap(),
        "port = 80\n",
        "a plan changes nothing"
    );
    apply(&runner, &dir, &second);
    assert_eq!(fs::read_to_string(&conf).unwrap(), "port = 8080\n");
    assert_eq!(
        fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // Drift is seen: a file changed behind Ansible's back is put right.
    fs::write(&conf, "port = 1\n").unwrap();
    let preview = plan(&runner, &dir, &second);
    assert_eq!(
        changes(&preview.changes),
        [("app_conf", ChangeAction::Update)]
    );
    apply(&runner, &dir, &second);
    assert_eq!(fs::read_to_string(&conf).unwrap(), "port = 8080\n");
}

/// A file that notifies a command, and a task guarded by a fact.
fn notifying(root: &Path, content: &str) -> Artifact {
    let root = root.display();
    compile(&format!(
        "    - id: conf\n      type: file\n      params:\n        path: {root}/app.conf\n        content: \"{content}\"\n      notifies:\n        - resource: reload\n          action: run\n    - id: reload\n      type: exec\n      params:\n        command: /bin/sh -c 'echo reloaded >> {root}/reloads'\n        creates: {root}/never\n    - id: only_here\n      type: file\n      params:\n        path: {root}/here\n        content: yes\n      runtimeWhen: \"ansible_facts['system'] == 'Linux' or ansible_facts['system'] == 'Darwin'\"\n    - id: never_here\n      type: file\n      params:\n        path: {root}/elsewhere\n        content: no\n      runtimeWhen: \"ansible_facts['system'] == 'Plan9'\"\n"
    ))
}

fn reloads(dir: &Path) -> usize {
    fs::read_to_string(dir.join("reloads"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_handler_runs_only_when_its_notifier_changed_and_a_condition_is_answered_by_the_host() {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-notify");
    let first = notifying(&dir, "a\\n");

    let applied = apply(&runner, &dir, &first);
    let names: Vec<&str> = applied
        .changes
        .iter()
        .map(|c| c.resource.as_str())
        .collect();
    eprintln!("{names:?}");
    assert!(names.contains(&"conf"), "{names:?}");
    assert!(names.contains(&"only_here"), "{names:?}");
    assert!(!names.contains(&"never_here"), "{names:?}");
    assert!(dir.join("here").exists());
    assert!(
        !dir.join("elsewhere").exists(),
        "a false condition skips the task"
    );
    // The task itself ran once in the play, and once more as the handler.
    assert_eq!(reloads(&dir), 2);

    // Nothing changed, so the handler does not run again; the command
    // itself does, having no file that says it is done.
    apply(&runner, &dir, &first);
    assert_eq!(reloads(&dir), 3);

    // A change to the file runs the handler again.
    apply(&runner, &dir, &notifying(&dir, "b\\n"));
    assert_eq!(reloads(&dir), 5);
}

#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_command_ansible_cannot_preview_makes_the_plan_unknown() {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-unknown");
    let artifact = compile(&format!(
        "    - id: touch\n      type: exec\n      params:\n        command: touch {}/touched\n",
        dir.display()
    ));
    let preview = plan(&runner, &dir, &artifact);
    assert!(
        preview.unknown,
        "a command with no creates is skipped in check mode"
    );
    assert!(!dir.join("touched").exists());
}

#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_failed_task_fails_the_run_with_ansibles_own_reason_and_what_changed_before_it() {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-failed");
    let root = dir.display();
    let artifact = compile(&format!(
        "    - id: before\n      type: file\n      params:\n        path: {root}/before\n        content: x\n    - id: broken\n      type: exec\n      params:\n        command: /bin/sh -c 'echo no such upstream >&2; exit 3'\n        creates: {root}/never\n      dependsOn:\n        - resource: before\n    - id: after\n      type: file\n      params:\n        path: {root}/after\n        content: x\n      dependsOn:\n        - resource: broken\n"
    ));
    let err: RunError = runner
        .apply(&context(&dir), &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "ansible-playbook reported a failed task");
    assert_eq!(err.exit_code, Some(2));
    assert!(err.output.contains("no such upstream"), "{}", err.output);
    assert_eq!(changes(&err.changes), [("before", ChangeAction::Update)]);
    assert!(dir.join("before").exists());
    assert!(
        !dir.join("after").exists(),
        "nothing runs after the failure"
    );
}

#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_the_play_through_a_private_file_and_is_in_nothing_ansible_prints_or_the_plugin_writes(
) {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-secret");
    const VALUE: &str = "s3cr3t-not-a-real-password-Q9";
    let root = dir.display();
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: env_file\n      type: file\n      params:\n        path: {root}/app.env\n        content: placeholder\n        mode: \"0600\"\n"
    );
    let mut graph = graph_from_text("site.yaml", &text, "ansible");
    let slot = secret_in_param(&mut graph, "env_file", "content", "db_password");
    let artifact = Ansible
        .emit(&graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    assert!(!artifact.files["playbook.yml"].contains(VALUE));
    let mut context = context(&dir);
    context.secrets = secrets(&[(slot, "db_password", VALUE)]);

    // A check run with diff would print the content; a task handed a
    // secret prints nothing.
    let log = KeptLog::default();
    let preview = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(
        changes(&preview.changes),
        [("env_file", ChangeAction::Update)]
    );
    assert!(
        !log.text().iter().any(|line| line.contains(VALUE)),
        "{:?}",
        log.text()
    );

    let log = KeptLog::default();
    runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert!(
        !log.text().iter().any(|line| line.contains(VALUE)),
        "{:?}",
        log.text()
    );
    assert_eq!(fs::read_to_string(dir.join("app.env")).unwrap(), VALUE);
    // Nothing the plugin wrote to its work directory holds the value.
    for entry in fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(VALUE),
                "{} holds the secret",
                path.display()
            );
        }
    }
}

#[test]
#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
fn a_credential_names_the_user_in_the_inventory_and_its_key_reaches_ansible_privately() {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-credential");
    const KEY: &str =
        "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-Q9\n-----END OPENSSH PRIVATE KEY-----";
    // The local connection runs as whoever runs the tests, so the user
    // named in the inventory is that user: Ansible reads it, and over SSH
    // it would log in as them. The key file is named on the command line
    // and, over the local connection, never opened.
    let user = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "root".to_owned());
    let root = dir.display();
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: localhost\n    credential: deploy\n  secrets:\n    - {{name: deploy_key, from: env, key: DEPLOY_KEY}}\n  credentials:\n    - {{name: deploy, type: ssh, user: {user}, privateKey: \"${{{{ secret.deploy_key }}}}\"}}\n  resources:\n    - id: marker\n      type: file\n      params:\n        path: {root}/marker\n        content: \"reached\\n\"\n"
    );
    let graph = graph_from_text("site.yaml", &text, "ansible");
    let artifact = Ansible
        .emit(&graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    assert_eq!(
        artifact.files["inventory.ini"],
        format!("[meridian]\nlocalhost ansible_user={user}\n")
    );
    let mut context = context(&dir);
    let declared = graph.credential.clone().unwrap();
    context.credential = Some(meridian_sdk::HeldCredential::from_values(
        declared,
        vec![(
            "privateKey".to_owned(),
            meridian_sdk::SecretValue::from_reader(KEY.as_bytes(), 4096).unwrap(),
        )],
    ));

    let log = KeptLog::default();
    let applied = runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(
        changes(&applied.changes),
        [("marker", ChangeAction::Update)]
    );
    assert_eq!(fs::read_to_string(dir.join("marker")).unwrap(), "reached\n");
    assert!(
        !log.text()
            .iter()
            .any(|line| line.contains("not-a-real-key")),
        "{:?}",
        log.text()
    );
    for entry in fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("not-a-real-key"),
                "{} holds the key",
                path.display()
            );
        }
    }
}

/// A module named in full, beside the shared types: a line kept in a
/// file, and a module that only reads.
/// `create: true` is the module's own argument: the file does not exist
/// yet, and without it lineinfile fails rather than creating it. No shared
/// `file` writes the file beside it, since one would put its own content
/// back on every run.
fn with_modules(root: &Path, line: &str) -> Artifact {
    let root = root.display();
    compile(&format!(
        "    - id: workers\n      type: ansible.builtin.lineinfile\n      state: present\n      params:\n        path: {root}/app.conf\n        regexp: '^workers'\n        line: \"{line}\"\n        create: true\n      notifies:\n        - resource: stamp\n          action: run\n    - id: stamp\n      type: ansible.builtin.copy\n      params:\n        dest: {root}/stamp\n        content: \"changed\\n\"\n    - id: looked\n      type: ansible.builtin.stat\n      params:\n        path: {root}/app.conf\n      dependsOn:\n        - resource: workers\n"
    ))
}

#[ignore = "needs a real ansible-playbook; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_module_named_in_full_runs_with_its_own_arguments_and_is_run_again_when_notified() {
    let Some(runner) = ansible() else { return };
    let dir = scratch(TMP, "real-modules");
    let first = with_modules(&dir, "workers = 4");

    // The stamp is a task of its own too, so it is written once in its
    // place, and once more as the handler the changed line notifies.
    let preview = plan(&runner, &dir, &first);
    assert!(!preview.unknown, "{preview:?}");
    assert_eq!(
        changes(&preview.changes),
        [
            ("workers", ChangeAction::Update),
            ("stamp", ChangeAction::Update),
            ("run stamp", ChangeAction::Update),
        ]
    );
    // In the real run the stamp task writes the file in its place, so the
    // handler, the same module run again because the line changed, finds
    // it there and reports nothing; in the check run neither had happened.
    let applied = apply(&runner, &dir, &first);
    assert_eq!(
        changes(&applied.changes),
        [
            ("workers", ChangeAction::Update),
            ("stamp", ChangeAction::Update),
        ]
    );
    assert_eq!(
        fs::read_to_string(dir.join("app.conf")).unwrap(),
        "workers = 4\n"
    );
    assert_eq!(fs::read_to_string(dir.join("stamp")).unwrap(), "changed\n");

    // In sync, and a module that only reads reports no change.
    let again = plan(&runner, &dir, &first);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // The state argument is the module's own: the line is taken out.
    let gone = compile(&format!(
        "    - id: workers\n      type: ansible.builtin.lineinfile\n      state: absent\n      params:\n        path: {}/app.conf\n        regexp: '^workers'\n",
        dir.display()
    ));
    let applied = apply(&runner, &dir, &gone);
    assert_eq!(
        changes(&applied.changes),
        [("workers", ChangeAction::Update)]
    );
    assert_eq!(fs::read_to_string(dir.join("app.conf")).unwrap(), "");

    // An argument the module does not take is Ansible's to refuse, when
    // the play runs and not before.
    let wrong = compile(&format!(
        "    - id: workers\n      type: ansible.builtin.lineinfile\n      params:\n        path: {}/app.conf\n        no_such_argument: 1\n",
        dir.display()
    ));
    let log = KeptLog::default();
    let failed = runner
        .apply(&context(&dir), &wrong, &log)
        .expect_err("an unknown argument fails the task");
    assert!(
        failed.to_string().contains("failed task"),
        "{failed}\n{}",
        log.text().join("\n")
    );
}

#[ignore = "needs a real ansible-doc; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_modules_schemas_are_exported_for_an_editor() {
    let Some(_) = ansible() else { return };
    let dir = scratch(TMP, "real-schemas");
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: motd\n      type: file\n      params:\n        path: /etc/motd\n        content: hi\n    - id: line\n      type: ansible.builtin.lineinfile\n      params:\n        path: /etc/motd\n        line: hi\n    - id: nowhere\n      type: example.missing.module\n      params:\n        x: 1\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("site.yaml", text, "ansible")],
    );
    let log = KeptLog::default();
    let export = AnsibleSchemas::from_env()
        .export(&request, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(export.tool, "ansible");
    assert!(
        export.tool_version.starts_with("2."),
        "{:?}",
        export.tool_version
    );
    assert_eq!(
        export.pins.get("ansible.builtin"),
        Some(&export.tool_version)
    );
    let line = export
        .schemas
        .iter()
        .find(|s| s.r#type == "ansible.builtin.lineinfile")
        .unwrap_or_else(|| panic!("lineinfile is described: {:?}", export.notes));
    assert!(!line.open);
    assert!(line.find_param("path").is_some_and(|p| p.required));
    assert!(line.find_param("line").is_some());
    assert_eq!(
        line.find_param("state").unwrap().values,
        ["absent", "present"]
    );
    assert!(
        line.source.starts_with("ansible.builtin 2."),
        "{}",
        line.source
    );
    // Every other module of ansible-core is listed by name, open.
    let copy = export
        .schemas
        .iter()
        .find(|s| s.r#type == "ansible.builtin.copy")
        .expect("copy is listed");
    assert!(copy.open && copy.params.is_empty() && !copy.description.is_empty());
    // The module that is not installed is a note, not a failure.
    assert!(
        export
            .notes
            .iter()
            .any(|n| n.contains("example.missing.module")),
        "{:?}",
        export.notes
    );
}
