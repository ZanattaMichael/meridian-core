//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_in_param};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_bicep::Bicep;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Bicep.emit(graph, &Data::new())
}

const TARGET: &str =
    "    - id: target\n      type: deployment\n      params: {resourceGroup: rg}\n";
const STORAGE: &str = "    - id: store\n      type: Microsoft.Storage/storageAccounts@2023-01-01\n      params:\n        name: logs\n        location: westeurope\n";

fn compile(resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: bicep\n  resources:\n{resources}"
    );
    emit(&graph_from_text("infra.yaml", &text, "bicep"))
}

fn bicep(resources: &str) -> String {
    compile(resources).unwrap().artifact.files["main.bicep"].clone()
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

#[test]
fn the_example_compiles_to_its_golden_file() {
    let graph = graph_from_file("examples/bicep/web-vm.yaml", "bicep");
    assert_golden(&golden(), "web-vm", &emit(&graph).unwrap());
}

#[test]
fn values_are_written_as_bicep_literals() {
    let text = bicep(&format!(
        "{TARGET}    - id: site\n      type: Microsoft.Web/sites@2022-09-01\n      params:\n        name: it's\n        properties:\n          enabled: true\n          count: 3\n          ratio: 1.5\n          whole: 2.0\n          nothing: null\n          empty: []\n          none: {{}}\n          script: \"echo $${{HOME}}\\n\"\n"
    ));
    assert!(text.contains("name: 'it\\'s'"), "{text}");
    assert!(text.contains("count: 3\n"), "{text}");
    assert!(text.contains("ratio: json('1.5')"), "{text}");
    assert!(text.contains("whole: 2\n"), "{text}");
    assert!(text.contains("nothing: null"), "{text}");
    assert!(text.contains("empty: []"), "{text}");
    assert!(text.contains("none: {}"), "{text}");
    assert!(text.contains("script: 'echo \\${HOME}\\n'"), "{text}");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(&format!(
        "{TARGET}{STORAGE}    - id: site\n      type: Microsoft.Web/sites@2022-09-01\n      params:\n        name: site\n        storage: ${{stor.id}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(
        err.msg,
        "param \"storage\": ${stor.id} refers to \"stor\", which is not a resource in this document"
    );
}

#[test]
fn a_function_call_that_mentions_a_resource_still_orders_after_it() {
    let text = bicep(&format!(
        "{TARGET}{STORAGE}    - id: site\n      type: Microsoft.Web/sites@2022-09-01\n      params:\n        name: site\n        key: ${{store.listKeys().keys[0].value}}\n        upper: ${{toUpper(store.name)}}\n      dependsOn:\n        - resource: store\n"
    ));
    assert!(text.contains("upper: toUpper(store.name)"), "{text}");
    assert!(!text.contains("dependsOn"), "{text}");
}

#[test]
fn the_document_must_say_where_it_deploys() {
    let err = refusal(STORAGE);
    assert_eq!(err.rule, "missing-resource");
    assert!(
        err.msg.contains("resource of type \"deployment\""),
        "{}",
        err.msg
    );
}

#[test]
fn a_resource_needs_its_azure_name_and_a_bicep_safe_id() {
    let err = refusal(&format!(
        "{TARGET}    - id: store\n      type: Microsoft.Storage/storageAccounts@2023-01-01\n      params: {{location: westeurope}}\n"
    ));
    assert_eq!(err.rule, "missing-param");

    let err = refusal(&format!(
        "{TARGET}    - id: my-store\n      type: Microsoft.Storage/storageAccounts@2023-01-01\n      params: {{name: logs}}\n"
    ));
    assert_eq!(err.rule, "invalid-name");
}

#[test]
fn a_type_without_its_api_version_is_refused() {
    let err = refusal(&format!(
        "{TARGET}    - id: store\n      type: Microsoft.Storage/storageAccounts\n      params: {{name: logs}}\n"
    ));
    assert_eq!(err.rule, "unsupported-resource-type");
    assert!(err.msg.contains("with its API version"), "{}", err.msg);
}

#[test]
fn notify_absent_and_runtime_conditions_are_refused() {
    let err = refusal(&format!(
        "{TARGET}{STORAGE}    - id: site\n      type: Microsoft.Web/sites@2022-09-01\n      params: {{name: site}}\n      notifies:\n        - resource: store\n          action: restart\n"
    ));
    assert_eq!(err.rule, "unsupported-action");

    let err = refusal(&format!(
        "{TARGET}    - id: store\n      type: Microsoft.Storage/storageAccounts@2023-01-01\n      state: absent\n      params: {{name: logs}}\n"
    ));
    assert_eq!(err.rule, "unsupported-state");
    assert!(err.msg.contains("never deletes one"), "{}", err.msg);
}

// ---- secrets ------------------------------------------------------------

#[test]
fn a_secret_in_a_param_is_a_secure_parameter_the_runner_fills_from_a_file() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: bicep\n  resources:\n{TARGET}{STORAGE}        tags: placeholder\n"
    );
    let mut graph = graph_from_text("infra.yaml", &text, "bicep");
    secret_in_param(&mut graph, "store", "tags", "db_password");
    let emitted = emit(&graph).unwrap();
    let bicep = &emitted.artifact.files["main.bicep"];
    assert!(
        bicep.contains("\n@secure()\nparam meridian_secret_db_password string\n"),
        "{bicep}"
    );
    assert!(
        bicep.contains("tags: meridian_secret_db_password\n"),
        "{bicep}"
    );
    assert!(!bicep.contains("(secret "), "{bicep}");
    // A parameter is nothing to order after.
    assert!(!bicep.contains("dependsOn"), "{bicep}");
    assert_eq!(
        meridian_target_bicep::secret_parameter("db_password"),
        "meridian_secret_db_password"
    );
    assert_eq!(
        meridian_target_bicep::secret_reference("db_password"),
        "${meridian_secret_db_password}"
    );
    assert!(Bicep.capabilities().secrets);
}

#[test]
fn a_secret_in_the_deployments_settings_is_refused_because_they_are_the_command_line() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: bicep\n  resources:\n{TARGET}{STORAGE}"
    );
    let mut graph = graph_from_text("infra.yaml", &text, "bicep");
    secret_in_param(&mut graph, "target", "resourceGroup", "db_password");
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-secret-place");
    assert!(
        err.msg
            .starts_with("param \"resourceGroup\" of the deployment holds a secret"),
        "{}",
        err.msg
    );
}

#[test]
fn the_common_types_compile_to_their_golden_file() {
    let graph = graph_from_file("examples/common/web-bicep.yaml", "bicep");
    assert_golden(&golden(), "common-web-bicep", &emit(&graph).unwrap());
}

#[test]
fn a_common_resource_without_a_location_is_at_the_resource_groups() {
    let text = bicep(&format!(
        "{TARGET}    - id: ip\n      type: address\n    - id: ip2\n      type: address\n      params: {{location: northeurope, tags: {{team: web}}}}\n"
    ));
    assert!(
        text.contains("location: resourceGroup().location"),
        "{text}"
    );
    assert!(text.contains("location: 'northeurope'"), "{text}");
    assert!(text.contains("team: 'web'"), "{text}");
}

#[test]
fn an_azure_vm_without_a_key_is_refused() {
    let err = refusal(&format!(
        "{TARGET}    - id: net\n      type: network\n      params: {{cidr: 10.0.0.0/16}}\n    - id: sub\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24}}\n    - id: vm\n      type: vm\n      params: {{subnet: \"${{sub}}\", size: Standard_B1s, image: img}}\n"
    ));
    assert_eq!(err.rule, "missing-param");
    assert_eq!(err.target, "bicep");
    assert_eq!(err.resource, "vm");
}

#[test]
fn a_labs_tags_are_left_off_an_azure_subnet_and_an_authors_are_refused() {
    let net = "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16, tags: {meridian-lab-run: l0000abcd}}\n";
    let text = bicep(&format!(
        "{TARGET}{net}    - id: s\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24, tags: {{meridian-lab-run: l0000abcd}}}}\n"
    ));
    assert_eq!(
        text.matches("'meridian-lab-run': 'l0000abcd'").count(),
        1,
        "{text}"
    );
    let err = refusal(&format!(
        "{TARGET}{net}    - id: s\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24, tags: {{team: web}}}}\n"
    ));
    assert_eq!(err.rule, "unsupported-param");
}
