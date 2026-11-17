//! The engine against a real `opa`, through a whole Meridian run.
//!
//! These need Open Policy Agent, so they do nothing unless
//! `MERIDIAN_TEST_REAL_OPA` names its program. The `opa` workflow runs them.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use meridian_ir::Bundle;
use meridian_orchestrator::{
    run, work_dir, Mode, Options, Outcome, Policy, Quiet, RunReport, StageId, StageKind,
    REASON_POLICY_DENIED,
};
use meridian_plugin_host::{Registry, StartOptions};

const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: site
  labels: LABELS
spec:
  target: fake
  resources:
RESOURCES
"#;

fn workflow(labels: &str, files: &[(&str, &str)]) -> Bundle {
    let resources: String = files
        .iter()
        .map(|(id, content)| {
            format!("    - id: {id}\n      type: file\n      params:\n        content: {content}\n")
        })
        .collect();
    let text = WORKFLOW
        .replace("LABELS", labels)
        .replace("RESOURCES", resources.trim_end());
    let bundle = Bundle::new(meridian_ir::parse("site.yaml", text.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let dir = meridian_target_fake::plugin_dir();
    registry
        .discover(dir.to_str().unwrap(), &StartOptions::default())
        .expect("the fake plugin loads");
    registry
}

fn site() -> StageId {
    StageId {
        kind: StageKind::Infrastructure,
        name: "site".to_owned(),
    }
}

/// The run's options with this policy, or `None` when there is no `opa` to
/// test against.
fn options(test: &str, policy: &Path) -> Option<Options> {
    let opa = std::env::var("MERIDIAN_TEST_REAL_OPA")
        .ok()
        .filter(|v| !v.is_empty())?;
    // The engine reads MERIDIAN_OPA; every test sets it to the same value.
    std::env::set_var("MERIDIAN_OPA", opa);
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("real")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    let engine = Path::new(env!("CARGO_BIN_EXE_meridian-policy-opa"));
    let policy = Policy::find(
        "opa",
        &[engine.parent().unwrap().to_path_buf()],
        vec![policy.to_path_buf()],
    )
    .unwrap();
    Some(Options {
        policy: Some(policy),
        ..Options::new(dir.join("state"))
    })
}

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/policy")
}

/// A policy of its own, written into the test's scratch directory.
fn written(test: &str, rego: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("real-policies")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("policy.rego");
    fs::write(&file, rego).unwrap();
    file
}

fn on_disk(options: &Options, file: &str) -> Option<String> {
    fs::read_to_string(
        work_dir(&options.state_dir, &site(), "fake")
            .join("current/files")
            .join(file),
    )
    .ok()
}

fn failed_with(report: &RunReport) -> &str {
    match &report.stages[0].outcome {
        Outcome::Failed(why) => why,
        other => panic!("the stage is {other:?}"),
    }
}

#[ignore = "needs a real opa; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_example_policy_allows_a_create_and_denies_a_delete_in_prod() {
    let Some(options) = options("example", &example()) else {
        return;
    };
    let registry = registry();
    let both = workflow("{env: prod}", &[("index", "hello"), ("motd", "welcome")]);
    let first = run(&both, &registry, Mode::Apply, &options, &mut Quiet);
    assert!(first.succeeded(), "{first:?}");
    let decisions = first.stages[0].policy.as_ref().unwrap();
    assert!(decisions.asked && decisions.allowed() && decisions.warn.is_empty());

    // Dropping a resource from the document deletes it, which the policy
    // does not allow in prod: nothing is applied.
    let one = workflow("{env: prod}", &[("index", "changed")]);
    let second = run(&one, &registry, Mode::Apply, &options, &mut Quiet);
    assert!(!second.succeeded());
    assert_eq!(second.stages[0].reason, REASON_POLICY_DENIED);
    let why = failed_with(&second);
    assert!(why.contains("would be a delete in prod"), "{why}");
    assert_eq!(on_disk(&options, "index").as_deref(), Some("hello"));
    assert_eq!(on_disk(&options, "motd").as_deref(), Some("welcome"));

    // A plan says the same, and changes nothing either.
    let plan = run(&one, &registry, Mode::Plan, &options, &mut Quiet);
    assert!(failed_with(&plan).contains("would be a delete in prod"));
}

#[ignore = "needs a real opa; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_example_policy_warns_about_a_stage_with_no_environment() {
    let Some(options) = options("warn", &example()) else {
        return;
    };
    let registry = registry();
    let report = run(
        &workflow("{team: web}", &[("index", "hello")]),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(report.succeeded(), "{report:?}");
    let decisions = report.stages[0].policy.as_ref().unwrap();
    assert_eq!(decisions.warn.len(), 1, "{decisions:?}");
    assert!(
        decisions.warn[0].contains("site has no env label"),
        "{decisions:?}"
    );
    assert_eq!(on_disk(&options, "index").as_deref(), Some("hello"));
}

#[ignore = "needs a real opa; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_policy_reads_the_resources_and_may_answer_with_objects() {
    let policy = written(
        "objects",
        r#"package meridian

import rego.v1

deny contains {"msg": msg} if {
	some resource in input.resources
	contains(resource.params.content, "password")
	msg := sprintf("%s holds a password", [resource.id])
}
"#,
    );
    let Some(options) = options("objects", &policy) else {
        return;
    };
    let registry = registry();
    let report = run(
        &workflow("{env: dev}", &[("index", "password=x")]),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert_eq!(report.stages[0].reason, REASON_POLICY_DENIED);
    assert!(failed_with(&report).contains("index holds a password"));
    assert_eq!(on_disk(&options, "index"), None);
}

#[ignore = "needs a real opa; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_policy_that_does_not_parse_or_names_another_package_is_a_denial() {
    let broken = written("broken", "package meridian\n\ndeny contains msg if {\n");
    let Some(options_broken) = options("broken", &broken) else {
        return;
    };
    let registry = registry();
    let site_document = workflow("{env: dev}", &[("index", "hello")]);
    let report = run(
        &site_document,
        &registry,
        Mode::Apply,
        &options_broken,
        &mut Quiet,
    );
    assert_eq!(report.stages[0].reason, REASON_POLICY_DENIED);
    let why = failed_with(&report);
    assert!(why.contains("could not be asked"), "{why}");
    assert!(why.contains("eval failed"), "{why}");
    assert!(!report.stages[0].policy.as_ref().unwrap().asked);
    assert_eq!(on_disk(&options_broken, "index"), None);

    let elsewhere = written(
        "elsewhere",
        "package other\n\nimport rego.v1\n\ndeny contains \"never\" if false\n",
    );
    let options_elsewhere = options("elsewhere", &elsewhere).unwrap();
    let report = run(
        &site_document,
        &registry,
        Mode::Apply,
        &options_elsewhere,
        &mut Quiet,
    );
    assert_eq!(report.stages[0].reason, REASON_POLICY_DENIED);
    let why = failed_with(&report);
    assert!(
        why.contains("define nothing in the package `meridian`"),
        "{why}"
    );
    assert_eq!(on_disk(&options_elsewhere, "index"), None);
}
