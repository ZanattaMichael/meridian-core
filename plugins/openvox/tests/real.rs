//! The plugin against the real OpenVox.
//!
//! These tests run `puppet` for real: they parse what the plugin compiles,
//! and plan and apply manifests that manage files under a scratch directory,
//! which needs no root. They run when `MERIDIAN_TEST_OPENVOX` is set, as it
//! is in CI where the workflow installs OpenVox first, and are skipped
//! otherwise so a machine without it still passes `cargo test`.
//!
//! ```text
//! gem install openvox
//! MERIDIAN_TEST_OPENVOX=1 cargo test -p meridian-target-openvox --test real
//! ```

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_host::{Client, StartOptions};
use meridian_plugin_testkit::{
    fixture, graph_from_file, graph_from_text, plugin_binary, scratch, KeptLog,
};
use meridian_sdk::tool::write_artifact;
use meridian_sdk::{Artifact, ChangeAction, Data, Emitter, Plan, RunContext, Runner};
use meridian_target_openvox::{OpenVox, OpenVoxRunner, Verify, MANIFEST_PATH, SITE_PATH};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// The real `puppet`, or `None` when these tests are not asked for.
fn openvox() -> Option<String> {
    if std::env::var_os("MERIDIAN_TEST_OPENVOX").is_none() {
        eprintln!("skipped: set MERIDIAN_TEST_OPENVOX=1 to run against the real OpenVox");
        return None;
    }
    Some(
        std::env::var("MERIDIAN_OPENVOX")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "puppet".to_owned()),
    )
}

/// Arguments that keep one test's settings and cache out of another's, and
/// out of the home directory of whoever runs them.
fn isolated(dir: &Path) -> Vec<String> {
    vec![
        format!("--confdir={}", dir.join("puppet/conf").display()),
        format!("--vardir={}", dir.join("puppet/var").display()),
    ]
}

/// A runner over the real tool that insists it is OpenVox.
fn runner(puppet: &str, dir: &Path) -> OpenVoxRunner {
    let mut runner = OpenVoxRunner::new(puppet, "bolt").with_verify(Verify::Strict);
    runner.tool.extra_args = isolated(dir);
    runner
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
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: openvox\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n{resources}"
    );
    OpenVox
        .emit(
            &graph_from_text("site.yaml", &text, "openvox"),
            &Data::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

/// A directory, a file in it, and a command that runs once.
fn site(root: &Path, content: &str, mode: &str) -> Artifact {
    let root = root.display();
    compile(&format!(
        "    - id: app_dir\n      type: file\n      state: directory\n      params:\n        path: {root}/app\n    - id: app_conf\n      type: file\n      params:\n        path: {root}/app/app.conf\n        content: \"{content}\"\n        mode: \"{mode}\"\n      dependsOn:\n        - resource: app_dir\n    - id: stamp\n      type: exec\n      params:\n        command: /bin/sh -c 'echo applied > {root}/app/stamp'\n        creates: {root}/app/stamp\n      dependsOn:\n        - resource: app_conf\n"
    ))
}

fn resources(plan: &Plan) -> Vec<(&str, ChangeAction)> {
    plan.changes
        .iter()
        .map(|c| (c.resource.as_str(), c.action))
        .collect()
}

#[test]
fn everything_the_plugin_compiles_is_a_manifest_openvox_parses() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-parse");
    let graphs = [
        ("accounts", fixture("accounts", "openvox")),
        ("restart_chain", fixture("restart_chain", "openvox")),
        (
            "example_web",
            graph_from_file("examples/openvox/web.yaml", "openvox"),
        ),
        (
            "example_local",
            graph_from_file("examples/openvox/local.yaml", "openvox"),
        ),
    ];
    for (name, graph) in graphs {
        let artifact = OpenVox.emit(&graph, &Data::new()).unwrap().artifact;
        let out = dir.join(name);
        write_artifact(&out, &artifact).unwrap();
        let validated = Command::new(&puppet)
            .args(["parser", "validate"])
            .args(isolated(&dir))
            .args([out.join(MANIFEST_PATH), out.join(SITE_PATH)])
            .output()
            .expect("puppet runs");
        assert!(
            validated.status.success(),
            "{name}: {}{}",
            String::from_utf8_lossy(&validated.stdout),
            String::from_utf8_lossy(&validated.stderr)
        );
    }
}

#[test]
fn a_plan_changes_nothing_an_apply_converges_and_a_second_plan_is_in_sync() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-converge");
    let runner = runner(&puppet, &dir);
    let context = context(&dir);
    let conf = dir.join("app/app.conf");
    let first = site(&dir, "workers = 2\\n", "0640");

    // A plan reports all three and touches none of them.
    let log = KeptLog::default();
    let plan = runner.plan(&context, &first, &log).unwrap();
    assert!(!plan.unknown);
    assert_eq!(
        resources(&plan),
        [
            ("app_dir", ChangeAction::Create),
            ("app_conf", ChangeAction::Create),
            ("stamp", ChangeAction::Update),
        ],
        "{:#?}",
        log.text()
    );
    assert!(!dir.join("app").exists(), "a plan must change nothing");

    // An apply makes it so, in dependency order, and says what it did.
    let applied = runner.apply(&context, &first, &KeptLog::default()).unwrap();
    assert_eq!(
        applied
            .changes
            .iter()
            .map(|c| c.resource.as_str())
            .collect::<Vec<_>>(),
        ["app_dir", "app_conf", "stamp"]
    );
    assert_eq!(fs::read_to_string(&conf).unwrap(), "workers = 2\n");
    assert_eq!(
        fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::read_to_string(dir.join("app/stamp")).unwrap(),
        "applied\n"
    );

    // Nothing is left to do, and applying again does nothing.
    let plan = runner.plan(&context, &first, &KeptLog::default()).unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
    let again = runner.apply(&context, &first, &KeptLog::default()).unwrap();
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // Editing the document changes only what was edited.
    let second = site(&dir, "workers = 8\\n", "0600");
    let plan = runner.plan(&context, &second, &KeptLog::default()).unwrap();
    assert_eq!(resources(&plan), [("app_conf", ChangeAction::Update)]);
    assert!(plan.changes[0].summary.contains("content"), "{plan:?}");
    assert!(plan.changes[0].summary.contains("mode"), "{plan:?}");
    assert_eq!(fs::read_to_string(&conf).unwrap(), "workers = 2\n");
    runner
        .apply(&context, &second, &KeptLog::default())
        .unwrap();
    assert_eq!(fs::read_to_string(&conf).unwrap(), "workers = 8\n");
    assert_eq!(
        fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // Drift on the node is seen and put right.
    fs::write(&conf, "workers = 1\n").unwrap();
    let plan = runner.plan(&context, &second, &KeptLog::default()).unwrap();
    assert_eq!(resources(&plan), [("app_conf", ChangeAction::Update)]);
    runner
        .apply(&context, &second, &KeptLog::default())
        .unwrap();
    assert_eq!(fs::read_to_string(&conf).unwrap(), "workers = 8\n");
}

#[test]
fn state_absent_removes_what_an_earlier_apply_created() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-absent");
    let runner = runner(&puppet, &dir);
    let context = context(&dir);
    let path = dir.join("old.conf");
    let declare = |state: &str| {
        compile(&format!(
            "    - id: old_conf\n      type: file\n      state: {state}\n      params:\n        path: {}\n",
            path.display()
        ))
    };
    fs::write(&path, "left over\n").unwrap();

    let plan = runner
        .plan(&context, &declare("absent"), &KeptLog::default())
        .unwrap();
    assert_eq!(resources(&plan), [("old_conf", ChangeAction::Delete)]);
    assert!(path.exists());

    let applied = runner
        .apply(&context, &declare("absent"), &KeptLog::default())
        .unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].action, ChangeAction::Delete);
    assert!(!path.exists());
}

#[test]
fn a_change_refreshes_the_command_it_notifies() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-notify");
    let runner = runner(&puppet, &dir);
    let context = context(&dir);
    let root = dir.display();
    let artifact = compile(&format!(
        "    - id: conf\n      type: file\n      params:\n        path: {root}/app.conf\n        content: a\n      notifies:\n        - resource: reload\n          action: run\n    - id: reload\n      type: exec\n      params:\n        command: /bin/sh -c 'echo run >> {root}/runs'\n"
    ));
    let manifest = &artifact.files[MANIFEST_PATH];
    assert!(
        manifest.contains("notify  => Exec['reload'],"),
        "{manifest}"
    );

    let log = KeptLog::default();
    runner.apply(&context, &artifact, &log).unwrap();
    let lines = log.text();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Exec[reload]: Triggered 'refresh' from 1 event")),
        "{lines:#?}"
    );
    // Once as declared and once more for the refresh.
    assert_eq!(fs::read_to_string(dir.join("runs")).unwrap(), "run\nrun\n");
}

#[test]
fn a_failed_resource_fails_the_apply_and_keeps_what_was_already_done() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-failed");
    let runner = runner(&puppet, &dir);
    let root = dir.display();
    let artifact = compile(&format!(
        "    - id: conf\n      type: file\n      params:\n        path: {root}/app.conf\n        content: a\n    - id: broken\n      type: exec\n      params:\n        command: /bin/sh -c 'echo no such luck >&2; exit 3'\n      dependsOn:\n        - resource: conf\n    - id: after\n      type: file\n      params:\n        path: {root}/after.conf\n        content: b\n      dependsOn:\n        - resource: broken\n"
    ));
    let log = KeptLog::default();
    let err = runner
        .apply(&context(&dir), &artifact, &log)
        .expect_err("the apply fails");
    assert_eq!(err.msg, "puppet apply reported a failed resource");
    assert!(matches!(err.exit_code, Some(4 | 6)), "{err}");
    assert!(err.command.contains("apply --detailed-exitcodes"), "{err}");
    assert!(err.output.contains("returned 3"), "{err}");
    // What came before the failure was done and is reported; what depended
    // on it was not attempted.
    assert_eq!(
        err.changes
            .iter()
            .map(|c| c.resource.as_str())
            .collect::<Vec<_>>(),
        ["conf"]
    );
    assert!(dir.join("app.conf").exists());
    assert!(!dir.join("after.conf").exists());
}

#[test]
fn a_manifest_openvox_rejects_is_a_failed_run_with_its_explanation() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-rejected");
    let artifact = Artifact {
        files: [
            (
                MANIFEST_PATH.to_owned(),
                "class meridian_site {\n  file { 'x':\n    no_such_attribute => 1,\n    path => '/tmp/x',\n  }\n}\n"
                    .to_owned(),
            ),
            (
                SITE_PATH.to_owned(),
                "node 'localhost' {\n  include meridian_site\n}\n".to_owned(),
            ),
        ]
        .into(),
    };
    let err = runner(&puppet, &dir)
        .plan(&context(&dir), &artifact, &KeptLog::default())
        .expect_err("the plan fails");
    assert_eq!(err.msg, "puppet apply failed");
    assert_eq!(err.exit_code, Some(1));
    assert!(err.output.contains("no_such_attribute"), "{err}");
    assert!(err.changes.is_empty());
}

#[test]
fn the_plugin_binary_plans_and_applies_with_the_real_tool() {
    let Some(puppet) = openvox() else { return };
    let dir = scratch(TMP, "real-process");
    let options = StartOptions {
        env: Some(vec![
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MERIDIAN_OPENVOX".to_owned(), puppet),
            ("MERIDIAN_OPENVOX_VERIFY".to_owned(), "strict".to_owned()),
            ("MERIDIAN_OPENVOX_ARGS".to_owned(), isolated(&dir).join(" ")),
        ]),
        ..StartOptions::default()
    };
    let client = Client::start(
        plugin_binary("meridian-target-openvox").to_str().unwrap(),
        &options,
    )
    .unwrap_or_else(|e| panic!("starting the plugin: {e}"));
    let target: PathBuf = dir.join("motd");
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: openvox\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: motd\n      type: file\n      params:\n        path: {}\n        content: \"welcome\\n\"\n",
        target.display()
    );
    let artifact = client
        .emit(
            &graph_from_text("site.yaml", &text, "openvox"),
            &Data::new(),
        )
        .unwrap()
        .artifact;

    let mut streamed = Vec::new();
    let plan = client
        .plan(&context(&dir), &artifact, &mut |line| {
            streamed.push(line.text)
        })
        .unwrap();
    assert_eq!(resources(&plan), [("motd", ChangeAction::Create)]);
    assert!(
        streamed.iter().any(|l| l.contains("File[motd]/ensure")),
        "{streamed:#?}"
    );
    assert!(!target.exists());

    let applied = client
        .apply(&context(&dir), &artifact, &mut |_| {})
        .unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(fs::read_to_string(&target).unwrap(), "welcome\n");

    let plan = client.plan(&context(&dir), &artifact, &mut |_| {}).unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
    client.close().unwrap();
}
