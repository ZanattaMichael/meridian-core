//! The whole plugin across the process boundary: the host starts the real
//! binary, compiles the examples through it, and plans and applies with a
//! stand-in for `az`. Nothing may be lost or reworded on the way.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use meridian_plugin_host::{Client, EmitError, StartOptions};
use meridian_plugin_testkit::{
    assert_golden, graph_from_file, graph_from_text, plugin_binary, scratch, stand_in,
};
use meridian_sdk::{
    Capabilities, Category, ChangeAction, Data, Operations, RunContext, Value, PROTOCOL_VERSION,
};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Starts the plugin with `az` standing in as the script `body`.
fn start(dir: &Path, body: &str) -> Client {
    let az = stand_in(dir, "az", body);
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("MERIDIAN_AZ".to_owned(), az.to_str().unwrap().to_owned()));
    env.push((
        "MERIDIAN_AZ_ARGS".to_owned(),
        "--only-show-errors".to_owned(),
    ));
    env.push((
        "MERIDIAN_AZ_PARAMETERS".to_owned(),
        "@secrets.parameters.json".to_owned(),
    ));
    let binary = plugin_binary("meridian-target-arm");
    Client::start(
        binary.to_str().unwrap(),
        &StartOptions {
            env: Some(env),
            ..StartOptions::default()
        },
    )
    .expect("the plugin starts")
}

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

#[test]
fn the_plugin_introduces_itself() {
    let dir = scratch(TMP, "process-info");
    let client = start(&dir, "exit 0");
    let info = client.info();
    assert_eq!(info.name, "arm");
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.category, Category::InfrastructureAsCode);
    assert_eq!(info.capabilities, Capabilities::default());
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(
        info.resource_types,
        ["<Namespace>/<type>", "deployment", "parameter"]
    );
    client.close().expect("the plugin stops cleanly");
}

#[test]
fn every_example_compiles_to_its_golden_files_across_the_boundary() {
    let dir = scratch(TMP, "process-compile");
    let client = start(&dir, "exit 0");
    for example in ["web-vm", "sql-database", "landing-zone"] {
        let graph = graph_from_file(&format!("examples/arm/{example}.yaml"), "arm");
        let emitted = client
            .emit(&graph, &Data::new())
            .unwrap_or_else(|e| panic!("compiling {example}: {e}"));
        common::assert_valid_template(&emitted.artifact.files["template.json"]);
        assert_golden(&golden(), example, &emitted);
    }
}

#[test]
fn a_refusal_crosses_the_boundary_with_its_rule_and_position() {
    let dir = scratch(TMP, "process-refusal");
    let client = start(&dir, "exit 0");
    let graph = graph_from_text(
        "infra.yaml",
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: arm\n  resources:\n    - id: deployment\n      type: deployment\n      params: {resourceGroup: net-rg}\n    - id: nic\n      type: Microsoft.Network/networkInterfaces\n      params:\n        apiVersion: \"2023-09-01\"\n        tags: {a: \"${missing}\"}\n",
        "arm",
    );
    let Err(EmitError::Refused(refusal)) = client.emit(&graph, &Data::new()) else {
        panic!("the document should be refused");
    };
    assert_eq!(refusal.target, "arm");
    assert_eq!(refusal.rule, "invalid-reference");
    assert_eq!(refusal.resource, "nic");
    assert_eq!(refusal.pos.file, "infra.yaml");
    assert_eq!(refusal.pos.line, 11);
}

#[test]
fn a_warning_crosses_the_boundary() {
    let dir = scratch(TMP, "process-warning");
    let client = start(&dir, "exit 0");
    let graph = graph_from_text(
        "infra.yaml",
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: arm\n  resources:\n    - id: deployment\n      type: deployment\n      params: {resourceGroup: net-rg}\n    - id: password\n      type: parameter\n      params: {type: securestring, defaultValue: hunter2}\n    - id: logs\n      type: Microsoft.Storage/storageAccounts\n      params: {apiVersion: \"2023-01-01\"}\n",
        "arm",
    );
    let emitted = client.emit(&graph, &Data::new()).unwrap();
    assert_eq!(emitted.warnings.len(), 1);
    assert_eq!(emitted.warnings[0].target, "arm");
    assert_eq!(emitted.warnings[0].resource, "password");
}

/// The whole path a workflow takes: compile, plan, apply, read outputs.
#[test]
fn compile_plan_apply_and_outputs_run_end_to_end() {
    let dir = scratch(TMP, "process-run");
    let id = "/subscriptions/0000/resourceGroups/web-rg/providers";
    let body = format!(
        r#"echo "$@" >> "{dir}/calls"
# The template the plugin wrote is where az is told to find it.
test -f template.json || {{ echo "no template in $(pwd)" >&2; exit 2; }}
case "$3" in
what-if) echo "previewing" >&2
  echo '{{"status":"Succeeded","error":null,"changes":[
    {{"changeType":"Create","resourceId":"{id}/Microsoft.Compute/virtualMachines/web-vm"}},
    {{"changeType":"Create","resourceId":"{id}/Microsoft.Network/publicIPAddresses/web-ip"}}]}}' ;;
create) echo "deploying" >&2
  echo '{{"properties":{{"outputs":{{"web_vm_ip":{{"type":"String","value":"203.0.113.7"}},"web_url":{{"type":"String","value":"http://web.example/"}}}}}}}}' ;;
show) echo '{{"properties":{{"outputs":{{"web_vm_ip":{{"type":"String","value":"203.0.113.7"}}}}}}}}' ;;
esac"#,
        dir = dir.display()
    );
    let client = start(&dir, &body);
    let graph = graph_from_file("examples/arm/web-vm.yaml", "arm");
    let artifact = client.emit(&graph, &Data::new()).unwrap().artifact;

    let mut context = RunContext {
        document: "web-vm".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };
    // An earlier stage's output that is one of the template's parameters.
    context
        .inputs
        .insert("admin_password".into(), Value::String("hunter2".into()));

    let mut lines = Vec::new();
    let plan = client
        .plan(&context, &artifact, &mut |line| lines.push(line.text))
        .unwrap_or_else(|e| panic!("planning: {e}"));
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            "create web_vm: Microsoft.Compute/virtualMachines",
            "create web_ip: Microsoft.Network/publicIPAddresses",
        ]
    );
    // The plugin's own line and what az printed both reach the host.
    assert_eq!(
        lines,
        [
            "deployment web-vm-prod: asking Resource Manager what would change in resource group web-rg",
            "previewing",
        ]
    );

    let applied = client
        .apply(&context, &artifact, &mut |_| {})
        .unwrap_or_else(|e| panic!("applying: {e}"));
    assert_eq!(applied.changes.len(), 2);
    assert!(applied
        .changes
        .iter()
        .all(|c| c.action == ChangeAction::Create));
    let outputs: Vec<(&str, &Value)> = applied
        .outputs
        .iter()
        .map(|o| (o.name.as_str(), &o.value))
        .collect();
    assert_eq!(
        outputs,
        [
            ("web_url", &Value::String("http://web.example/".into())),
            ("web_vm_ip", &Value::String("203.0.113.7".into())),
        ]
    );

    let outputs = client
        .outputs(&context)
        .unwrap_or_else(|e| panic!("outputs: {e}"));
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].name, "web_vm_ip");

    let common = "--name web-vm-prod --resource-group web-rg --only-show-errors --output json";
    let deploy = "--template-file template.json --mode Incremental --parameters @secrets.parameters.json --parameters admin_password=\"hunter2\"";
    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            format!("deployment group what-if {common} {deploy} --no-pretty-print"),
            format!("deployment group what-if {common} {deploy} --no-pretty-print"),
            format!("deployment group create {common} {deploy}"),
            format!("deployment group show {common}"),
        ]
    );
    // What az deployed is byte for byte what was compiled.
    assert_eq!(
        fs::read_to_string(dir.join("work/template.json")).unwrap(),
        artifact.files["template.json"]
    );
}
