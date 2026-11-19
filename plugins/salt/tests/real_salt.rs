//! The plugin against Salt itself.
//!
//! These tests run when `MERIDIAN_TEST_REAL_SALT_CALL` names a `salt-call`
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
    credential, fixture, graph_from_file, graph_from_text, scratch, secret_in_param, secrets,
    without_positions, KeptLog,
};
use meridian_sdk::{AgentPrograms, Artifact, Data, Emitter, RunContext, Runner};
use meridian_target_salt::{Salt, SaltRunner, Transport};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// `salt-ssh` keeps one deployment of itself per user on the host
/// (`/var/tmp/.<user>_<hash>_salt`), and two runs against the same host at
/// once race over it: one removes and redeploys it while the other is
/// using it. The tests that reach the host over SSH take this lock, so they
/// run one at a time.
static SSH_HOST: std::sync::Mutex<()> = std::sync::Mutex::new(());
const VARIABLE: &str = "MERIDIAN_TEST_REAL_SALT_CALL";

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
/// which an unprivileged Salt cannot do. `MERIDIAN_TEST_REAL_SALT_CALL_ROOT` names
/// a program that runs `salt-call` as root; without it, the ordinary one
/// serves when the tests themselves run as root.
fn privileged(dir: &Path) -> Option<SaltRunner> {
    let as_root = std::env::var("MERIDIAN_TEST_REAL_SALT_CALL_ROOT")
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
        agent: AgentPrograms::default(),
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
#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
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

#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
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

#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
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
    let causes: Vec<(&str, &str)> = err
        .failures
        .iter()
        .map(|f| (f.resource.as_str(), f.cause.as_str()))
        .collect();
    assert_eq!(
        causes,
        [
            ("broken", "resource-failed"),
            ("never", "dependency-failed")
        ]
    );
    assert_eq!(err.failures[0].tool_code, "rc=1");
    assert!(base.is_dir());
    assert!(!base.join("never").exists());
}

#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
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
    assert_eq!(err.failures[0].cause.as_str(), "invalid-config");
}

/// The other transport, against a host that accepts SSH. It runs when
/// `MERIDIAN_TEST_REAL_SALT_SSH` names a `salt-ssh` program;
/// `MERIDIAN_TEST_REAL_SALT_SSH_HOST` names the host (this machine by default) and
/// `MERIDIAN_TEST_REAL_SALT_SSH_ARGS` says how to sign in to it.
#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
#[test]
fn another_host_is_configured_over_ssh() {
    let Some(program) = std::env::var("MERIDIAN_TEST_REAL_SALT_SSH")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    let _one_at_a_time = SSH_HOST.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch(TMP, "real-ssh");
    let runner = SaltRunner {
        salt_call: "salt-call".into(),
        salt_ssh: program,
        transport: Some(Transport::Ssh),
        extra_args: std::env::var("MERIDIAN_TEST_REAL_SALT_SSH_ARGS")
            .map(|v| v.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
        agent: AgentPrograms::default(),
    };
    let ctx = RunContext {
        host: std::env::var("MERIDIAN_TEST_REAL_SALT_SSH_HOST")
            .unwrap_or_else(|_| "127.0.0.1".into()),
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

/// The same host, reached as a credential: the key `MERIDIAN_TEST_REAL_SALT_SSH_KEY`
/// names is read into a secret and handed to `salt-ssh` through an agent
/// Meridian starts, and the user running the test is the credential's user.
/// `MERIDIAN_TEST_REAL_SALT_SSH_ARGS` is not used: it names a key file, which
/// would stand in for the agent.
#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
#[test]
fn another_host_is_reached_as_a_credential_whose_key_is_in_an_agent() {
    let (Some(program), Some(key_path)) = (
        std::env::var("MERIDIAN_TEST_REAL_SALT_SSH")
            .ok()
            .filter(|v| !v.is_empty()),
        std::env::var("MERIDIAN_TEST_REAL_SALT_SSH_KEY")
            .ok()
            .filter(|v| !v.is_empty()),
    ) else {
        return;
    };
    let key = fs::read_to_string(&key_path).unwrap();
    let user = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| {
            String::from_utf8(
                std::process::Command::new("id")
                    .arg("-un")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned()
        });
    let _one_at_a_time = SSH_HOST.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch(TMP, "real-ssh-credential");
    let runner = SaltRunner {
        salt_call: "salt-call".into(),
        salt_ssh: program,
        transport: Some(Transport::Ssh),
        // The host's key is not known yet; nothing else is said about how
        // to sign in, which is the credential's to say.
        extra_args: vec!["-i".into()],
        agent: AgentPrograms::default(),
    };
    let ctx = RunContext {
        host: std::env::var("MERIDIAN_TEST_REAL_SALT_SSH_HOST")
            .unwrap_or_else(|_| "127.0.0.1".into()),
        credential: Some(credential("ssh", &user, &[("privateKey", key.trim_end())])),
        ..context(&dir)
    };
    let base = dir.join("node");
    let artifact = compile(&format!(
        "    - id: made\n      type: file\n      state: directory\n      params: {{path: {0}/etc}}\n",
        base.display()
    ));
    let log = KeptLog::default();

    let plan = runner
        .plan(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("planning: {e}\n{}", log.text().join("\n")));
    assert_eq!(ids(&plan.changes), ["made"]);
    let applied = runner
        .apply(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("applying: {e}\n{}", log.text().join("\n")));
    assert_eq!(ids(&applied.changes), ["made"]);
    assert!(base.join("etc").is_dir());
    assert!(runner.plan(&ctx, &artifact, &log).unwrap().in_sync());
    // The key is in nothing the plugin wrote or printed.
    let fingerprint = key.lines().nth(1).unwrap().trim();
    nothing_under_work_holds(&dir, fingerprint);
    assert!(!log.text().iter().any(|l| l.contains(fingerprint)));
}

/// Nothing the plugin wrote to its work directory holds `value`.
fn nothing_under_work_holds(dir: &Path, value: &str) {
    fn walk(dir: &Path, value: &str) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, value);
            } else if path.is_file() {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(value),
                    "{} holds the secret",
                    path.display()
                );
            }
        }
    }
    walk(&dir.join("work"), value);
}

const SECRET: &str = "s3cr3t-not-a-real-password-Q9";

#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_salt_as_pillar_data_and_is_in_nothing_salt_prints_or_the_plugin_writes() {
    let dir = scratch(TMP, "real-secret");
    let Some(runner) = real(&dir) else { return };
    let base = dir.join("node");
    let path = base.join("app.env");
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: salt\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: app_dir\n      type: file\n      state: directory\n      params:\n        path: {base}\n    - id: env_file\n      type: file\n      params:\n        path: {path}\n        content: placeholder\n        mode: \"0600\"\n      dependsOn:\n        - resource: app_dir\n",
        base = base.display(),
        path = path.display()
    );
    let mut graph = graph_from_text("site.yaml", &text, "salt");
    let slot = secret_in_param(&mut graph, "env_file", "content", "db_password");
    let artifact = Salt
        .emit(&graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    assert!(!artifact.files["states/web.sls"].contains(SECRET));
    let mut ctx = context(&dir);
    ctx.secrets = secrets(&[(slot, "db_password", SECRET)]);

    // A dry run sees the change and, show_changes being off, prints no
    // diff of it.
    let log = KeptLog::default();
    let plan = runner
        .plan(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("planning: {e}\n{}", log.text().join("\n")));
    assert_eq!(
        ids(&plan.changes),
        ["app_dir", "env_file"],
        "{}",
        log.text().join("\n")
    );
    assert!(
        !log.text().iter().any(|line| line.contains(SECRET)),
        "{:?}",
        log.text()
    );
    assert!(!path.exists(), "a plan creates nothing");

    let log = KeptLog::default();
    runner
        .apply(&ctx, &artifact, &log)
        .unwrap_or_else(|e| panic!("applying: {e}\n{}", log.text().join("\n")));
    assert!(
        !log.text().iter().any(|line| line.contains(SECRET)),
        "{:?}",
        log.text()
    );
    // Salt ends a managed file with a newline, as it does for any content.
    assert_eq!(fs::read_to_string(&path).unwrap(), format!("{SECRET}\n"));
    nothing_under_work_holds(&dir, SECRET);
}

#[ignore = "needs a real salt-call; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_state_functions_schemas_are_exported_for_an_editor() {
    use meridian_sdk::{SchemaRequest, SchemaSource};
    use meridian_target_salt::SaltSchemas;

    let dir = scratch(TMP, "real-schemas");
    let Some(runner) = real(&dir) else {
        eprintln!("skipped: set {VARIABLE} to a salt-call program");
        return;
    };
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: salt\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: motd\n      type: file\n      params:\n        path: /etc/motd\n        content: hi\n    - id: line\n      type: file.append\n      params:\n        name: /etc/motd\n        text: hi\n    - id: nowhere\n      type: example.missing\n      params:\n        x: 1\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("site.yaml", text, "salt")],
    );
    let log = KeptLog::default();
    let export = SaltSchemas {
        program: runner.salt_call.clone(),
        extra_args: runner.extra_args.clone(),
    }
    .export(&request, &log)
    .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(export.tool, "salt");
    assert!(
        export
            .tool_version
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit()),
        "{:?}",
        export.tool_version
    );
    assert_eq!(export.pins.get("salt"), Some(&export.tool_version));
    let append = export
        .schemas
        .iter()
        .find(|s| s.r#type == "file.append")
        .unwrap_or_else(|| panic!("file.append is described: {:?}", export.notes));
    assert!(
        append.open,
        "a state function takes keyword arguments: {append:?}"
    );
    assert!(append.find_param("text").is_some(), "{append:?}");
    assert!(
        append.find_param("name").is_none_or(|p| !p.required),
        "name is the id: {append:?}"
    );
    assert_eq!(append.actions, ["watch"]);
    assert!(append.source.starts_with("salt "), "{}", append.source);
    let managed = export
        .schemas
        .iter()
        .find(|s| s.r#type == "file.managed")
        .expect("file.managed is listed");
    assert!(managed.open && managed.params.is_empty());
    assert!(
        export.notes.iter().any(|n| n.contains("example.missing")),
        "{:?}",
        export.notes
    );
}

#[ignore = "needs a real salt-call or salt-ssh; see docs/contributing/real-tool-tests.md"]
#[test]
fn inspect_runs_one_state_in_test_mode_and_changes_nothing() {
    let dir = scratch(TMP, "real-inspect");
    let Some(runner) = real(&dir) else { return };
    let base = dir.join("node");
    let conf = base.join("srv/app/conf/app.conf");
    let ctx = context(&dir);
    let artifact = compile(&site(&base, "v1"));
    runner
        .apply(&ctx, &artifact, &KeptLog::default())
        .unwrap_or_else(|e| panic!("applying: {e}"));

    let read = runner
        .inspect(&ctx, &artifact, "app_conf", "localhost")
        .unwrap_or_else(|e| panic!("inspecting: {e}"));
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(true));
    assert!(
        read.command.contains("state.sls_id app_conf web test=True"),
        "{}",
        read.command
    );

    fs::write(&conf, "drifted\n").unwrap();
    let read = runner
        .inspect(&ctx, &artifact, "app_conf", "localhost")
        .unwrap_or_else(|e| panic!("inspecting: {e}"));
    assert_eq!(read.in_desired_state, Some(false));
    assert_eq!(
        fs::read_to_string(&conf).unwrap(),
        "drifted\n",
        "inspect changes nothing"
    );
}
