//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

mod common;

use std::path::{Path, PathBuf};

use common::assert_valid_template;
use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_in_param};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_arm::Arm;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Arm.emit(graph, &Data::new())
}

fn compile_named(name: &str, spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: {name}\nspec:\n  target: arm\n{spec}"
    );
    emit(&graph_from_text("infra.yaml", &text, "arm"))
}

/// A document's resources, after the deployment settings every one needs.
fn spec(resources: &str) -> String {
    format!("  resources:\n{GROUP}{resources}")
}

fn template(resources: &str) -> String {
    let text = compile_named("net", &spec(resources))
        .unwrap()
        .artifact
        .files["template.json"]
        .clone();
    assert_valid_template(&text);
    text
}

fn refusal(resources: &str) -> CompileError {
    compile_named("net", &spec(resources)).expect_err("the document should be refused")
}

const GROUP: &str =
    "    - id: deployment\n      type: deployment\n      params: {resourceGroup: net-rg}\n";

const VNET: &str = "    - id: vnet\n      type: Microsoft.Network/virtualNetworks\n      params:\n        apiVersion: \"2023-09-01\"\n        name: web-vnet\n";

/// A network interface whose params are given by the caller.
fn nic(params: &str) -> String {
    format!(
        "    - id: nic\n      type: Microsoft.Network/networkInterfaces\n      params:\n        apiVersion: \"2023-09-01\"\n{params}"
    )
}

const VNET_ID: &str = "resourceId('Microsoft.Network/virtualNetworks', 'web-vnet')";

// ---- golden files ----

fn example(file: &str, case: &str) -> Emitted {
    let graph = graph_from_file(&format!("examples/arm/{file}.yaml"), "arm");
    let emitted = emit(&graph).unwrap();
    let text = &emitted.artifact.files["template.json"];
    assert_valid_template(text);
    assert!(!text.contains("${"), "a reference was left in {text}");
    assert_golden(&golden(), case, &emitted);
    emitted
}

#[test]
fn the_virtual_machine_example_compiles_to_its_golden_template() {
    let emitted = example("web-vm", "web-vm");
    assert!(emitted.warnings.is_empty());
}

#[test]
fn the_database_example_compiles_child_and_extension_resources() {
    example("sql-database", "sql-database");
}

#[test]
fn the_landing_zone_example_compiles_for_a_subscription() {
    let emitted = example("landing-zone", "landing-zone");
    let text = &emitted.artifact.files["template.json"];
    assert!(
        text.contains("subscriptionDeploymentTemplate.json#"),
        "{text}"
    );
    assert!(!text.contains("[resourceId("), "{text}");
}

#[test]
fn compiling_is_deterministic() {
    let graph = graph_from_file("examples/arm/web-vm.yaml", "arm");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

/// The first fenced block of a language in the user guide.
fn guide_block(language: &str) -> String {
    let guide = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/targets/arm.md"),
    )
    .expect("the guide exists");
    let fence = format!("```{language}\n");
    let start = guide.find(&fence).expect("the guide has such a block") + fence.len();
    let end = guide[start..].find("```").expect("the block is closed");
    guide[start..start + end].to_owned()
}

#[test]
fn the_guides_first_document_compiles_to_what_the_guide_shows() {
    let emitted = emit(&graph_from_text("guide.yaml", &guide_block("yaml"), "arm")).unwrap();
    assert_eq!(emitted.artifact.files["template.json"], guide_block("json"));
}

#[test]
fn an_unquoted_api_version_is_still_text() {
    let text = template(
        "    - id: logs\n      type: Microsoft.Storage/storageAccounts\n      params: {apiVersion: 2023-01-01}\n",
    );
    assert!(text.contains("\"apiVersion\": \"2023-01-01\""), "{text}");
}

// ---- references ----

#[test]
fn a_reference_becomes_the_expression_that_reads_it() {
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n        tags:\n          id: ${vnet}\n          also: ${vnet.id}\n          name: ${vnet.name}\n          type: ${vnet.type}\n          api: ${vnet.apiVersion}\n        properties:\n          space: ${vnet.properties.addressSpace.addressPrefixes[0]}\n          all: ${vnet.properties}\n          location: ${vnet.location}\n          sku: ${vnet.sku.name}\n")
    ));
    for want in [
        format!("\"id\": \"[{VNET_ID}]\""),
        format!("\"also\": \"[{VNET_ID}]\""),
        // What is known when compiling is written out, not computed.
        "\"name\": \"web-vnet\"".to_owned(),
        "\"type\": \"Microsoft.Network/virtualNetworks\"".to_owned(),
        "\"api\": \"2023-09-01\"".to_owned(),
        format!(
            "\"space\": \"[reference({VNET_ID}, '2023-09-01').addressSpace.addressPrefixes[0]]\""
        ),
        format!("\"all\": \"[reference({VNET_ID}, '2023-09-01')]\""),
        format!("\"location\": \"[reference({VNET_ID}, '2023-09-01', 'Full').location]\""),
        format!("\"sku\": \"[reference({VNET_ID}, '2023-09-01', 'Full').sku.name]\""),
    ] {
        assert!(text.contains(&want), "missing {want}\n{text}");
    }
}

#[test]
fn every_reference_is_a_dependency_because_arm_infers_none() {
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n        tags: {network: \"${vnet.name}\"}\n")
    ));
    assert!(
        text.contains(&format!(
            "\"dependsOn\": [\n        \"[{VNET_ID}]\"\n      ]"
        )),
        "{text}"
    );
    // Declared once, even when both a reference and a dependsOn ask for it.
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n        tags: {network: \"${vnet}\"}\n      dependsOn:\n        - resource: vnet\n")
    ));
    assert_eq!(text.matches("dependsOn").count(), 1, "{text}");
    assert_eq!(
        text.matches(&format!("\"[{VNET_ID}]\"")).count(),
        2,
        "{text}"
    );
}

#[test]
fn a_dependency_with_no_reference_is_still_written() {
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n      dependsOn:\n        - resource: vnet\n")
    ));
    assert!(text.contains(&format!("\"[{VNET_ID}]\"")), "{text}");
}

#[test]
fn references_in_text_become_one_format() {
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n        tags:\n          path: ${vnet}/subnets/${vnet.name}\n          quoted: \"it's {here} $${not} ${vnet.name}\"\n")
    ));
    assert!(
        text.contains(&format!(
            "\"path\": \"[format('{{0}}/subnets/{{1}}', {VNET_ID}, 'web-vnet')]\""
        )),
        "{text}"
    );
    // A quote is doubled and a brace is doubled, as format() wants them, and
    // the escaped reference is literal text.
    assert!(
        text.contains("\"quoted\": \"[format('it''s {{here}} ${{not}} {0}', 'web-vnet')]\""),
        "{text}"
    );
}

#[test]
fn a_reference_inside_an_expression_is_put_in_place() {
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: \"[concat(${vnet.name}, '-nic')]\"\n        properties:\n          keys: \"[listKeys(${vnet}, ${vnet.apiVersion}).keys[0].value]\"\n")
    ));
    assert!(
        text.contains("\"name\": \"[concat('web-vnet', '-nic')]\""),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "\"keys\": \"[listKeys({VNET_ID}, '2023-09-01').keys[0].value]\""
        )),
        "{text}"
    );
}

#[test]
fn an_expression_and_escaped_text_pass_through_untouched() {
    let text = template(&nic(
        "        name: web-nic\n        location: \"[resourceGroup().location]\"\n        tags:\n          literal: \"[[not an expression]\"\n          dollars: cost is $${5}\n",
    ));
    assert!(
        text.contains("\"location\": \"[resourceGroup().location]\""),
        "{text}"
    );
    assert!(
        text.contains("\"literal\": \"[[not an expression]\""),
        "{text}"
    );
    assert!(text.contains("\"dollars\": \"cost is ${5}\""), "{text}");
}

#[test]
fn a_value_the_core_placed_from_data_is_never_an_expression() {
    // Issue 136: a value placed from data that opens with `[` reaches the
    // plugin opening with `[[`, which the plugin declared as the escape.
    let declared = Arm.placed_text();
    assert_eq!(declared.len(), 1);
    assert_eq!(
        (
            declared[0].text.as_str(),
            declared[0].written.as_str(),
            declared[0].only_at_start
        ),
        ("[", "[[", true)
    );
    // Where Resource Manager would otherwise read an expression, the escape
    // is kept; where it could not, it is dropped, so the deployed value is
    // the literal either way.
    let text = template(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n        location: \"[[resourceGroup().id]\"\n        tags:\n          open: \"[[not closed\"\n          list: [\"[[listKeys(x)]\"]\n          joined: \"[[x] ${vnet.name}\"\n")
    ));
    assert!(
        text.contains("\"location\": \"[[resourceGroup().id]\""),
        "{text}"
    );
    assert!(text.contains("\"open\": \"[not closed\""), "{text}");
    assert!(text.contains("\"[[listKeys(x)]\""), "{text}");
    assert!(
        text.contains("\"joined\": \"[format('[x] {0}', 'web-vnet')]\""),
        "{text}"
    );
}

#[test]
fn a_name_left_out_is_the_id() {
    let text = template(
        "    - id: logs\n      type: Microsoft.Storage/storageAccounts\n      params: {apiVersion: \"2023-01-01\"}\n",
    );
    assert!(text.contains("\"name\": \"logs\""), "{text}");
}

#[test]
fn a_parameter_is_declared_and_read() {
    let text = template(&format!(
        "    - id: names\n      type: parameter\n      params: {{type: array, defaultValue: [a, b]}}\n{}",
        nic("        name: ${names[0]}\n        tags: {second: \"${names[1]}\", both: \"${names}\"}\n")
    ));
    assert!(
        text.contains("\"name\": \"[parameters('names')[0]]\""),
        "{text}"
    );
    assert!(
        text.contains("\"second\": \"[parameters('names')[1]]\""),
        "{text}"
    );
    assert!(
        text.contains("\"both\": \"[parameters('names')]\""),
        "{text}"
    );
    assert!(
        text.contains("\"parameters\": {\n    \"names\": {\n      \"defaultValue\": [\n        \"a\",\n        \"b\"\n      ],\n      \"type\": \"array\"\n    }\n  }"),
        "{text}"
    );
}

#[test]
fn a_child_whose_name_is_only_known_when_deploying_is_split_there() {
    let text = template(
        "    - id: prefix\n      type: parameter\n      params: {type: string}\n    - id: db\n      type: Microsoft.Sql/servers/databases\n      params:\n        apiVersion: \"2023-05-01-preview\"\n        name: ${prefix}-sql/orders\n    - id: lock\n      type: Microsoft.Authorization/locks\n      params:\n        apiVersion: \"2020-05-01\"\n        scope: ${db}\n        properties: {level: CanNotDelete}\n",
    );
    let name = "format('{0}-sql/orders', parameters('prefix'))";
    assert!(
        text.contains(&format!(
            "\"scope\": \"[resourceId('Microsoft.Sql/servers/databases', split({name}, '/')[0], split({name}, '/')[1])]\""
        )),
        "{text}"
    );
}

#[test]
fn an_output_is_always_a_string() {
    let outputs = "  outputs:\n    - name: id\n      value: ${vnet}\n    - name: state\n      value: ${vnet.properties.subnets}\n    - name: mixed\n      value: vnet ${vnet.name}\n    - name: fixed\n      value: plain\n    - name: own\n      value: \"[resourceGroup().tags]\"\n";
    let text = compile_named("net", &format!("{}{outputs}", spec(VNET)))
        .unwrap()
        .artifact
        .files["template.json"]
        .clone();
    assert_valid_template(&text);
    for want in [
        format!("\"value\": \"[{VNET_ID}]\""),
        // What might not be text is made into text.
        format!("\"value\": \"[string(reference({VNET_ID}, '2023-09-01').subnets)]\""),
        "\"value\": \"[format('vnet {0}', 'web-vnet')]\"".to_owned(),
        "\"value\": \"plain\"".to_owned(),
        "\"value\": \"[string(resourceGroup().tags)]\"".to_owned(),
    ] {
        assert!(text.contains(&want), "missing {want}\n{text}");
    }
    assert_eq!(text.matches("\"type\": \"string\"").count(), 5, "{text}");
}

// ---- refusals: references ----

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(&nic(
        "        name: web-nic\n        tags: {a: \"${vnett.name}\"}\n",
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "nic");
    assert_eq!(
        err.msg,
        "param \"tags\": ${vnett.name} refers to \"vnett\", which is not a resource or a \
         parameter of this document. An ARM expression is written in square brackets, not in ${}"
    );
}

#[test]
fn state_can_only_be_read_where_arm_allows_reference() {
    let err = refusal(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n        tags: {space: \"${vnet.properties.addressSpace}\"}\n")
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(
        err.msg.starts_with(
            "param \"tags\": ${vnet.properties.addressSpace} reads the state of \"vnet\""
        ),
        "{}",
        err.msg
    );

    let err = refusal(&format!(
        "{VNET}{}",
        nic("        name: ${vnet.properties.addressSpace}\n")
    ));
    assert_eq!(err.rule, "invalid-reference");
}

#[test]
fn a_resource_cannot_read_its_own_state() {
    let err = refusal(&nic(
        "        name: web-nic\n        properties: {mac: \"${nic.properties.macAddress}\"}\n",
    ));
    assert_eq!(err.rule, "invalid-reference");
    // Its own name is known without asking Azure, so that is fine.
    let text = template(&nic(
        "        name: web-nic\n        properties: {label: \"${nic.name}\"}\n",
    ));
    assert!(text.contains("\"label\": \"web-nic\""), "{text}");
    assert!(!text.contains("dependsOn"), "{text}");
}

#[test]
fn a_malformed_reference_is_refused() {
    for (params, rule) in [
        (
            "        name: web-nic\n        tags: {a: \"${vnet[0]}\"}\n",
            "invalid-reference",
        ),
        (
            "        name: web-nic\n        tags: {a: \"${vnet.name.length}\"}\n",
            "invalid-reference",
        ),
        (
            "        name: web-nic\n        tags: {a: \"${vnet.}\"}\n",
            "invalid-reference",
        ),
        (
            "        name: web-nic\n        properties: {a: \"${vnet.properties.a b}\"}\n",
            "invalid-reference",
        ),
        (
            "        name: web-nic\n        tags: {a: \"${vnet.name\"}\n",
            "invalid-reference",
        ),
    ] {
        let err = refusal(&format!("{VNET}{}", nic(params)));
        assert_eq!(err.rule, rule, "{params}: {}", err.msg);
    }
}

#[test]
fn names_defined_by_each_other_are_refused() {
    let err = refusal(
        "    - id: a\n      type: Microsoft.Storage/storageAccounts\n      params: {apiVersion: \"2023-01-01\", name: \"${b.name}x\"}\n    - id: b\n      type: Microsoft.Storage/storageAccounts\n      params: {apiVersion: \"2023-01-01\", name: \"${a.name}y\"}\n",
    );
    assert_eq!(err.rule, "dependency-cycle");
    assert_eq!(
        err.msg,
        "param \"name\": the name of resource \"a\" is defined in terms of itself"
    );
}

#[test]
fn resources_that_depend_on_each_other_in_a_circle_are_refused() {
    let account = |id: &str, other: &str| {
        format!(
            "    - id: {id}\n      type: Microsoft.Storage/storageAccounts\n      params:\n        apiVersion: \"2023-01-01\"\n        properties: {{peer: \"${{{other}.properties.primaryLocation}}\"}}\n"
        )
    };
    let err = refusal(&format!(
        "{}{}{}",
        account("a", "b"),
        account("b", "c"),
        account("c", "a")
    ));
    assert_eq!(err.rule, "dependency-cycle");
    assert_eq!(
        err.msg,
        "resources depend on each other in a circle, which Resource Manager refuses: a -> b -> c -> a"
    );
}

#[test]
fn a_name_must_have_as_many_parts_as_its_type() {
    let err = refusal(
        "    - id: db\n      type: Microsoft.Sql/servers/databases\n      params: {apiVersion: \"2023-05-01-preview\", name: orders}\n",
    );
    assert_eq!(err.rule, "invalid-name");
    assert_eq!(
        err.msg,
        "param \"name\": resource \"db\" has the type \"Microsoft.Sql/servers/databases\", whose \
         name has 2 parts separated by \"/\", but its name is \"orders\""
    );
    let err = refusal(&VNET.replace("web-vnet", "a/b"));
    assert_eq!(err.rule, "invalid-name");
}

#[test]
fn a_broken_expression_is_refused_with_what_is_wrong() {
    let err = refusal(&nic(
        "        name: web-nic\n        location: \"[resourceGroup().location\"\n        tags: {a: \"[concat('a', 'b']\"}\n",
    ));
    assert_eq!(err.rule, "invalid-expression");
    assert_eq!(err.resource, "nic");
    assert_eq!(
        err.msg,
        "\"[concat('a', 'b']\" is not a well-formed ARM expression: the call to concat is never \
         closed (at character 16)"
    );
}

// ---- refusals: resources ----

#[test]
fn a_type_that_is_not_arms_is_refused() {
    let err = refusal(
        "    - id: vm\n      type: azurerm_linux_virtual_machine\n      params: {name: x}\n",
    );
    assert_eq!(err.rule, "unsupported-resource-type");
}

#[test]
fn the_api_version_is_required_and_checked() {
    let err = refusal("    - id: logs\n      type: Microsoft.Storage/storageAccounts\n      params: {name: logs}\n");
    assert_eq!(err.rule, "missing-param");
    for bad in ["latest", "\"2023\"", "[2023, 1]"] {
        let err = refusal(&format!(
            "    - id: logs\n      type: Microsoft.Storage/storageAccounts\n      params: {{apiVersion: {bad}}}\n"
        ));
        assert_eq!(err.rule, "invalid-param", "{bad}");
    }
}

#[test]
fn params_meridian_writes_itself_are_reserved() {
    for key in ["type", "dependsOn"] {
        let err = refusal(&format!(
            "    - id: logs\n      type: Microsoft.Storage/storageAccounts\n      params: {{apiVersion: \"2023-01-01\", {key}: x}}\n"
        ));
        assert_eq!(err.rule, "reserved-name", "{key}");
    }
}

#[test]
fn a_notify_is_refused_because_nothing_could_carry_it() {
    let err = refusal(&format!(
        "{VNET}{}",
        nic("        name: web-nic\n      notifies:\n        - resource: vnet\n          action: restart\n")
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "nic");
}

#[test]
fn absent_is_refused_with_what_to_do_instead() {
    let err = refusal(&VNET.replace("      params:", "      state: absent\n      params:"));
    assert_eq!(err.rule, "unsupported-state");
    assert!(err.msg.contains("mode Complete"), "{}", err.msg);
}

#[test]
fn nothing_can_depend_on_a_parameter_or_the_deployment() {
    for other in ["deployment", "size"] {
        let err = refusal(&format!(
            "    - id: size\n      type: parameter\n      params: {{type: string}}\n{}",
            nic(&format!(
                "        name: web-nic\n      dependsOn:\n        - resource: {other}\n"
            ))
        ));
        assert_eq!(err.rule, "unsupported-dependency", "{other}");
    }
}

#[test]
fn the_same_resource_twice_is_refused_unless_a_condition_tells_them_apart() {
    let twice = |extra: &str| {
        format!(
            "{VNET}    - id: other\n      type: microsoft.network/virtualnetworks\n      params:\n        apiVersion: \"2023-09-01\"\n        name: WEB-VNET\n{extra}"
        )
    };
    let err = refusal(&twice(""));
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "other");
    template(&twice(
        "        condition: \"[equals(resourceGroup().location, 'westus')]\"\n",
    ));
}

#[test]
fn a_template_has_a_size_limit() {
    let many: String = (0..801)
        .map(|i| {
            format!("    - id: st{i}\n      type: Microsoft.Storage/storageAccounts\n      params: {{apiVersion: \"2023-01-01\"}}\n")
        })
        .collect();
    let err = refusal(&many);
    assert_eq!(err.rule, "template-too-large");
    assert_eq!(
        err.msg,
        "the document has 801 resources, and a template holds at most 800"
    );
}

// ---- the deployment's settings ----

#[test]
fn a_deployment_is_named_by_the_document_unless_told() {
    let files = compile_named("web-vm", &spec(VNET)).unwrap().artifact.files;
    assert!(files["deployment.json"].contains("\"name\": \"web-vm\""));
    assert!(files["deployment.json"].contains("\"mode\": \"Incremental\""));
    assert!(!files["template.json"].contains("net-rg"));

    // The core refuses a name that is not a file name (issue 133), so what
    // is left for ARM is a name Azure cannot take: longer than 64 characters.
    let err = compile_named(&"a".repeat(65), &spec(VNET)).unwrap_err();
    assert_eq!(err.rule, "invalid-name");
}

#[test]
fn a_deployment_needs_somewhere_to_go() {
    let err = compile_named("net", &format!("  resources:\n{VNET}")).unwrap_err();
    assert_eq!(err.rule, "missing-param");
    assert!(err.msg.contains("resourceGroup"), "{}", err.msg);
}

fn with_settings(settings: &str) -> Result<Emitted, CompileError> {
    compile_named(
        "net",
        &format!(
            "  resources:\n    - id: deployment\n      type: deployment\n      params: {settings}\n{VNET}"
        ),
    )
}

#[test]
fn deployment_settings_are_checked() {
    for (settings, rule) in [
        ("{resourceGroup: \"bad group!\"}", "invalid-param"),
        ("{resourceGroup: ends-with.}", "invalid-param"),
        ("{resourceGroup: rg, location: westus}", "invalid-param"),
        ("{resourceGroup: rg, mode: Destroy}", "invalid-param"),
        ("{resourceGroup: rg, scope: tenant}", "invalid-param"),
        ("{resourceGroup: [rg]}", "invalid-param"),
        ("{resourceGroup: rg, profile: prod}", "unsupported-param"),
        ("{scope: subscription}", "missing-param"),
        (
            "{scope: subscription, location: westus, resourceGroup: rg}",
            "invalid-param",
        ),
        (
            "{scope: subscription, location: westus, mode: Complete}",
            "invalid-param",
        ),
        ("{resourceGroup: rg, name: \"a/b\"}", "invalid-name"),
    ] {
        let err = with_settings(settings).expect_err(settings);
        assert_eq!(err.rule, rule, "{settings}: {}", err.msg);
    }
    let files =
        with_settings("{resourceGroup: rg, mode: Complete, subscription: prod, name: net.v2}")
            .unwrap()
            .artifact
            .files;
    for want in [
        "\"mode\": \"Complete\"",
        "\"subscription\": \"prod\"",
        "\"name\": \"net.v2\"",
    ] {
        assert!(files["deployment.json"].contains(want), "{want}");
    }
}

#[test]
fn a_document_is_one_deployment() {
    let err =
        refusal("    - id: second\n      type: deployment\n      params: {resourceGroup: other}\n");
    assert_eq!(err.rule, "duplicate-resource");
}

#[test]
fn a_template_needs_a_resource() {
    let err = refusal("    - id: size\n      type: parameter\n      params: {type: string}\n");
    assert_eq!(err.rule, "empty-document");
}

// ---- parameters and outputs ----

#[test]
fn parameters_are_checked() {
    for (params, rule) in [
        ("{defaultValue: x}", "missing-param"),
        ("{type: text}", "invalid-param"),
        ("{type: [string]}", "invalid-param"),
        ("{type: string, value: x}", "unsupported-param"),
        (
            "{type: string, defaultValue: \"[concat('a']\"}",
            "invalid-expression",
        ),
    ] {
        let err = refusal(&format!(
            "    - id: size\n      type: parameter\n      params: {params}\n{VNET}"
        ));
        assert_eq!(err.rule, rule, "{params}: {}", err.msg);
        assert_eq!(err.resource, "size");
    }
}

#[test]
fn a_secret_written_into_the_document_is_warned_about() {
    let compile = |default: &str| {
        compile_named(
            "net",
            &spec(&format!(
                "    - id: password\n      type: parameter\n      params: {{type: securestring{default}}}\n{VNET}"
            )),
        )
        .unwrap()
    };
    let emitted = compile(", defaultValue: hunter2");
    assert_eq!(emitted.warnings.len(), 1);
    assert_eq!(emitted.warnings[0].resource, "password");
    assert!(
        emitted.warnings[0].to_string().starts_with(
            "arm: password: the securestring parameter has its default written in the document"
        ),
        "{}",
        emitted.warnings[0]
    );
    // No default, or one computed when deploying, writes no secret.
    assert!(compile("").warnings.is_empty());
    assert!(compile(", defaultValue: \"[newGuid()]\"")
        .warnings
        .is_empty());
}

#[test]
fn outputs_that_differ_only_in_case_are_refused() {
    let err = compile_named(
        "net",
        &format!(
            "{}  outputs:\n    - name: vnet_id\n      value: ${{vnet}}\n    - name: Vnet_ID\n      value: ${{vnet.name}}\n",
            spec(VNET)
        ),
    )
    .unwrap_err();
    assert_eq!(err.rule, "duplicate-output");
}

#[test]
fn an_output_with_a_bad_reference_or_expression_is_refused() {
    for (value, rule) in [
        ("${nothing.name}", "invalid-reference"),
        ("\"[concat('a',]\"", "invalid-expression"),
    ] {
        let err = compile_named(
            "net",
            &format!(
                "{}  outputs:\n    - name: out\n      value: {value}\n",
                spec(VNET)
            ),
        )
        .unwrap_err();
        assert_eq!(err.rule, rule, "{value}: {}", err.msg);
        assert!(err.msg.starts_with("output \"out\": "), "{}", err.msg);
    }
}

// ---- what the plugin declares ----

#[test]
fn the_plugin_declares_what_it_is() {
    use meridian_sdk::{Capabilities, Category};
    assert_eq!(Arm.name(), "arm");
    assert_eq!(Arm.category(), Category::InfrastructureAsCode);
    assert_eq!(
        Arm.capabilities(),
        Capabilities {
            secrets: true,
            adopt: meridian_sdk::Adoption::Converge,
            ..Capabilities::default()
        }
    );
    assert_eq!(
        Arm.supported_resource_types(),
        [
            "<Namespace>/<type>",
            "address",
            "deployment",
            "disk",
            "firewall",
            "network",
            "objectStore",
            "parameter",
            "subnet",
            "vm"
        ]
    );
}

// ---- secrets ------------------------------------------------------------

#[test]
fn a_secret_in_a_param_is_a_securestring_parameter_the_runner_fills_from_a_file() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: arm\n{}",
        spec(&format!("{VNET}        tags: placeholder\n"))
    );
    let mut graph = graph_from_text("infra.yaml", &text, "arm");
    secret_in_param(&mut graph, "vnet", "tags", "db_password");
    let emitted = emit(&graph).unwrap();
    let template = &emitted.artifact.files["template.json"];
    assert_valid_template(template);
    assert!(
        template.contains("\"tags\": \"[parameters('meridian_secret_db_password')]\""),
        "{template}"
    );
    assert!(
        template.contains(
            "\"meridian_secret_db_password\": {\n      \"type\": \"securestring\"\n    }"
        ),
        "{template}"
    );
    assert!(!template.contains("(secret "), "{template}");
    assert!(!template.contains("dependsOn"), "{template}");
    // The runner's record of the deployment knows the parameter is secure.
    let deployment = &emitted.artifact.files["deployment.json"];
    assert!(
        deployment.contains("\"meridian_secret_db_password\": \"securestring\""),
        "{deployment}"
    );
    assert_eq!(
        meridian_target_arm::secret_parameter("db_password"),
        "meridian_secret_db_password"
    );
    assert_eq!(
        meridian_target_arm::secret_reference("db_password"),
        "${meridian_secret_db_password}"
    );
    assert!(Arm.capabilities().secrets);
}

#[test]
fn a_secret_in_the_deployments_settings_or_a_parameters_declaration_is_refused() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: arm\n{}",
        spec(&format!("{VNET}    - id: owner\n      type: parameter\n      params:\n        type: string\n        defaultValue: ops\n"))
    );
    for (id, param) in [("deployment", "resourceGroup"), ("owner", "defaultValue")] {
        let mut graph = graph_from_text("infra.yaml", &text, "arm");
        secret_in_param(&mut graph, id, param, "db_password");
        let err = emit(&graph).unwrap_err();
        assert_eq!(err.rule, "unsupported-secret-place", "{id}");
        assert!(
            err.msg.starts_with(&format!("param {param:?} of the")),
            "{}",
            err.msg
        );
    }
}

// ---- the common infrastructure types ----

#[test]
fn the_common_types_compile_to_their_golden_template() {
    let graph = graph_from_file("examples/common/web-arm.yaml", "arm");
    let emitted = emit(&graph).unwrap();
    let text = &emitted.artifact.files["template.json"];
    assert_valid_template(text);
    assert!(!text.contains("${"), "a reference was left in {text}");
    assert_golden(&golden(), "common-web-arm", &emitted);
}

#[test]
fn a_common_resource_is_written_with_its_api_version_at_the_groups_location() {
    let text = template("    - id: ip\n      type: address\n    - id: ip2\n      type: address\n      params: {location: northeurope}\n");
    assert!(
        text.contains("\"type\": \"Microsoft.Network/publicIPAddresses\""),
        "{text}"
    );
    assert!(text.contains("\"apiVersion\": \"2023-09-01\""), "{text}");
    assert!(text.contains("[resourceGroup().location]"), "{text}");
    assert!(text.contains("\"northeurope\""), "{text}");
}

#[test]
fn a_common_subnet_is_named_under_its_network_and_deployed_after_it() {
    let text = template("    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n    - id: web\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24}\n");
    assert!(
        text.contains("\"type\": \"Microsoft.Network/virtualNetworks/subnets\""),
        "{text}"
    );
    assert!(text.contains("format('{0}/web', 'net')"), "{text}");
    assert!(text.contains("\"dependsOn\""), "{text}");
}

#[test]
fn a_common_type_the_mapping_cannot_express_is_refused_by_the_arm_target() {
    let err = refusal("    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n    - id: web\n      type: subnet\n      params: {network: \"${net}\", cidr: 10.0.1.0/24, public: true}\n");
    assert_eq!(err.target, "arm");
    assert_eq!(err.rule, "unsupported-param");
}
