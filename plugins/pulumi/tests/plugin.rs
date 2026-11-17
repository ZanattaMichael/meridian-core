//! The plugin as the core meets it: the real binary, in its own process,
//! driven by the host over the plugin protocol.
//!
//! The other tests call the library directly. This one proves nothing is lost
//! or reworded on the way across the boundary, and that the binary is wired
//! to the runner and reads its settings from the environment.

use std::fs;
use std::path::{Path, PathBuf};

use meridian_plugin_host::{Client, EmitError, RunFailure, StartOptions};
use meridian_plugin_testkit::{
    assert_golden, graph_from_file, graph_from_text, plugin_binary, scratch, stand_in,
};
use meridian_sdk::{Capabilities, Category, Data, Operations, RunContext, Value, PROTOCOL_VERSION};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

const STAND_IN: &str = r#"echo "$@" >> "$CALLS"
case "$1 $2" in
"stack select") echo "Created stack 'dev'" ;;
"preview "*) echo '{"steps":[{"op":"create","urn":"urn:pulumi:dev::pets::random:index/randomPet:RandomPet::pet","newState":{"type":"random:index/randomPet:RandomPet"}}]}' ;;
"up "*) echo '    + 1 created' ;;
"stack output") case "$*" in
  *--show-secrets*) echo '{"pet_name":"web-wise-cat","password":"hunter2"}' ;;
  *) echo '{"pet_name":"web-wise-cat","password":"[secret]"}' ;;
  esac ;;
esac"#;

/// Starts the plugin binary with a stand-in for `pulumi` named through the
/// variable an operator would use.
fn start(dir: &Path) -> Client {
    let program = stand_in(dir, "pulumi", STAND_IN);
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("PULUMI_") && key != "MERIDIAN_PULUMI")
        .collect();
    env.push((
        "MERIDIAN_PULUMI".to_owned(),
        program.to_str().unwrap().to_owned(),
    ));
    env.push((
        "CALLS".to_owned(),
        dir.join("calls").to_str().unwrap().to_owned(),
    ));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let binary = plugin_binary("meridian-target-pulumi");
    Client::start(binary.to_str().unwrap(), &options).expect("the plugin starts")
}

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "pets".into(),
        work_dir: dir.join("work"),
        inputs: Data::from([("prefix".to_owned(), Value::String("db".into()))]),
        ..RunContext::default()
    }
}

#[test]
fn the_binary_reports_what_the_plugin_is() {
    let dir = scratch(TMP, "plugin-info");
    let client = start(&dir);
    let info = client.info();
    assert_eq!(info.name, "pulumi");
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.category, Category::InfrastructureAsCode);
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(info.capabilities, Capabilities::default());
    assert_eq!(
        info.resource_types,
        [
            "<package>:<module>:<Type>",
            "config",
            "invoke.<function>",
            "stack"
        ]
    );
    client.close().expect("the plugin shuts down cleanly");
}

#[test]
fn the_examples_compile_across_the_boundary_to_the_same_bytes() {
    let dir = scratch(TMP, "plugin-compile");
    let client = start(&dir);
    for case in ["web-vm", "pets"] {
        let graph = graph_from_file(&format!("examples/pulumi/{case}.yaml"), "pulumi");
        let emitted = client.emit(&graph, &Data::new()).expect("it compiles");
        assert_golden(&golden(), case, &emitted);
    }
}

#[test]
fn a_refusal_crosses_the_boundary_with_its_rule_and_position() {
    let dir = scratch(TMP, "plugin-refusal");
    let client = start(&dir);
    let graph = graph_from_text(
        "infra.yaml",
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: pulumi\n  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        subnetId: ${subnett.id}\n",
        "pulumi",
    );
    let Err(EmitError::Refused(err)) = client.emit(&graph, &Data::new()) else {
        panic!("the document should be refused, not fail");
    };
    assert_eq!(err.target, "pulumi");
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "vm");
    assert_eq!(err.pos.file, "infra.yaml");
    assert_eq!(err.pos.line, 8);
}

#[test]
fn the_host_plans_applies_and_reads_outputs_through_the_binary() {
    let dir = scratch(TMP, "plugin-run");
    let client = start(&dir);
    let graph = graph_from_file("examples/pulumi/pets.yaml", "pulumi");
    let artifact = client.emit(&graph, &Data::new()).unwrap().artifact;
    let context = context(&dir);

    let mut lines = Vec::new();
    let plan = client
        .plan(&context, &artifact, &mut |line| lines.push(line.text))
        .unwrap_or_else(|e| panic!("planning: {e}"));
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(
        plan.changes[0].to_string(),
        "create pet: random:index/randomPet:RandomPet"
    );
    assert_eq!(
        lines,
        [
            "Created stack 'dev'",
            "stack dev: asking Pulumi what would change"
        ]
    );
    // What the plugin wrote is what it compiled.
    assert_eq!(
        fs::read_to_string(dir.join("work/Pulumi.yaml")).unwrap(),
        artifact.files["Pulumi.yaml"]
    );

    let applied = client
        .apply(&context, &artifact, &mut |_| {})
        .unwrap_or_else(|e| panic!("applying: {e}"));
    let masked = applied
        .outputs
        .iter()
        .find(|o| o.name == "password")
        .unwrap();
    assert!(masked.sensitive, "sensitivity survives the wire");
    assert!(masked.value == Value::String("hunter2".into()));

    let outputs = client.outputs(&context).unwrap();
    assert_eq!(outputs, applied.outputs);

    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    let common = " --non-interactive --color never";
    assert_eq!(
        calls
            .lines()
            .map(|l| l.replace(common, ""))
            .collect::<Vec<_>>(),
        [
            "stack select dev --create",
            "preview --stack dev --json --config prefix=db",
            "stack select dev --create",
            "preview --stack dev --json --config prefix=db",
            "up --stack dev --yes --skip-preview --config prefix=db",
            "stack output --stack dev --json",
            "stack output --stack dev --json --show-secrets",
            "stack output --stack dev --json",
            "stack output --stack dev --json --show-secrets",
        ]
    );
}

#[test]
fn a_failure_of_the_tool_is_an_answer_and_not_a_fault() {
    let dir = scratch(TMP, "plugin-failure");
    let client = start(&dir);
    let foreign = meridian_plugin_testkit::artifact(&[("main.tf.json", "{}")]);
    let Err(RunFailure::Tool(err)) = client.plan(&context(&dir), &foreign, &mut |_| {}) else {
        panic!("a foreign artifact is the tool's answer, not a plugin fault");
    };
    assert!(err.msg.contains("was not compiled by this plugin"));
}
