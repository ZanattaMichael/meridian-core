//! The plugin against Salt itself.
//!
//! These tests run when `MERIDIAN_TEST_SALT_CALL` names a `salt-call`
//! program, and pass without doing anything when it does not, so the suite
//! needs no Salt installed. CI installs one and sets the variable. The one
//! test that dry-runs package, service and account states needs a Salt that
//! runs as root; see [`privileged`].
//!
//! Salt is run masterless and unprivileged: a configuration directory made
//! for the test keeps its cache and logs in a scratch directory, and every
//! state that is really applied touches only that directory.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{
    fixture, graph_from_file, graph_from_text, scratch, without_positions, KeptLog,
};
use meridian_sdk::{Artifact, Data, Emitter, RunContext, Runner};
use meridian_target_salt::{Salt, SaltRunner, Transport};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const VARIABLE: &str = "MERIDIAN_TEST_SALT_CALL";

/// The output of `id` with one flag: who the tests run as.
fn id(flag: &str) -> String {
    let output = std::process::Command::new("id")
        .arg(flag)
        .output()
        .expect("id runs");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A runner for the real `salt-call`, or `None` when the tests are not asked
/// to use one.
fn real(dir: &Path) -> Option<SaltRunner> {
    let program = std::env::var(VARIABLE).ok().filter(|v| !v.is_empty())?;
    Some(runner_for(dir, program, &id("-un")))
}

/// A runner for a `salt-call` that may change the system, or `None` when
/// there is none. Dry-running a package state refreshes the package index,
/// which an unprivileged Salt cannot do. `MERIDIAN_TEST_SALT_CALL_ROOT` names
/// a program that runs `salt-call` as root; without it, the ordinary one
/// serves when the tests themselves run as root.
fn privileged(dir: &Path) -> Option<SaltRunner> {
    let as_root = std::env::var("MERIDIAN_TEST_SALT_CALL_ROOT")
        .ok()
        .filter(|v| !v.is_empty());
    match as_root {
        Some(program) => Some(runner_for(dir, program, "root")),
        None if id("-u") == "0" => real(dir),
        None => None,
    }
}

fn runner_for(dir: &Path, program: String, user: &str) -> SaltRunner {
    let config = dir.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("minion"),
        format!(
            "root_dir: {}\nfile_client: local\nid: meridian-test\nuser: {user}\n",
            dir.join("root").display(),
        ),
    )
    .unwrap();
    SaltRunner {
        salt_call: program,
        salt_ssh: "salt-ssh".into(),
        transport: Some(Transport::Local),
        extra_args: vec![
            "--config-dir".into(),
            config.to_str().unwrap().to_owned(),
            "--log-level".into(),
            "error".into(),
        ],
    }
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web".into(),
        host: "localhost".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn compile(resources: &str) -> Artifact {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: salt\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n{resources}"
    );
    Salt.emit(&graph_from_text("site.yaml", &text, "salt"), &Data::new())
        .unwrap_or_else(|e| panic!("compiling: {e}"))
        .artifact
}

fn ids(changes: &[meridian_sdk::Change]) -> Vec<&str> {
    changes.iter().map(|c| c.resource.as_str()).collect()
}

/// Every shared fixture, dry-run by Salt. Nothing is changed. A file that
/// did not render, a state function that does not exist, an argument Salt
/// does not know or a requisite that names nothing would each fail here.
#[test]
fn salt_accepts_what_every_fixture_compiles_to() {
    let dir = scratch(TMP, "real-fixtures");
    let Some(runner) = privileged(&dir) else {
        return;
    };
    let example = "examples/salt/web-server.yaml";
    for name in [
        "accounts",
        "restart_chain",
        "runtime_condition",
        "web_stack",
        example,
    ] {
        let mut graph = without_positions(if name == example {
            graph_from_file(name, "salt")
        } else {
            fixture(name, "salt")
        });
        for resource in &mut graph.resources {
            // The shared fixture states its condition in another target's
            // terms.
            if resource.runtime_when.starts_with("ansible_") {
                resource.runtime_when = "grains['os_family'] == 'Debian'".to_owned();
            }
        }
        let artifact = Salt.emit(&graph, &Data::new()).unwrap().artifact;
        let log = KeptLog::default();
        let plan = runner
            .plan(&context(&dir), &artifact, &log)
            .unwrap_or_else(|e| panic!("{name}: {e}\n{}", log.text().join("\n")));
        assert!(!plan.changes.is_empty(), "{name} would change nothing");
        // Salt puts what it objects to in a state without failing it, such as
        // an argument the function does not take, in the log as a warning.
        for line in log.text() {
            assert!(
                !line.contains("invalid keyword") && !line.contains("is not available"),
                "{name}: {line}"
            );
        }
    }
}

/// The resources of the document the next test applies, under `base`.
fn site(base: &Path, content: &str) -> String {
    let base = base.display();
    format!(
        r#"    - id: app_dir
      type: file
      state: directory
      params:
        path: {base}/srv/app/conf
        mode: "0750"
    - id: app_conf
      type: file
      params:
        path: {base}/srv/app/conf/app.conf
        mode: "0640"
        content: {content:?}
      dependsOn:
        - resource: app_dir
      notifies:
        - resource: reindex
          action: run
    - id: build
      type: exec
      params:
        command: touch built
        chdir: {base}/srv/app
        creates: {base}/srv/app/built
      dependsOn:
        - resource: app_dir
    - id: reindex
      type: exec
      params:
        command: echo indexed >> {base}/srv/app/index.log
        shell: true
      dependsOn:
        - resource: build
    - id: on_linux
      type: exec
      runtimeWhen: grains['kernel'] == 'Linux'
      params:
        command: touch {base}/srv/app/linux
        creates: {base}/srv/app/linux
      dependsOn:
        - resource: app_dir
    - id: on_plan9
      type: exec
      runtimeWhen: grains['kernel'] == 'Plan9'
      params:
        command: touch {base}/srv/app/plan9
        creates: {base}/srv/app/plan9
      dependsOn:
        - resource: app_dir
    - id: after_plan9
      type: exec
      params:
        command: touch {base}/srv/app/after
        creates: {base}/srv/app/after
      dependsOn:
        - resource: on_plan9
    - id: cleanup
      type: exec
      params:
        command: rm {base}/srv/app/built
        removes: {base}/srv/app/no-such-file
      dependsOn:
        - resource: build
"#
    )
}

/// Text Jinja would take for a template, a YAML document in its own right,
/// and a last line with no newline.
const CONTENT: &str = "# {{ not.a.variable }} {% not a tag %} {# not a comment #}\nkey: value\n  indented: \"quoted\"\nlast";

#[test]
fn a_document_is_planned_applied_and_then_in_sync() {
    let dir = scratch(TMP, "real-apply");
    let Some(runner) = real(&dir) else { return };
    let base = dir.join("node");
    let app = base.join("srv/app");
    let ctx = context(&dir);
    let log = KeptLog::default();
    let artifact = compile(&site(&base, CONTENT));

    // A plan says what would change and changes nothing.
    let plan = runner
        .plan(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("planning: {e}"));
    assert_eq!(
        ids(&plan.changes),
        [
            "app_dir",
            "app_conf",
            "build",
            "reindex",
            "on_linux",
            "after_plan9",
            "run reindex"
        ],
        "{}",
        log.text().join("\n")
    );
    assert!(!base.exists(), "a plan created {}", base.display());

    // An apply makes it so.
    let applied = runner
        .apply(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("applying: {e}"));
    assert_eq!(ids(&applied.changes), ids(&plan.changes));
    assert_eq!(
        fs::read_to_string(app.join("conf/app.conf")).unwrap(),
        format!("{CONTENT}\n"),
        "the content reached the node as it was written"
    );
    assert!(app.join("built").exists());
    // A runtime condition the node meets runs its state; one it does not
    // meet skips it, and what depends on the skipped state still runs.
    assert!(app.join("linux").exists());
    assert!(!app.join("plan9").exists());
    assert!(app.join("after").exists());
    // `removes` names a path that is not there, so the command did not run.
    assert!(app.join("built").exists());
    // The resource ran once in its place and once more because the file that
    // notifies it changed.
    assert_eq!(
        fs::read_to_string(app.join("index.log")).unwrap(),
        "indexed\nindexed\n"
    );

    // Nothing is left to do, but for the one command with no guard, which
    // runs every time. The state that reacts to the file does not.
    let plan = runner.plan(&ctx, &artifact, &log).unwrap();
    assert_eq!(ids(&plan.changes), ["reindex"]);
    let applied = runner.apply(&ctx, &artifact, &log).unwrap();
    assert_eq!(ids(&applied.changes), ["reindex"]);
    assert_eq!(
        fs::read_to_string(app.join("index.log")).unwrap(),
        "indexed\nindexed\nindexed\n"
    );

    // Changing the file changes it, and only then is the notified resource
    // run a second time.
    let artifact = compile(&site(&base, "changed\n"));
    let plan = runner.plan(&ctx, &artifact, &log).unwrap();
    assert_eq!(ids(&plan.changes), ["app_conf", "reindex", "run reindex"]);
    let applied = runner.apply(&ctx, &artifact, &log).unwrap();
    assert_eq!(
        ids(&applied.changes),
        ["app_conf", "reindex", "run reindex"]
    );
    assert_eq!(
        fs::read_to_string(app.join("conf/app.conf")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(app.join("index.log")).unwrap(),
        "indexed\nindexed\nindexed\nindexed\nindexed\n"
    );
}

#[test]
fn a_failed_state_stops_what_depends_on_it_and_reports_what_was_done() {
    let dir = scratch(TMP, "real-failure");
    let Some(runner) = real(&dir) else { return };
    let base: PathBuf = dir.join("node");
    let artifact = compile(&format!(
        "    - id: made\n      type: file\n      state: directory\n      params: {{path: {0}}}\n    - id: broken\n      type: exec\n      params: {{command: /bin/false}}\n      dependsOn:\n        - resource: made\n    - id: never\n      type: exec\n      params: {{command: touch {0}/never}}\n      dependsOn:\n        - resource: broken\n",
        base.display()
    ));
    let err = runner
        .apply(&context(&dir), &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "salt-call reported 2 failed states");
    assert_eq!(ids(&err.changes), ["made"]);
    assert!(err.output.contains("broken: "), "{}", err.output);
    assert!(
        err.output.contains("never: One or more requisite failed"),
        "{}",
        err.output
    );
    assert!(base.is_dir());
    assert!(!base.join("never").exists());
}

#[test]
fn a_condition_that_is_not_an_expression_is_salts_to_refuse() {
    let dir = scratch(TMP, "real-render");
    let Some(runner) = real(&dir) else { return };
    let artifact = compile(
        "    - id: job\n      type: exec\n      runtimeWhen: grains['kernel' ==\n      params: {command: /bin/true}\n",
    );
    let err = runner
        .plan(&context(&dir), &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "salt-call could not compile the state file");
    assert!(err.output.contains("Rendering SLS"), "{}", err.output);
}

/// The other transport, against a host that accepts SSH. It runs when
/// `MERIDIAN_TEST_SALT_SSH` names a `salt-ssh` program;
/// `MERIDIAN_TEST_SALT_SSH_HOST` names the host (this machine by default) and
/// `MERIDIAN_TEST_SALT_SSH_ARGS` says how to sign in to it.
#[test]
fn another_host_is_configured_over_ssh() {
    let Some(program) = std::env::var("MERIDIAN_TEST_SALT_SSH")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    let dir = scratch(TMP, "real-ssh");
    let runner = SaltRunner {
        salt_call: "salt-call".into(),
        salt_ssh: program,
        transport: Some(Transport::Ssh),
        extra_args: std::env::var("MERIDIAN_TEST_SALT_SSH_ARGS")
            .map(|v| v.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
    };
    let ctx = RunContext {
        host: std::env::var("MERIDIAN_TEST_SALT_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        ..context(&dir)
    };
    let base = dir.join("node");
    let artifact = compile(&format!(
        "    - id: made\n      type: file\n      state: directory\n      params: {{path: {0}/etc}}\n    - id: conf\n      type: file\n      params: {{path: {0}/etc/app.conf, content: \"over ssh\\n\"}}\n      dependsOn:\n        - resource: made\n",
        base.display()
    ));
    let log = KeptLog::default();

    let plan = runner
        .plan(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("planning: {e}\n{}", log.text().join("\n")));
    assert_eq!(ids(&plan.changes), ["made", "conf"]);
    assert!(!base.exists(), "a plan created {}", base.display());

    let applied = runner
        .apply(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("applying: {e}\n{}", log.text().join("\n")));
    assert_eq!(ids(&applied.changes), ["made", "conf"]);
    assert_eq!(
        fs::read_to_string(base.join("etc/app.conf")).unwrap(),
        "over ssh\n"
    );
    assert!(runner.plan(&ctx, &artifact, &log).unwrap().in_sync());
}
