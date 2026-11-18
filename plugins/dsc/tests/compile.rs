//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_dsc::{node_path, Dsc};

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Dsc.emit(graph, &Data::new())
}

const SERVICE: &str = "    - id: spooler\n      type: PSDscResources/Service\n      params:\n        Name: Spooler\n        State: Running\n";
const KEY: &str = "    - id: key\n      type: Microsoft.Windows/Registry\n      params:\n        keyPath: HKLM\\Software\\App\n";

fn document(host: &str, resources: &str) -> String {
    with_settings(host, "", resources)
}

/// A document with `spec.settings` written as a flow mapping, or none.
fn with_settings(host: &str, settings: &str, resources: &str) -> String {
    let settings = if settings.is_empty() {
        String::new()
    } else {
        format!("  settings: {settings}\n")
    };
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: dsc\n  targetHost:\n    from: inline\n    host: {host}\n{settings}  resources:\n{resources}"
    )
}

fn compile(resources: &str) -> Result<Emitted, CompileError> {
    emit(&graph_from_text(
        "site.yaml",
        &document("localhost", resources),
        "dsc",
    ))
}

fn compile_with(settings: &str, resources: &str) -> Result<Emitted, CompileError> {
    emit(&graph_from_text(
        "site.yaml",
        &with_settings("localhost", settings, resources),
        "dsc",
    ))
}

fn node(resources: &str) -> String {
    compile(resources).unwrap().artifact.files[&node_path("web")].clone()
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

#[test]
fn the_example_compiles_to_its_golden_files() {
    let graph = graph_from_file("examples/dsc/web-server.yaml", "dsc");
    assert_golden(&golden(), "web-server", &emit(&graph).unwrap());
}

#[test]
fn a_resource_is_a_name_a_type_and_its_properties() {
    assert_eq!(
        node(SERVICE),
        "parameters: {}\nvariables: {}\nresources:\n  - name: spooler\n    type: PSDscResources/Service\n    properties:\n      Name: Spooler\n      State: Running\n"
    );
}

#[test]
fn the_project_is_one_layer_and_carries_the_runners_settings() {
    let emitted = compile_with("{engine: dscv3}", KEY).unwrap();
    assert!(emitted.warnings.is_empty(), "{:?}", emitted.warnings);
    let datum = &emitted.artifact.files["Datum.yml"];
    assert!(
        datum.contains("  - Projects\\$($Node.ProjectPresence)\\$($Node.Project)\n"),
        "{datum}"
    );
    assert!(datum.contains("  Engine: DscV3\n"), "{datum}");
    assert!(datum.contains("  Target: Local\n"), "{datum}");
    assert!(datum.contains("  ConfigurationVersion: 0.5\n"), "{datum}");
}

#[test]
fn dependencies_and_notifications_are_the_runners_own() {
    let text = node(&format!(
        "{KEY}      notifies:\n        - resource: spooler\n          action: refresh\n{SERVICE}      dependsOn:\n        - resource: key\n    - id: last\n      type: PSDscResources/Service\n      params: {{Name: W32Time}}\n      dependsOn:\n        - resource: spooler\n"
    ));
    assert!(
        text.contains("    notify:\n      - PSDscResources/Service/spooler\n"),
        "{text}"
    );
    assert!(
        text.contains("    dependsOn:\n      - PSDscResources/Service/spooler\n"),
        "{text}"
    );
    // The notify already orders the key first, so it is not said twice.
    assert!(!text.contains("- Microsoft.Windows/Registry/key"), "{text}");
}

#[test]
fn a_reference_reads_the_other_resource_and_orders_after_it() {
    let text = node(&format!(
        "{SERVICE}    - id: note\n      type: PSDscResources/Environment\n      params:\n        Name: SPOOLER\n        Value: state is ${{spooler.State}}, cost $5\n"
    ));
    assert!(
        text.contains("Value: state is $((reference 'spooler').State), cost `$5\n"),
        "{text}"
    );
    assert!(
        text.contains("    dependsOn:\n      - PSDscResources/Service/spooler\n"),
        "{text}"
    );

    let err = refusal(&format!(
        "{SERVICE}    - id: note\n      type: PSDscResources/Environment\n      params:\n        Value: ${{spooler}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(err.msg.contains("must read a property"), "{}", err.msg);

    let err = refusal(
        "    - id: note\n      type: PSDscResources/Environment\n      params:\n        Value: ${spoolr.State}\n",
    );
    assert!(
        err.msg.contains("not a resource in this document"),
        "{}",
        err.msg
    );
}

#[test]
fn a_runtime_condition_is_a_pre_condition() {
    let text = node(
        "    - id: key\n      type: Microsoft.Windows/Registry\n      params: {keyPath: HKLM\\Software\\App}\n      runtimeWhen: \"equals (nodeName) 'web'\"\n",
    );
    assert!(
        text.contains("    preCondition: equals (nodeName) 'web'\n"),
        "{text}"
    );
}

#[test]
fn another_machine_needs_to_be_told_how_it_is_reached() {
    let err = emit(&graph_from_text(
        "site.yaml",
        &document("web01.example.com", SERVICE),
        "dsc",
    ))
    .unwrap_err();
    assert_eq!(err.rule, "missing-param");
    assert!(
        err.msg.contains("Set spec.settings.target to WinRM or SSH"),
        "{}",
        err.msg
    );

    let emitted = emit(&graph_from_text(
        "site.yaml",
        &with_settings(
            "web01.example.com",
            "{target: ssh, credential: deploy}",
            SERVICE,
        ),
        "dsc",
    ))
    .unwrap();
    let text = &emitted.artifact.files[&node_path("web")];
    assert!(
        text.contains(
            "    target:\n      action: SSH\n      computerName: web01.example.com\n      credentialRef: deploy\n"
        ),
        "{text}"
    );
}

#[test]
fn what_dsc_cannot_take_is_refused() {
    let err = refusal("    - id: nginx\n      type: package\n      params: {name: nginx}\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert!(err.msg.contains("PSDscResources/Service"), "{}", err.msg);

    let err = refusal(
        "    - id: key\n      type: Microsoft.Windows/Registry\n      state: absent\n      params: {keyPath: x}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert!(err.msg.contains("Ensure or _exist"), "{}", err.msg);

    let err = refusal(&format!(
        "{KEY}      notifies:\n        - resource: spooler\n          action: restart\n{SERVICE}"
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert!(err.msg.contains("(supported: refresh)"), "{}", err.msg);

    let err = compile_with("{engine: v9}", KEY).unwrap_err();
    assert_eq!(err.rule, "invalid-setting");
    assert!(
        err.msg.contains("accepted: Auto, DscV2, DscV3"),
        "{}",
        err.msg
    );
    let err = compile_with("{target: [SSH]}", KEY).unwrap_err();
    assert_eq!(err.rule, "invalid-setting");
    assert!(err.msg.contains("must be a string"), "{}", err.msg);

    // Settings alone configure nothing: the only resource being the old
    // runner resource leaves no instance.
    let err = refusal("    - id: settings\n      type: runner\n      params: {engine: Auto}\n");
    assert_eq!(err.rule, "empty-configuration");
}

#[test]
fn a_key_the_target_does_not_read_is_refused_before_emit() {
    // The host makes this check from `settings_keys`; the SDK makes it again
    // in front of every emit, so a tree handed straight to the emitter is
    // refused the same way.
    assert_eq!(Dsc.settings_keys(), ["credential", "engine", "target"]);
    let graph = graph_from_text(
        "site.yaml",
        &with_settings("localhost", "{engine: DscV2, transport: SSH}", KEY),
        "dsc",
    );
    let err = meridian_sdk::check_settings(&Dsc, &graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-setting");
    assert!(
        err.msg.contains("spec.settings.transport")
            && err.msg.contains("accepted: credential, engine, target"),
        "{}",
        err.msg
    );
}

#[test]
fn a_runner_resource_still_works_with_a_warning_and_cannot_contradict_the_settings() {
    // The old way: the same three as params of a resource of type "runner".
    let emitted = compile(&format!(
        "    - id: settings\n      type: runner\n      params: {{engine: dscv3, target: Local}}\n{KEY}"
    ))
    .unwrap();
    let datum = &emitted.artifact.files["Datum.yml"];
    assert!(datum.contains("  Engine: DscV3\n"), "{datum}");
    assert_eq!(emitted.warnings.len(), 1, "{:?}", emitted.warnings);
    assert_eq!(emitted.warnings[0].resource, "settings");
    assert!(
        emitted.warnings[0]
            .msg
            .contains("Move its params (engine, target) to spec.settings"),
        "{}",
        emitted.warnings[0].msg
    );
    // The resource is not a DSC resource, so it is not an instance.
    assert!(!emitted.artifact.files[&node_path("web")].contains("runner"));

    // Saying the same thing twice is allowed, in either case.
    let both = compile_with(
        "{engine: DscV3}",
        &format!("    - id: settings\n      type: runner\n      params: {{engine: dscv3}}\n{KEY}"),
    )
    .unwrap();
    assert!(both.artifact.files["Datum.yml"].contains("  Engine: DscV3\n"));

    // Contradicting the settings is refused, not resolved either way.
    let err = compile_with(
        "{engine: DscV3}",
        &format!("    - id: settings\n      type: runner\n      params: {{engine: DscV2}}\n{KEY}"),
    )
    .unwrap_err();
    assert_eq!(err.rule, "conflicting-settings");
    assert_eq!(err.resource, "settings");
    assert!(
        err.msg.contains("spec.settings.engine is \"DscV3\"")
            && err.msg.contains("resource says \"DscV2\""),
        "{}",
        err.msg
    );

    // Two runner resources are still two.
    let err = refusal(&format!(
        "    - id: a\n      type: runner\n      params: {{engine: DscV2}}\n    - id: b\n      type: runner\n      params: {{target: Local}}\n{KEY}"
    ));
    assert_eq!(err.rule, "duplicate-runner");
    let err = refusal(&format!(
        "    - id: settings\n      type: runner\n      params: {{mode: x}}\n{KEY}"
    ));
    assert_eq!(err.rule, "unsupported-setting");
}

#[test]
fn the_target_version_chooses_the_engine() {
    let versioned = |version: &str, resources: &str| {
        let text = document("localhost", resources).replace(
            "  target: dsc\n",
            &format!("  target: dsc\n  targetVersion: \"{version}\"\n"),
        );
        emit(&graph_from_text("site.yaml", &text, "dsc"))
    };
    assert_eq!(Dsc.target_versions(), ["2", "3"]);

    for (version, engine) in [("2", "DscV2"), ("3", "DscV3"), ("3.1", "DscV3")] {
        let datum = versioned(version, KEY).unwrap().artifact.files["Datum.yml"].clone();
        assert!(datum.contains(&format!("  Engine: {engine}\n")), "{datum}");
    }
    // With no version asked for, the settings decide, as before.
    let datum = compile(KEY).unwrap().artifact.files["Datum.yml"].clone();
    assert!(datum.contains("  Engine: Auto\n"), "{datum}");

    // A setting that agrees, or says nothing about the engine, is fine.
    let versioned_with = |version: &str, settings: &str| {
        let text = with_settings("localhost", settings, KEY).replace(
            "  target: dsc\n",
            &format!("  target: dsc\n  targetVersion: \"{version}\"\n"),
        );
        emit(&graph_from_text("site.yaml", &text, "dsc"))
    };
    versioned_with("3", "{engine: DscV3}").unwrap();
    let datum = versioned_with("2", "{target: Local}")
        .unwrap()
        .artifact
        .files["Datum.yml"]
        .clone();
    assert!(datum.contains("  Engine: DscV2\n"), "{datum}");

    // One that disagrees is refused, not overruled.
    let e = versioned_with("2", "{engine: DscV3}").expect_err("the engines disagree");
    assert_eq!(e.rule, "conflicting-engine");
    assert!(
        e.msg.contains("targetVersion \"2\" means the engine DscV2")
            && e.msg.contains("spec.settings.engine asks for DscV3"),
        "{}",
        e.msg
    );
    // The old runner resource is held to the same.
    let agrees =
        format!("    - id: settings\n      type: runner\n      params: {{engine: DscV3}}\n{KEY}");
    let e = versioned("2", &agrees).expect_err("the engines disagree");
    assert_eq!(e.rule, "conflicting-engine");
    assert!(e.msg.contains("the runner asks for DscV3"), "{}", e.msg);
}

#[test]
fn a_condition_on_the_runner_is_refused_not_dropped() {
    let err = refusal(&format!(
        "    - id: settings\n      type: runner\n      params:\n        engine: DscV3\n      runtimeWhen: \"equals (nodeName) 'web'\"\n{SERVICE}"
    ));
    assert_eq!(err.rule, "unsupported-runtime-condition");
    assert_eq!(err.resource, "settings");
    assert!(
        err.msg
            .contains("Put the condition on the DSC resources it should guard"),
        "{}",
        err.msg
    );
}
