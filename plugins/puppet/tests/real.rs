//! The plugin against a real Puppet.
//!
//! These tests run `puppet` for real: they parse what the plugin compiles,
//! and plan and apply manifests that manage files under a scratch directory,
//! which needs no root. They are marked `#[ignore]` and run when
//! `MERIDIAN_TEST_REAL_PUPPET` is set, as the `puppet` workflow sets it after
//! installing the gem:
//!
//! ```text
//! gem install puppet
//! MERIDIAN_TEST_REAL_PUPPET=1 cargo test -p meridian-target-puppet --test real -- --ignored
//! ```
//!
//! `MERIDIAN_PUPPET` names another program. The `openvox` target shares this
//! plugin's compiler and has the fuller test; this one proves the `puppet`
//! target's own runner, which drives `puppet apply` on the node, against the
//! tool it is named after.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;
use std::process::Command;

use meridian_plugin_testkit::{
    fixture, graph_from_file, graph_from_text, real_tool_program, scratch, secret_in_param,
    secrets, KeptLog,
};
use meridian_sdk::tool::write_artifact;
use meridian_sdk::{
    AgentPrograms, Artifact, ChangeAction, Data, Emitter, Plan, RunContext, Runner, SecretRef,
};
use meridian_target_puppet::{Puppet, PuppetRunner, Transport, MANIFEST_PATH, SITE_PATH};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// The real `puppet`, or `None` when these tests are not asked for.
fn puppet() -> Option<String> {
    real_tool_program("PUPPET", "puppet")
}

/// Arguments that keep one test's settings and cache out of another's, and
/// out of the home directory of whoever runs them.
fn isolated(dir: &Path) -> Vec<String> {
    vec![
        format!("--confdir={}", dir.join("puppet/conf").display()),
        format!("--vardir={}", dir.join("puppet/var").display()),
        // The code and log directories are not under confdir by default
        // (they are ~/.puppetlabs/etc/code and ~/.puppetlabs/var/log), and
        // two tests creating them at once trip over each other.
        format!("--codedir={}", dir.join("puppet/code").display()),
        format!("--logdir={}", dir.join("puppet/log").display()),
        format!("--rundir={}", dir.join("puppet/run").display()),
        // The public directory is under ~/.puppetlabs/opt too, and every
        // run makes it; two tests making it at once trip over each other.
        format!("--publicdir={}", dir.join("puppet/public").display()),
    ]
}

fn runner(puppet: &str, dir: &Path) -> PuppetRunner {
    PuppetRunner {
        puppet: puppet.to_owned(),
        bolt: "bolt".to_owned(),
        transport: Some(Transport::Local),
        extra_args: isolated(dir),
        agent: AgentPrograms::default(),
    }
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
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n{resources}"
    );
    Puppet
        .emit(&graph_from_text("site.yaml", &text, "puppet"), &Data::new())
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

#[ignore = "needs a real puppet; see docs/contributing/real-tool-tests.md"]
#[test]
fn everything_the_plugin_compiles_is_a_manifest_puppet_parses() {
    let Some(puppet) = puppet() else { return };
    let dir = scratch(TMP, "real-parse");
    let graphs = [
        ("accounts", fixture("accounts", "puppet")),
        ("restart_chain", fixture("restart_chain", "puppet")),
        (
            "example_web",
            graph_from_file("examples/puppet/web-server.yaml", "puppet"),
        ),
    ];
    for (name, graph) in graphs {
        let artifact = Puppet.emit(&graph, &Data::new()).unwrap().artifact;
        let out = dir.join(name);
        write_artifact(&out, &artifact).unwrap();
        let validated = Command::new(&puppet)
            .args(["parser", "validate"])
            .args(isolated(&dir))
            .args([out.join(MANIFEST_PATH), out.join(SITE_PATH)])
            .output()
            .unwrap_or_else(|e| {
                panic!("MERIDIAN_TEST_REAL_PUPPET is set, and {puppet:?} does not run: {e}")
            });
        assert!(
            validated.status.success(),
            "{name}: {}{}",
            String::from_utf8_lossy(&validated.stdout),
            String::from_utf8_lossy(&validated.stderr)
        );
    }
}

#[ignore = "needs a real puppet; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_plan_changes_nothing_an_apply_converges_and_a_second_plan_is_in_sync() {
    let Some(puppet) = puppet() else { return };
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

    // An apply makes it so, in dependency order.
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
        fs::read_to_string(dir.join("app/stamp")).unwrap(),
        "applied\n"
    );

    // Nothing is left to do, and applying again does nothing.
    let plan = runner.plan(&context, &first, &KeptLog::default()).unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
    let again = runner.apply(&context, &first, &KeptLog::default()).unwrap();
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // Drift on the node is seen and put right.
    fs::write(&conf, "workers = 1\n").unwrap();
    let plan = runner.plan(&context, &first, &KeptLog::default()).unwrap();
    assert_eq!(resources(&plan), [("app_conf", ChangeAction::Update)]);
    runner.apply(&context, &first, &KeptLog::default()).unwrap();
    assert_eq!(fs::read_to_string(&conf).unwrap(), "workers = 2\n");
}

#[ignore = "needs a real puppet; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_failed_resource_fails_the_apply_and_keeps_what_was_already_done() {
    let Some(puppet) = puppet() else { return };
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
    assert!(matches!(err.exit_code, Some(4 | 6)), "{err}");
    assert!(err.output.contains("returned 3"), "{err}");
    assert!(
        dir.join("app.conf").is_file(),
        "what came before the failure was done"
    );
    assert!(
        !dir.join("after.conf").exists(),
        "what depended on it was not attempted"
    );
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

/// A file whose content is a secret, compiled with the secret in place.
fn with_secret(dir: &Path) -> (Artifact, SecretRef) {
    let root = dir.display();
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: env_file\n      type: file\n      params:\n        path: {root}/app.env\n        content: placeholder\n        mode: \"0600\"\n"
    );
    let mut graph = graph_from_text("site.yaml", &text, "puppet");
    let slot = secret_in_param(&mut graph, "env_file", "content", "db_password");
    let artifact = Puppet
        .emit(&graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    assert!(!artifact.files["manifests/init.pp"].contains(SECRET));
    (artifact, slot)
}

#[ignore = "needs a real puppet; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_puppet_through_hiera_and_is_in_nothing_puppet_prints_or_the_plugin_writes() {
    let Some(puppet) = puppet() else { return };
    let dir = scratch(TMP, "real-secret");
    let runner = runner(&puppet, &dir);
    let (artifact, slot) = with_secret(&dir);
    let mut context = context(&dir);
    context.secrets = secrets(&[(slot, "db_password", SECRET)]);

    // A noop run sees the change and, the value being Sensitive, prints it
    // redacted.
    let log = KeptLog::default();
    let plan = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    // With the content Sensitive, the tool redacts the one line that would
    // have said the file is new ("defined content as ..."), so the change
    // reads as an update of the content: the redaction is the point.
    assert_eq!(resources(&plan), [("env_file", ChangeAction::Update)]);
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
    nothing_under_work_holds(&dir, SECRET);

    // In sync afterwards, with the same value handed over again.
    let plan = runner
        .plan(&context, &artifact, &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
}
