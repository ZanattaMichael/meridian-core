//! The whole plugin against a real `tofu`: compile, plan, apply, outputs.
//!
//! Everything here uses resources built into OpenTofu or a provider that
//! needs no account, so the tests create nothing outside their scratch
//! directory.
//!
//! They run when `MERIDIAN_TEST_TOFU` names the program, as the `opentofu`
//! workflow does after installing it:
//!
//! ```sh
//! MERIDIAN_TEST_TOFU=tofu cargo test -p meridian-target-opentofu --test real
//! ```
//!
//! Without it they pass without doing anything, so `cargo test` works on a
//! machine that has no OpenTofu. Set `MERIDIAN_TEST_TOFU_NETWORK=1` as well to
//! include the test that downloads a provider from the registry.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;

use meridian_plugin_host::{Client, RunFailure, StartOptions};
use meridian_plugin_testkit::{graph_from_file, graph_from_text, plugin_binary, scratch};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, LogLine, Output, Plan, ResourceGraph,
    RunContext, Value,
};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const PASSPHRASE: &str = "correct-horse-battery-staple";

/// The program to test against, or `None` to skip.
fn tofu() -> Option<String> {
    let program = std::env::var("MERIDIAN_TEST_TOFU").ok()?;
    (!program.trim().is_empty()).then_some(program)
}

/// Starts the plugin's real binary, told which `tofu` to run.
fn start(program: &str) -> Client {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("TF_VAR_"))
        .collect();
    env.push(("MERIDIAN_TOFU".to_owned(), program.to_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let binary = plugin_binary("meridian-target-opentofu");
    Client::start(&binary.to_string_lossy(), &options).expect("the OpenTofu plugin starts")
}

fn context(dir: &Path, document: &str, inputs: &[(&str, &str)]) -> RunContext {
    RunContext {
        document: document.to_owned(),
        work_dir: dir.join("work"),
        inputs: inputs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), Value::String((*value).to_owned())))
            .collect(),
        ..RunContext::default()
    }
}

/// Compiles across the process boundary, as the host does before a run.
fn compile(client: &Client, graph: &ResourceGraph) -> Artifact {
    client
        .emit(graph, &Data::new())
        .expect("the document compiles")
        .artifact
}

fn explain(what: &str, failure: &RunFailure, printed: &[String]) -> String {
    let detail = match failure {
        RunFailure::Tool(e) => format!("{}\n{}\n{}", e.msg, e.command, e.output),
        RunFailure::Plugin(e) => e.to_string(),
    };
    format!(
        "{what} failed: {detail}\n--- printed ---\n{}",
        printed.join("\n")
    )
}

/// A plan that is expected to succeed, with what OpenTofu printed.
fn planned_showing(
    what: &str,
    client: &Client,
    context: &RunContext,
    artifact: &Artifact,
) -> (Plan, Vec<String>) {
    let mut printed = Vec::new();
    let result = client.plan(context, artifact, &mut |line: LogLine| {
        printed.push(line.text)
    });
    match result {
        Ok(plan) => (plan, printed),
        Err(e) => panic!("{}", explain(what, &e, &printed)),
    }
}

/// A plan that is expected to succeed.
fn plan_ok(what: &str, client: &Client, context: &RunContext, artifact: &Artifact) -> Plan {
    planned_showing(what, client, context, artifact).0
}

/// An apply that is expected to succeed.
fn apply_ok(what: &str, client: &Client, context: &RunContext, artifact: &Artifact) -> Applied {
    let mut printed = Vec::new();
    let result = client.apply(context, artifact, &mut |line: LogLine| {
        printed.push(line.text)
    });
    result.unwrap_or_else(|e| panic!("{}", explain(what, &e, &printed)))
}

fn changes(list: &[Change]) -> Vec<(ChangeAction, &str)> {
    let mut seen: Vec<_> = list
        .iter()
        .map(|c| (c.action, c.resource.as_str()))
        .collect();
    seen.sort_by_key(|(_, resource)| *resource);
    seen
}

fn output<'a>(outputs: &'a [Output], name: &str) -> &'a Output {
    outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name:?}"))
}

#[test]
fn an_encrypted_state_is_planned_applied_and_read_back() {
    let Some(program) = tofu() else {
        eprintln!("skipped: MERIDIAN_TEST_TOFU is not set");
        return;
    };
    let dir = scratch(TMP, "real-encrypted");
    let client = start(&program);
    let graph = graph_from_file("examples/opentofu/encrypted-state.yaml", "opentofu");
    let artifact = compile(&client, &graph);
    let inputs = [("state_passphrase", PASSPHRASE), ("environment", "ci")];
    let context = context(&dir, "encrypted-state", &inputs);

    // A plan says what would be created and creates nothing.
    let (plan, printed) = planned_showing("the first plan", &client, &context, &artifact);
    assert!(!plan.unknown);
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Create, "release"),
            (ChangeAction::Create, "smoke_test")
        ]
    );
    assert!(
        !context.work_dir.join("terraform.tfstate").exists(),
        "a plan writes no state"
    );
    assert!(
        printed.iter().any(|line| line.contains("Plan: 2 to add")),
        "the tool's own words are relayed: {printed:?}"
    );

    // An apply does what the plan said and reports the outputs.
    let applied = apply_ok("the apply", &client, &context, &artifact);
    assert_eq!(changes(&applied.changes), changes(&plan.changes));
    let release = output(&applied.outputs, "release");
    assert_eq!(release.value, Value::String("web-ci".into()));
    assert!(!release.sensitive);
    // The fingerprint is computed from a sensitive variable, so the compiled
    // output was marked sensitive and OpenTofu accepted it.
    let fingerprint = output(&applied.outputs, "passphrase_fingerprint");
    assert!(fingerprint.sensitive);
    assert_eq!(fingerprint.value.as_str().map(str::len), Some(12));

    // The state on disk is encrypted: the value it records is not readable.
    let state = fs::read_to_string(context.work_dir.join("terraform.tfstate"))
        .expect("the apply wrote a state file");
    assert!(state.contains("encrypted_data"), "{state}");
    assert!(!state.contains("web-ci"), "the state is not in the clear");
    // So is the saved plan, which holds a copy of the state.
    let plan_file = fs::read(context.work_dir.join("meridian.tfplan")).expect("a plan was saved");
    assert!(
        !plan_file.starts_with(b"PK"),
        "an unencrypted plan is a zip archive; this one should not be"
    );
    // The passphrase is in neither the configuration nor anything beside it.
    for entry in fs::read_dir(&context.work_dir).unwrap().flatten() {
        if entry.path().is_file() {
            let bytes = fs::read(entry.path()).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(PASSPHRASE),
                "{} holds the passphrase",
                entry.path().display()
            );
        }
    }

    // Outputs can be read again without planning anything.
    let read = client
        .outputs(&context)
        .unwrap_or_else(|e| panic!("{}", explain("reading outputs", &e, &[])));
    assert_eq!(
        output(&read, "release").value,
        Value::String("web-ci".into())
    );

    // Nothing has changed, so a second plan is empty.
    let again = plan_ok("the second plan", &client, &context, &artifact);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // A different input is an update in place of the one resource it reaches.
    let other = [("state_passphrase", PASSPHRASE), ("environment", "prod")];
    let renamed = RunContext {
        inputs: other
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect(),
        ..context.clone()
    };
    let plan = plan_ok("the third plan", &client, &renamed, &artifact);
    assert_eq!(changes(&plan.changes), [(ChangeAction::Update, "release")]);

    // The wrong passphrase cannot read the state, and that is the tool's
    // answer, not a failure of the plugin.
    let wrong = [
        ("state_passphrase", "not-the-right-passphrase"),
        ("environment", "ci"),
    ];
    let locked_out = RunContext {
        inputs: wrong
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect(),
        ..context.clone()
    };
    match client.plan(&locked_out, &artifact, &mut |_| {}) {
        Err(RunFailure::Tool(e)) => {
            assert!(e.msg.starts_with("tofu "), "{}", e.msg);
            assert!(e.exit_code.is_some_and(|code| code != 0));
            assert!(!e.output.contains(PASSPHRASE));
        }
        other => panic!("the wrong passphrase should fail the tool, got {other:?}"),
    }

    client.close().expect("the plugin exits cleanly");
}

#[test]
fn a_notify_replaces_the_notified_resource_when_its_source_changes() {
    let Some(program) = tofu() else {
        eprintln!("skipped: MERIDIAN_TEST_TOFU is not set");
        return;
    };
    let dir = scratch(TMP, "real-notify");
    let client = start(&program);
    let context = context(&dir, "notify", &[]);
    let path = "plugins/opentofu/tests/documents/notify.yaml";

    let first = compile(&client, &graph_from_file(path, "opentofu"));
    let applied = apply_ok("the first apply", &client, &context, &first);
    assert_eq!(
        changes(&applied.changes),
        [
            (ChangeAction::Create, "config"),
            (ChangeAction::Create, "service")
        ]
    );
    assert_eq!(
        output(&applied.outputs, "config").value,
        Value::String("v1".into())
    );

    // The configuration changes; the service's own arguments do not.
    let text = fs::read_to_string(meridian_plugin_testkit::repo_root().join(path))
        .unwrap()
        .replace("input: v1", "input: v2");
    let second = compile(&client, &graph_from_text(path, &text, "opentofu"));
    let plan = plan_ok("the plan", &client, &context, &second);
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Update, "config"),
            (ChangeAction::Replace, "service")
        ],
        "the compile warned that the service would be replaced, and it is"
    );

    let applied = apply_ok("the second apply", &client, &context, &second);
    assert_eq!(
        output(&applied.outputs, "config").value,
        Value::String("v2".into())
    );

    // A resource removed from the document is destroyed.
    let trimmed = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: notify\nspec:\n  target: opentofu\n  resources:\n    - id: config\n      type: terraform_data\n      params:\n        input: v2\n  outputs:\n    - name: config\n      value: ${config.output}\n";
    let third = compile(&client, &graph_from_text(path, trimmed, "opentofu"));
    let plan = plan_ok("the last plan", &client, &context, &third);
    assert_eq!(changes(&plan.changes), [(ChangeAction::Delete, "service")]);
}

#[test]
fn a_configuration_opentofu_rejects_fails_the_plan_with_its_reason() {
    let Some(program) = tofu() else {
        eprintln!("skipped: MERIDIAN_TEST_TOFU is not set");
        return;
    };
    let dir = scratch(TMP, "real-rejected");
    let client = start(&program);
    // The compile cannot know that terraform_data has no such argument; only
    // OpenTofu, which has the schema, can.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: rejected\nspec:\n  target: opentofu\n  resources:\n    - id: note\n      type: terraform_data\n      params:\n        no_such_argument: 1\n";
    let artifact = compile(&client, &graph_from_text("rejected.yaml", text, "opentofu"));
    match client.plan(&context(&dir, "rejected", &[]), &artifact, &mut |_| {}) {
        Err(RunFailure::Tool(e)) => {
            assert_eq!(e.msg, "tofu plan failed");
            assert!(e.output.contains("no_such_argument"), "{}", e.output);
            assert!(e.changes.is_empty());
        }
        other => panic!("expected the plan to fail, got {other:?}"),
    }
}

#[test]
fn variables_locals_and_validation_work_the_way_the_document_says() {
    let Some(program) = tofu() else {
        eprintln!("skipped: MERIDIAN_TEST_TOFU is not set");
        return;
    };
    let dir = scratch(TMP, "real-variables");
    let client = start(&program);
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: variables\nspec:\n  target: opentofu\n  resources:\n    - id: region\n      type: variable\n      params:\n        type: string\n        default: eu-west-1\n        validation:\n          condition: ${startswith(region, \"eu-\")}\n          error_message: Only European regions are allowed.\n    - id: replicas\n      type: variable\n      params:\n        type: number\n        default: 2\n    - id: big\n      type: variable\n      params:\n        type: bool\n        default: false\n    - id: name_prefix\n      type: local\n      params:\n        value: ${lower(\"Web\")}-${region}\n    - id: sizing\n      type: local\n      params:\n        value:\n          size: '${big ? \"large\" : \"small\"}'\n          count: ${replicas + 1}\n    - id: record\n      type: terraform_data\n      params:\n        input:\n          name: ${name_prefix}\n          sizing: ${sizing}\n          zones: '${[for zone in [\"a\", \"b\"] : \"${region}${zone}\"]}'\n  outputs:\n    - name: name\n      value: ${record.output.name}\n    - name: count\n      value: ${record.output.sizing.count}\n    - name: zones\n      value: ${record.output.zones}\n    - name: size\n      value: ${sizing.size}\n";
    let artifact = compile(
        &client,
        &graph_from_text("variables.yaml", text, "opentofu"),
    );

    let defaults = context(&dir, "variables", &[]);
    let applied = apply_ok("the apply", &client, &defaults, &artifact);
    assert_eq!(
        output(&applied.outputs, "name").value,
        Value::String("web-eu-west-1".into())
    );
    assert_eq!(output(&applied.outputs, "count").value, Value::Int(3));
    assert_eq!(
        output(&applied.outputs, "zones").value,
        Value::List(vec![
            Value::String("eu-west-1a".into()),
            Value::String("eu-west-1b".into())
        ])
    );
    assert_eq!(
        output(&applied.outputs, "size").value,
        Value::String("small".into())
    );

    // Inputs from an earlier stage replace the defaults, typed as declared.
    let handed_over = context(
        &dir,
        "variables",
        &[("region", "eu-north-1"), ("replicas", "4"), ("big", "true")],
    );
    let applied = apply_ok("the second apply", &client, &handed_over, &artifact);
    assert_eq!(
        output(&applied.outputs, "name").value,
        Value::String("web-eu-north-1".into())
    );
    assert_eq!(output(&applied.outputs, "count").value, Value::Int(5));
    assert_eq!(
        output(&applied.outputs, "size").value,
        Value::String("large".into())
    );

    // The document's own validation rule is OpenTofu's to enforce.
    let elsewhere = context(&dir, "variables", &[("region", "us-east-1")]);
    match client.plan(&elsewhere, &artifact, &mut |_| {}) {
        Err(RunFailure::Tool(e)) => {
            assert!(
                e.output.contains("Only European regions are allowed."),
                "{}",
                e.output
            );
        }
        other => panic!("expected the validation to fail the plan, got {other:?}"),
    }
}

#[test]
fn a_provider_from_the_registry_and_an_ephemeral_resource_work() {
    let Some(program) = tofu() else {
        eprintln!("skipped: MERIDIAN_TEST_TOFU is not set");
        return;
    };
    if std::env::var_os("MERIDIAN_TEST_TOFU_NETWORK").is_none() {
        eprintln!("skipped: MERIDIAN_TEST_TOFU_NETWORK is not set");
        return;
    }
    let dir = scratch(TMP, "real-provider");
    let client = start(&program);
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: provider\nspec:\n  target: opentofu\n  resources:\n    - id: random\n      type: provider\n      params:\n        source: hashicorp/random\n        version: \">= 3.7.0\"\n    - id: session_key\n      type: ephemeral.random_password\n      params:\n        length: 24\n    - id: pet\n      type: random_pet\n      params:\n        length: 2\n    - id: label\n      type: terraform_data\n      params:\n        input: pet-${pet.id}\n  outputs:\n    - name: label\n      value: ${label.output}\n";
    let artifact = compile(&client, &graph_from_text("provider.yaml", text, "opentofu"));
    let context = context(&dir, "provider", &[]);
    let applied = apply_ok("the apply", &client, &context, &artifact);
    assert_eq!(
        changes(&applied.changes),
        [
            (ChangeAction::Create, "label"),
            (ChangeAction::Create, "pet")
        ],
        "an ephemeral resource is opened and closed, never created"
    );
    let label = output(&applied.outputs, "label")
        .value
        .as_str()
        .unwrap()
        .to_owned();
    assert!(label.starts_with("pet-") && label.len() > 4, "{label}");
    // An ephemeral value is in no state.
    let state = fs::read_to_string(context.work_dir.join("terraform.tfstate")).unwrap();
    assert!(!state.contains("random_password"), "{state}");
}
