//! The plugin as it ships: the real binary, loaded by the host, driven over
//! the plugin protocol, and run through the orchestrator as a stage of a
//! workflow.

#![allow(clippy::result_large_err)]

use std::fs;
use std::path::{Path, PathBuf};

use meridian_ir::Bundle;
use meridian_orchestrator::{
    run, work_dir, Mode, Observer, Options, Outcome, StageId, StageKind, StageReport,
};
use meridian_plugin_host::{EmitError, Registry, StartOptions};
use meridian_plugin_testkit::{
    assert_golden, graph_from_file, graph_from_text, plugin_binary, scratch, stand_in,
};
use meridian_sdk::{
    Capabilities, Category, ChangeAction, Data, LogLine, Operations, Value, PROTOCOL_VERSION,
};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const PACKAGE: &str = "meridian-target-cloud-init";
const EXAMPLE: &str = "examples/cloud-init/bootstrap.yaml";

/// Starts the plugin with settings added to this process's environment, the
/// way an operator's shell would hand them over.
fn start(settings: &[(&str, &str)]) -> StartOptions {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("MERIDIAN_CLOUD_INIT"))
        .collect();
    env.extend(
        settings
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    StartOptions {
        env: Some(env),
        ..StartOptions::default()
    }
}

/// A registry holding the real plugin, and the fake one to stand for a tool
/// that provisions a machine.
fn registry(settings: &[(&str, &str)]) -> Registry {
    let mut registry = Registry::new();
    registry
        .load(plugin_binary(PACKAGE).to_str().unwrap(), &start(settings))
        .expect("the cloud-init plugin loads");
    registry
        .load(
            meridian_target_fake::plugin_path().to_str().unwrap(),
            &StartOptions::default(),
        )
        .expect("the fake plugin loads");
    registry
}

/// Stand-ins for the two programs the runner drives, each recording how it
/// was called.
struct Tools {
    dir: PathBuf,
    cloud_init: String,
    ssh: String,
}

fn tools(test: &str, cloud_init: &str, ssh: &str) -> Tools {
    let dir = scratch(TMP, test);
    let record = |name: &str, body: &str| {
        stand_in(
            &dir,
            name,
            &format!("echo \"$@\" >> \"{}/{name}.args\"\n{body}", dir.display()),
        )
        .to_str()
        .unwrap()
        .to_owned()
    };
    Tools {
        cloud_init: record("cloud-init", cloud_init),
        ssh: record("ssh", ssh),
        dir,
    }
}

fn bundle(text: &str) -> Bundle {
    let bundle = Bundle::new(meridian_ir::parse("workflow.yaml", text.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn options(tools: &Tools) -> Options {
    Options::new(tools.dir.join("state"))
}

/// Keeps what the tools printed, as the operator would see it.
#[derive(Default)]
struct Lines(Vec<String>);

impl Observer for Lines {
    fn log(&mut self, id: &StageId, line: &LogLine) {
        self.0.push(format!("{}: {}", id.name, line.text));
    }
    fn stage_finished(&mut self, _report: &StageReport) {}
}

#[test]
fn the_host_learns_what_the_plugin_is_from_the_plugin() {
    let registry = registry(&[]);
    assert_eq!(registry.names(), ["cloud-init", "fake"]);
    let info = registry.target("cloud-init").unwrap().info();
    assert_eq!(info.name, "cloud-init");
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.category, Category::ConfigurationManagement);
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(
        info.resource_types,
        ["exec", "file", "group", "package", "service", "user"]
    );
    assert_eq!(
        info.capabilities,
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
        }
    );
    assert!(
        info.path.trim_end_matches(".exe").ends_with(PACKAGE),
        "{}",
        info.path
    );
    registry.close().unwrap();
}

#[test]
fn nothing_is_lost_or_reworded_across_the_process_boundary() {
    let registry = registry(&[]);
    let target = registry.target("cloud-init").unwrap();

    let emitted = target
        .emit(&graph_from_file(EXAMPLE, "cloud-init"), &Data::new())
        .unwrap();
    assert_golden(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata"),
        "bootstrap",
        &emitted,
    );

    // A refusal arrives as a refusal, with the author's line still on it.
    let graph = graph_from_text(
        "site.yaml",
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: cloud-init\n  targetHost:\n    from: inline\n    host: web01\n  resources:\n    \
         - id: old\n      type: user\n      state: absent\n      params: {name: old}\n",
        "cloud-init",
    );
    let Err(EmitError::Refused(refusal)) = target.emit(&graph, &Data::new()) else {
        panic!("removing a user should be refused");
    };
    assert_eq!(refusal.target, "cloud-init");
    assert_eq!(refusal.resource, "old");
    assert_eq!(refusal.rule, "unsupported-state");
    assert_eq!(
        (refusal.pos.file.as_str(), refusal.pos.line),
        ("site.yaml", 11)
    );
    assert!(
        refusal
            .to_string()
            .starts_with("site.yaml:11:7: cloud-init: resource \"old\" [unsupported-state]: "),
        "{refusal}"
    );
    registry.close().unwrap();
}

const SINGLE: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: cloud-init
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: nginx
      type: package
      params:
        name: nginx
"#;

#[test]
fn a_workflow_plans_then_applies_through_the_real_plugin() {
    let tools = tools("workflow", "echo 'Valid schema user-data'", "exit 99");
    let registry = registry(&[("MERIDIAN_CLOUD_INIT", &tools.cloud_init)]);
    let options = options(&tools);
    let id = StageId {
        kind: StageKind::ResourceSet,
        name: "web".into(),
    };
    let seed = work_dir(&options.state_dir, &id, "cloud-init").join("seed");

    let mut lines = Lines::default();
    let report = run(&bundle(SINGLE), &registry, Mode::Plan, &options, &mut lines);
    assert!(report.succeeded(), "{:?}", report.stages[0].outcome);
    let Outcome::Planned(plan) = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].action, ChangeAction::Create);
    assert_eq!(lines.0, ["web: Valid schema user-data"]);
    assert!(!seed.exists(), "a plan wrote the seed");

    let report = run(
        &bundle(SINGLE),
        &registry,
        Mode::Apply,
        &options,
        &mut lines,
    );
    assert!(report.succeeded(), "{:?}", report.stages[0].outcome);
    let Outcome::Applied { changes, outputs } = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert_eq!(changes.len(), 1);
    let user_data = fs::read_to_string(seed.join("user-data")).unwrap();
    assert_eq!(
        user_data,
        "#cloud-config\n# Compiled by Meridian from the document \"web\". Change the document, \
         not this file.\npackages:\n  - nginx\n"
    );
    let seed_dir = outputs.iter().find(|o| o.name == "seed_dir").unwrap();
    assert_eq!(
        seed_dir.value,
        Value::String(
            meridian_sdk::tool::absolute_path(&seed)
                .to_string_lossy()
                .into_owned()
        )
    );

    // Applying the same document again has nothing to do.
    let report = run(
        &bundle(SINGLE),
        &registry,
        Mode::Apply,
        &options,
        &mut lines,
    );
    let Outcome::Applied { changes, .. } = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert!(changes.is_empty());

    // Nothing asked for a wait, so ssh was never run.
    assert!(!tools.dir.join("ssh.args").exists());
    registry.close().unwrap();
}

#[test]
fn rejected_user_data_fails_the_stage_with_what_cloud_init_said() {
    let tools = tools(
        "rejected",
        "echo 'Error: Cloud config schema errors: packages: 1 is not of type array' >&2\nexit 1",
        "",
    );
    let registry = registry(&[("MERIDIAN_CLOUD_INIT", &tools.cloud_init)]);
    let report = run(
        &bundle(SINGLE),
        &registry,
        Mode::Apply,
        &options(&tools),
        &mut Lines::default(),
    );
    assert!(!report.succeeded());
    let Outcome::Failed(why) = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert!(why.contains("cloud-init rejected the user-data"), "{why}");
    assert!(why.contains("is not of type array"), "{why}");
    registry.close().unwrap();
}

/// A machine is provisioned by one tool, and cloud-init's stage then waits
/// for that machine, at the address the first stage reported, to finish its
/// first boot.
const TWO_STAGE: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
  outputs:
    - name: address
      value: "${vm.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: cloud-init
  targetHost:
    from: infrastructure
    ref: network
    output: address
  resources:
    - id: nginx
      type: package
      params:
        name: nginx
"#;

#[test]
fn a_later_stage_waits_for_the_machine_an_earlier_one_created() {
    let tools = tools("two-stage", "", "echo 'status: done'");
    let registry = registry(&[
        ("MERIDIAN_CLOUD_INIT", &tools.cloud_init),
        ("MERIDIAN_CLOUD_INIT_WAIT", "ssh"),
        ("MERIDIAN_CLOUD_INIT_SSH", &tools.ssh),
        ("MERIDIAN_CLOUD_INIT_SSH_ARGS", "-l ubuntu  -i /keys/deploy"),
    ]);
    let mut lines = Lines::default();
    let report = run(
        &bundle(TWO_STAGE),
        &registry,
        Mode::Apply,
        &options(&tools),
        &mut lines,
    );
    assert!(report.succeeded(), "{:?}", report.stages);
    assert_eq!(report.stages[1].host, "10.0.0.5");
    assert_eq!(
        fs::read_to_string(tools.dir.join("ssh.args"))
            .unwrap()
            .trim(),
        "-o BatchMode=yes -l ubuntu -i /keys/deploy 10.0.0.5 cloud-init status --wait --long"
    );
    assert!(
        lines.0.contains(&"web: status: done".to_owned()),
        "{:?}",
        lines.0
    );
    // The machine is reached by an address, so the seed does not name it.
    let artifact = report.stages[1].artifact.as_ref().unwrap();
    assert_eq!(artifact.files["meta-data"], "instance-id: meridian-web\n");
    registry.close().unwrap();
}

#[test]
fn a_first_boot_that_failed_stops_the_workflow() {
    let tools = tools("two-stage-failed", "", "echo 'status: error'\nexit 1");
    let registry = registry(&[
        ("MERIDIAN_CLOUD_INIT", &tools.cloud_init),
        ("MERIDIAN_CLOUD_INIT_WAIT", "ssh"),
        ("MERIDIAN_CLOUD_INIT_SSH", &tools.ssh),
    ]);
    let report = run(
        &bundle(TWO_STAGE),
        &registry,
        Mode::Apply,
        &options(&tools),
        &mut Lines::default(),
    );
    assert!(!report.succeeded());
    let Outcome::Failed(why) = &report.stages[1].outcome else {
        panic!("{:?}", report.stages[1].outcome);
    };
    assert!(
        why.contains("cloud-init on 10.0.0.5 reports that the first boot failed"),
        "{why}"
    );
    registry.close().unwrap();
}

#[test]
fn a_setting_that_means_nothing_is_reported_and_the_check_can_be_turned_off() {
    let tools = tools("settings", "exit 1", "");
    let registry = registry(&[("MERIDIAN_CLOUD_INIT_WAIT", "telnet")]);
    let report = run(
        &bundle(SINGLE),
        &registry,
        Mode::Plan,
        &options(&tools),
        &mut Lines::default(),
    );
    let Outcome::Failed(why) = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert!(
        why.contains("MERIDIAN_CLOUD_INIT_WAIT is \"telnet\"; it can be \"ssh\" or \"off\""),
        "{why}"
    );
    registry.close().unwrap();

    // With the check off, a cloud-init that would reject everything is never
    // asked.
    let registry = self::registry(&[
        ("MERIDIAN_CLOUD_INIT", &tools.cloud_init),
        ("MERIDIAN_CLOUD_INIT_VALIDATE", "off"),
    ]);
    let report = run(
        &bundle(SINGLE),
        &registry,
        Mode::Apply,
        &options(&tools),
        &mut Lines::default(),
    );
    assert!(report.succeeded(), "{:?}", report.stages[0].outcome);
    assert!(!tools.dir.join("cloud-init.args").exists());
    registry.close().unwrap();
}
