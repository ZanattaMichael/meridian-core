//! A whole workflow against a real `tofu`: a document compiled by the
//! cloud-init plugin is imported by the document OpenTofu applies, and
//! arrives in OpenTofu as a variable.
//!
//! This is the hand-off of issue 47 with real plugins on both sides. It
//! creates nothing outside its scratch directory: the value lands in a
//! `terraform_data` resource, which is built into OpenTofu.
//!
//! Like `tests/real.rs`, it runs when `MERIDIAN_TEST_REAL_TOFU` names the
//! program and passes without doing anything otherwise.

#![cfg(unix)]

use meridian_ir::Bundle;
use meridian_orchestrator::{run, Mode, Options, Outcome, Quiet};
use meridian_plugin_host::{Registry, StartOptions};
use meridian_plugin_testkit::{plugin_binary, scratch};
use meridian_sdk::Value;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: machine
spec:
  target: opentofu
  imports:
    - name: user_data
      from: artifact
      ref: bootstrap
      file: user-data
  resources:
    - id: user_data
      type: variable
      params:
        type: string
    - id: holder
      type: terraform_data
      params:
        input: ${user_data}
  outputs:
    - name: received
      value: ${holder.output}
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: bootstrap
spec:
  target: cloud-init
  targetHost:
    from: inline
    host: web-1
  resources:
    - id: motd
      type: file
      params:
        path: /etc/motd
        content: MOTD
"#;

fn workflow(motd: &str) -> Bundle {
    let text = WORKFLOW.replace("MOTD", motd);
    let bundle = Bundle::new(meridian_ir::parse("site.yaml", text.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn received(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Applied { outputs, .. } => {
            match &outputs.iter().find(|o| o.name == "received").unwrap().value {
                Value::String(text) => text.clone(),
                other => panic!("received is {other:?}"),
            }
        }
        other => panic!("the machine is {other:?}"),
    }
}

#[ignore = "needs a real tofu; see docs/contributing/real-tool-tests.md"]
#[test]
fn user_data_compiled_by_one_plugin_is_a_variable_in_the_tool_of_another() {
    let Some(program) = std::env::var("MERIDIAN_TEST_REAL_TOFU")
        .ok()
        .filter(|program| !program.trim().is_empty())
    else {
        eprintln!("skipped: MERIDIAN_TEST_REAL_TOFU is not set");
        return;
    };
    let dir = scratch(TMP, "real-imports");

    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("TF_VAR_"))
        .collect();
    env.push(("MERIDIAN_TOFU".to_owned(), program));
    // The cloud-init plugin only compiles here, which needs no cloud-init.
    env.push(("MERIDIAN_CLOUD_INIT_VALIDATE".to_owned(), "off".to_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let mut registry = Registry::new();
    for package in ["meridian-target-opentofu", "meridian-target-cloud-init"] {
        registry
            .load(&plugin_binary(package).to_string_lossy(), &options)
            .unwrap_or_else(|e| panic!("{package} loads: {e}"));
    }

    let run_options = Options::new(dir.join("state"));
    let first = run(
        &workflow("first boot"),
        &registry,
        Mode::Apply,
        &run_options,
        &mut Quiet,
    );
    assert!(first.succeeded(), "{first:?}");
    // The machine is written first, and runs after what it imports from.
    let order: Vec<&str> = first.stages.iter().map(|s| s.id.name.as_str()).collect();
    assert_eq!(order, ["bootstrap", "machine"]);
    let value = received(&first.stages[1].outcome);
    assert!(value.starts_with("#cloud-config"), "{value}");
    assert!(value.contains("/etc/motd"), "{value}");
    assert!(value.contains("first boot"), "{value}");

    // A change to the bootstrap document is a change to the machine.
    let plan = run(
        &workflow("second boot"),
        &registry,
        Mode::Plan,
        &run_options,
        &mut Quiet,
    );
    match &plan.stages[1].outcome {
        Outcome::Planned(plan) => assert!(!plan.changes.is_empty(), "{plan:?}"),
        other => panic!("the machine is {other:?}"),
    }
    let second = run(
        &workflow("second boot"),
        &registry,
        Mode::Apply,
        &run_options,
        &mut Quiet,
    );
    assert!(second.succeeded(), "{second:?}");
    assert!(received(&second.stages[1].outcome).contains("second boot"));
    registry.close().unwrap();
}
