//! The plugin against a real Chef client in local mode.
//!
//! These tests run `chef-client` (or Cinc's `cinc-client`, the same program
//! built without the trademark and the licence prompt) for real: they
//! converge cookbooks the plugin compiled, managing files under a scratch
//! directory, which needs no root. They run when `MERIDIAN_TEST_REAL_CHEF=1`,
//! as the `chef` workflow sets after installing the client:
//!
//! ```sh
//! MERIDIAN_TEST_REAL_CHEF=1 MERIDIAN_CHEF_CLIENT=cinc-client \
//!     cargo test -p meridian-target-chef --test real
//! ```
//!
//! Without the variable they pass without doing anything. With it, a client
//! that does not run is a failure. `MERIDIAN_TEST_REAL_CHEF_ROOT` names a
//! program that runs the client as root (a one-line `sudo` wrapper); when it
//! is set, packages, users, groups and a service are managed for real too.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use meridian_plugin_testkit::{graph_from_text, scratch, secret_in_param, secrets, KeptLog};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, Plan, RunContext, Runner, SecretRef,
};
use meridian_target_chef::{Chef, ChefRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn runs(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The runner over the real client, or `None` when these tests are not
/// asked for.
fn chef() -> Option<ChefRunner> {
    if std::env::var("MERIDIAN_TEST_REAL_CHEF").ok().as_deref() != Some("1") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_CHEF=1 to run against a real Chef client");
        return None;
    }
    let runner = ChefRunner::from_env();
    assert!(
        runs(&runner.client),
        "MERIDIAN_TEST_REAL_CHEF=1, and {:?} does not run",
        runner.client
    );
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
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n{resources}"
    );
    Chef.emit(&graph_from_text("site.yaml", &text, "chef"), &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn plan(runner: &ChefRunner, dir: &Path, artifact: &Artifact) -> Plan {
    let log = KeptLog::default();
    let plan = runner
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    // Shown with --nocapture, so that what the client really prints for a
    // why-run is on the workflow's log.
    eprintln!("--- why-run ---\n{}", log.text().join("\n"));
    plan
}

fn apply(runner: &ChefRunner, dir: &Path, artifact: &Artifact) -> Applied {
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

#[ignore = "needs a real chef-client; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_plan_changes_nothing_an_apply_converges_and_a_second_plan_is_in_sync() {
    let Some(runner) = chef() else { return };
    let dir = scratch(TMP, "real-converge");
    let conf = dir.join("app/app.conf");
    let first = site(&dir, "port = 80\\n", "0640");

    // Why-run says what would change, in the document's order, and changes
    // nothing.
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

    // An apply does it.
    let applied = apply(&runner, &dir, &first);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert!(applied.outputs.is_empty(), "Chef reports no outputs");
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

    // Drift is seen: a file changed behind Chef's back is put right.
    fs::write(&conf, "port = 1\n").unwrap();
    let preview = plan(&runner, &dir, &second);
    assert_eq!(
        changes(&preview.changes),
        [("app_conf", ChangeAction::Update)]
    );

    // An absent file is a delete.
    let root = dir.display();
    let gone = compile(&format!(
        "    - id: app_conf\n      type: file\n      state: absent\n      params:\n        path: {root}/app/app.conf\n"
    ));
    let preview = plan(&runner, &dir, &gone);
    assert_eq!(
        changes(&preview.changes),
        [("app_conf", ChangeAction::Delete)]
    );
    apply(&runner, &dir, &gone);
    assert!(!conf.exists());
}

#[ignore = "needs a real chef-client; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_notify_and_a_runtime_condition_behave_as_chef_does_them() {
    let Some(runner) = chef() else { return };
    let dir = scratch(TMP, "real-notify");
    let root = dir.display();
    let document = |content: &str| {
        compile(&format!(
            "    - id: conf\n      type: file\n      params:\n        path: {root}/app.conf\n        content: \"{content}\"\n      notifies:\n        - resource: reload\n          action: run\n    - id: reload\n      type: exec\n      params:\n        command: /bin/sh -c 'echo reloaded >> {root}/reloads'\n      runtimeWhen: \"::File.exist?('{root}/allow-reload')\"\n    - id: skipped\n      type: exec\n      params:\n        command: /bin/sh -c 'echo ran > {root}/skipped'\n      runtimeWhen: \"node['platform_family'] == 'no-such-family'\"\n"
        ))
    };

    // The condition is Ruby that Chef evaluates on the node when it reaches
    // the resource. Neither command's condition holds, so neither runs,
    // notified or not.
    let applied = apply(&runner, &dir, &document("one"));
    assert_eq!(changes(&applied.changes), [("conf", ChangeAction::Update)]);
    assert!(!dir.join("reloads").exists());
    assert!(!dir.join("skipped").exists());

    // With its condition true the notified command runs: once for its own
    // action and once more because the file that notifies it changed.
    fs::write(dir.join("allow-reload"), "").unwrap();
    let applied = apply(&runner, &dir, &document("two"));
    assert_eq!(
        changes(&applied.changes),
        [
            ("conf", ChangeAction::Update),
            ("reload", ChangeAction::Update)
        ]
    );
    let reloads = fs::read_to_string(dir.join("reloads")).unwrap();
    assert_eq!(reloads, "reloaded\nreloaded\n");
    assert!(!dir.join("skipped").exists());
}

#[ignore = "needs a real chef-client; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_command_that_fails_fails_the_apply_and_reports_what_changed_before_it() {
    let Some(runner) = chef() else { return };
    let dir = scratch(TMP, "real-failure");
    let root = dir.display();
    let artifact = compile(&format!(
        "    - id: before\n      type: file\n      params:\n        path: {root}/before\n        content: made\n    - id: broken\n      type: exec\n      params:\n        command: /bin/sh -c 'echo no such host >&2; exit 4'\n      dependsOn:\n        - resource: before\n    - id: after\n      type: file\n      params:\n        path: {root}/after\n        content: never\n      dependsOn:\n        - resource: broken\n"
    ));
    let error = runner
        .apply(&context(&dir), &artifact, &KeptLog::default())
        .expect_err("the command fails");
    assert_eq!(error.msg, "chef-client could not converge the cookbook");
    assert!(error.exit_code.is_some_and(|code| code != 0));
    assert!(error.output.contains("no such host"), "{}", error.output);
    assert_eq!(
        changes(&error.changes),
        [("before", ChangeAction::Update)],
        "what was changed before the failure is still reported"
    );
    assert!(dir.join("before").exists());
    assert!(!dir.join("after").exists(), "Chef stops at the failure");
}

/// Packages, accounts and a service, which only root can manage. The names
/// are ones nothing else on a CI runner uses.
#[ignore = "needs a real chef-client; see docs/contributing/real-tool-tests.md"]
#[test]
fn packages_accounts_and_a_service_are_managed_as_root() {
    let Some(mut runner) = chef() else { return };
    let Some(as_root) = std::env::var("MERIDIAN_TEST_REAL_CHEF_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        eprintln!("skipped: MERIDIAN_TEST_REAL_CHEF_ROOT is not set");
        return;
    };
    assert!(runs(&as_root), "{as_root:?} does not run");
    runner.client = as_root;
    let dir = scratch(TMP, "real-root");
    let exists = |args: &[&str]| {
        Command::new(args[0])
            .args(&args[1..])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };

    let present = compile(
        "    - id: tool\n      type: package\n      params:\n        name: hello\n    - id: team\n      type: group\n      params:\n        name: meridian-team\n        system: true\n    - id: deploy\n      type: user\n      params:\n        name: meridian-deploy\n        system: true\n        shell: /usr/sbin/nologin\n        groups: [meridian-team]\n      dependsOn:\n        - resource: team\n    - id: scheduler\n      type: service\n      state: running\n      params:\n        name: cron\n        enabled: true\n",
    );
    let preview = plan(&runner, &dir, &present);
    assert_eq!(
        changes(&preview.changes),
        [
            ("tool", ChangeAction::Update),
            ("team", ChangeAction::Update),
            ("deploy", ChangeAction::Update),
        ],
        "cron already runs on the machine, so the service is in sync"
    );
    assert!(!exists(&["getent", "passwd", "meridian-deploy"]));

    apply(&runner, &dir, &present);
    assert!(exists(&["dpkg", "-s", "hello"]));
    assert!(exists(&["getent", "group", "meridian-team"]));
    let groups = Command::new("id")
        .args(["-nG", "meridian-deploy"])
        .output()
        .expect("id runs");
    assert!(
        String::from_utf8_lossy(&groups.stdout)
            .split_whitespace()
            .any(|g| g == "meridian-team"),
        "the user is in the group"
    );

    let again = plan(&runner, &dir, &present);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // Absent is a removal of each.
    let absent = compile(
        "    - id: deploy\n      type: user\n      state: absent\n      params:\n        name: meridian-deploy\n    - id: team\n      type: group\n      state: absent\n      params:\n        name: meridian-team\n      dependsOn:\n        - resource: deploy\n    - id: tool\n      type: package\n      state: absent\n      params:\n        name: hello\n",
    );
    let applied = apply(&runner, &dir, &absent);
    assert_eq!(
        changes(&applied.changes),
        [
            ("deploy", ChangeAction::Delete),
            ("team", ChangeAction::Delete),
            ("tool", ChangeAction::Delete),
        ]
    );
    assert!(!exists(&["getent", "passwd", "meridian-deploy"]));
    assert!(!exists(&["getent", "group", "meridian-team"]));
    assert!(!exists(&["dpkg", "-s", "hello"]));
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

#[ignore = "needs a real chef-client; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_chef_as_a_node_attribute_and_is_in_nothing_chef_prints_or_the_plugin_writes() {
    let Some(runner) = chef() else { return };
    let dir = scratch(TMP, "real-secret");
    let root = dir.display();
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: env_file\n      type: file\n      params:\n        path: {root}/app.env\n        content: placeholder\n        mode: \"0600\"\n"
    );
    let mut graph = graph_from_text("site.yaml", &text, "chef");
    let slot: SecretRef = secret_in_param(&mut graph, "env_file", "content", "db_password");
    let artifact = Chef
        .emit(&graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    assert!(!artifact.files["cookbooks/meridian_site/recipes/default.rb"].contains(SECRET));
    let mut context = context(&dir);
    context.secrets = secrets(&[(slot, "db_password", SECRET)]);

    // A why-run sees the change and, the resource being sensitive, prints
    // no content.
    let log = KeptLog::default();
    let preview = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(
        changes(&preview.changes),
        [("env_file", ChangeAction::Update)]
    );
    assert!(
        !log.text().iter().any(|line| line.contains(SECRET)),
        "{:?}",
        log.text()
    );
    assert!(!dir.join("app.env").exists(), "a plan creates nothing");

    let log = KeptLog::default();
    runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert!(
        !log.text().iter().any(|line| line.contains(SECRET)),
        "{:?}",
        log.text()
    );
    assert_eq!(fs::read_to_string(dir.join("app.env")).unwrap(), SECRET);
    // Local mode writes the node back after a run, attributes and all; that
    // went to the private directory, not the work directory.
    nothing_under_work_holds(&dir, SECRET);
    assert!(
        !dir.join("work/nodes").exists(),
        "the node was written to the work directory"
    );

    let again = runner
        .plan(&context, &artifact, &KeptLog::default())
        .unwrap();
    assert!(again.changes.is_empty(), "{:?}", again.changes);
}

#[ignore = "needs a real Chef or Cinc client; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_resources_schemas_are_exported_for_an_editor() {
    use meridian_sdk::{SchemaRequest, SchemaSource};
    use meridian_target_chef::ChefSchemas;

    let Some(_) = chef() else { return };
    let dir = scratch(TMP, "real-schemas");
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: motd\n      type: file\n      params:\n        path: /etc/motd\n        content: hi\n    - id: repo\n      type: apt_repository\n      params:\n        uri: https://example.com/apt\n    - id: nowhere\n      type: example_cookbook_resource\n      params:\n        x: 1\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("site.yaml", text, "chef")],
    );
    let log = KeptLog::default();
    let export = ChefSchemas::from_env()
        .export(&request, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(export.tool, "chef");
    assert!(
        export
            .tool_version
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit()),
        "{:?}",
        export.tool_version
    );
    assert_eq!(export.pins.get("chef"), Some(&export.tool_version));
    // What the inspector itself says about the one resource, so a reading
    // this test finds wrong can be compared with the tool's own output.
    for inspector in [
        "cinc-resource-inspector",
        "/opt/cinc/bin/cinc-resource-inspector",
        "chef-resource-inspector",
    ] {
        if let Ok(output) = Command::new(inspector).arg("apt_repository").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            let head: String = text.chars().take(6000).collect();
            eprintln!("--- {inspector} apt_repository ---\n{head}");
            break;
        }
    }
    let repo = export
        .schemas
        .iter()
        .find(|s| s.r#type == "apt_repository")
        .unwrap_or_else(|| panic!("apt_repository is described: {:?}", export.notes));
    eprintln!("--- as read ---\n{repo:?}");
    assert!(!repo.open, "{repo:?}");
    assert!(repo.find_param("uri").is_some(), "{repo:?}");
    assert!(
        repo.actions.contains(&"add".to_owned()),
        "{:?}",
        repo.actions
    );
    assert!(!repo.actions.contains(&"nothing".to_owned()));
    assert!(repo.source.starts_with("chef "), "{}", repo.source);
    // The shared names are only ever under `native/`; the rest by name, open.
    assert!(export.schemas.iter().all(|s| s.r#type != "file"));
    let package = export
        .schemas
        .iter()
        .find(|s| s.r#type == "native/package")
        .expect("package is listed under native/");
    assert!(package.open && package.params.is_empty());
    assert!(export.schemas.iter().any(|s| s.r#type == "cron" && s.open));
    assert!(
        export
            .notes
            .iter()
            .any(|n| n.contains("example_cookbook_resource")),
        "{:?}",
        export.notes
    );
}
