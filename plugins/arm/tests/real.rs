//! The plugin against a real reader of ARM templates, and, when an Azure
//! subscription is available, against Azure itself.
//!
//! Two sets of tests, each asked for by its own variable.
//!
//! `MERIDIAN_TEST_REAL_ARM=1` runs what needs no account. Nothing validates
//! an ARM template offline the way Resource Manager does, but the Bicep
//! compiler reads ARM templates to convert them (`az bicep decompile`), and
//! to do that it has to parse the whole template: its schema and scope,
//! every resource, every template expression and every reference to a
//! parameter. Each template the plugin compiles is put through it. The
//! `azure` workflow runs these on every pull request that touches the plugin.
//!
//! `MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP=<name>` runs the runner for real:
//! a what-if, a deployment and its outputs, in that resource group, with the
//! `az` command already signed in. The group must exist. The test creates one
//! user-assigned managed identity in it, which costs nothing, and deletes it.
//!
//! ```sh
//! MERIDIAN_TEST_REAL_ARM=1 cargo test -p meridian-target-arm --test real
//! az login
//! MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP=meridian-ci \
//!     cargo test -p meridian-target-arm --test real
//! ```
//!
//! Without either variable the tests pass without doing anything.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    graph_from_file, graph_from_text, repo_root, scratch, secret_in_param, KeptLog,
};
use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, NoLog, Output, Plan, ResourceGraph,
    RunContext, Runner, Value,
};
use meridian_target_arm::{Arm, ArmRunner, TEMPLATE_PATH};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// One `az` call of the test's own, returning what it printed on stdout.
fn az(dir: &Path, args: &[&str]) -> Result<String, String> {
    let finished = Tool::new(ArmRunner::from_env().program)
        .args(args.iter().copied())
        .dir(dir)
        .capture_stdout()
        .run(&NoLog)
        .map_err(|e| e.to_string())?;
    if finished.success() {
        Ok(finished.stdout)
    } else {
        Err(format!("{}{}", finished.stdout, finished.stderr))
    }
}

fn compile(graph: &ResourceGraph) -> Artifact {
    Arm.emit(graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

/// The Bicep compiler's own complaints about what it converted a template
/// to, by their codes.
fn error_codes(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter_map(|line| line.split_once(": Error ")?.1.split(':').next())
        .collect()
}

/// Reads a compiled template with the Bicep compiler and returns the Bicep
/// it was converted to.
///
/// One kind of complaint is allowed. A template names another resource by
/// its id, in `dependsOn` and in an extension resource's `scope`, which is
/// what Resource Manager expects. Bicep wants a resource symbol there, and
/// the converter cannot always find the resource an id expression means
/// (BCP034 for a dependency, BCP036 for a scope). By then it has parsed the
/// whole template and written every resource out, so the Bicep is still
/// returned. Anything else it says is a failure.
fn read_back(dir: &Path, artifact: &Artifact) -> Result<String, String> {
    write_artifact(dir, artifact).unwrap();
    let ran = az(
        dir,
        &["bicep", "decompile", "--file", TEMPLATE_PATH, "--force"],
    );
    if let Err(why) = ran {
        let codes = error_codes(&why);
        let only_ids = !codes.is_empty() && codes.iter().all(|c| ["BCP034", "BCP036"].contains(c));
        if !only_ids {
            return Err(why);
        }
        eprintln!("the reader could not turn a resource id into a symbol:\n{why}");
    }
    let converted = Path::new(TEMPLATE_PATH).with_extension("bicep");
    fs::read_to_string(dir.join(&converted))
        .map_err(|e| format!("{} was not written: {e}", converted.display()))
}

fn asked_for_the_reader() -> bool {
    if std::env::var("MERIDIAN_TEST_REAL_ARM").ok().as_deref() != Some("1") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_ARM=1 to have real tooling read the templates");
        return false;
    }
    if let Err(why) = az(Path::new("."), &["bicep", "version"]) {
        panic!("MERIDIAN_TEST_REAL_ARM=1, and `az bicep` does not run: {why}");
    }
    true
}

#[ignore = "needs a real az; see docs/contributing/real-tool-tests.md"]
#[test]
fn every_example_is_a_template_real_tooling_reads() {
    if !asked_for_the_reader() {
        return;
    }
    // Each example, with something only a reader that understood the
    // template could have produced from it.
    let examples = [
        (
            "web-vm",
            vec![
                "param admin_password string",
                "'Microsoft.Network/networkInterfaces@2023-09-01'",
                "'Microsoft.Storage/storageAccounts@2023-01-01'",
            ],
        ),
        (
            "../common/web-arm",
            vec![
                "'Microsoft.Network/virtualNetworks/subnets@2023-09-01'",
                "'Microsoft.Compute/virtualMachines@2024-03-01'",
            ],
        ),
        (
            "landing-zone",
            vec![
                "targetScope = 'subscription'",
                "'Microsoft.Resources/resourceGroups@2024-03-01'",
                "param owner string = 'platform-team'",
            ],
        ),
    ];
    for (name, expected) in examples {
        let dir = scratch(
            TMP,
            &format!("real-read-{}", name.replace("../common/", "common-")),
        );
        let artifact = compile(&graph_from_file(
            &format!("examples/arm/{name}.yaml"),
            "arm",
        ));
        let bicep = read_back(&dir, &artifact).unwrap_or_else(|why| {
            panic!(
                "{name}: the template could not be read:\n{why}\n--- {TEMPLATE_PATH} ---\n{}",
                artifact.files[TEMPLATE_PATH]
            )
        });
        for piece in expected {
            assert!(bicep.contains(piece), "{name}: no {piece:?} in\n{bicep}");
        }
    }
}

#[ignore = "needs a real az; see docs/contributing/real-tool-tests.md"]
#[test]
fn child_resources_and_an_extension_resource_are_read() {
    if !asked_for_the_reader() {
        return;
    }
    let example = fs::read_to_string(repo_root().join("examples/arm/sql-database.yaml")).unwrap();

    // Without its lock, the example is read whole: child resources, names
    // made from a parameter, a secure parameter and a condition.
    let lock = example.find("    # An extension resource").unwrap();
    let outputs = example.find("  outputs:").unwrap();
    let lock_output = example.find("    - name: lock_id").unwrap();
    let without_lock = format!("{}{}", &example[..lock], &example[outputs..lock_output]);
    let dir = scratch(TMP, "real-read-sql-database");
    let artifact = compile(&graph_from_text("sql-database.yaml", &without_lock, "arm"));
    let bicep = read_back(&dir, &artifact).unwrap_or_else(|why| {
        panic!(
            "the template could not be read:\n{why}\n--- {TEMPLATE_PATH} ---\n{}",
            artifact.files[TEMPLATE_PATH]
        )
    });
    for piece in [
        "@secure()",
        "'Microsoft.Sql/servers/databases@2023-05-01-preview'",
        "'Microsoft.Sql/servers/firewallRules@2023-05-01-preview'",
    ] {
        assert!(bicep.contains(piece), "no {piece:?} in\n{bicep}");
    }

    // With the lock, whose `scope` is the id of the database it extends.
    // Whether Azure accepts that scope is for the subscription test to prove.
    let dir = scratch(TMP, "real-read-sql-database-lock");
    let artifact = compile(&graph_from_file("examples/arm/sql-database.yaml", "arm"));
    let bicep = read_back(&dir, &artifact).unwrap_or_else(|why| panic!("{why}"));
    let lock = "'Microsoft.Authorization/locks@2020-05-01'";
    assert!(bicep.contains(lock), "no {lock:?} in\n{bicep}");
}

#[ignore = "needs a real az; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_template_with_a_broken_expression_is_refused_by_the_reader() {
    if !asked_for_the_reader() {
        return;
    }
    let dir = scratch(TMP, "real-read-broken");
    // Proof that the reader is really reading: the same check that passes
    // the examples refuses a template whose expression names a parameter
    // that does not exist. The plugin itself refuses to compile such a
    // document, so the template is damaged after compiling.
    let mut artifact = compile(&graph_from_file("examples/arm/landing-zone.yaml", "arm"));
    let template =
        artifact.files[TEMPLATE_PATH].replace("parameters('owner')", "parameters('nobody')");
    assert_ne!(template, artifact.files[TEMPLATE_PATH]);
    artifact.files.insert(TEMPLATE_PATH.to_owned(), template);
    let why = read_back(&dir, &artifact).expect_err("there is no such parameter");
    assert!(why.contains("nobody"), "{why}");
}

// ---------------------------------------------------------------------------
// Against Azure itself.

/// The resource group to deploy into, or `None` to skip.
fn resource_group() -> Option<String> {
    let Some(group) = std::env::var("MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        eprintln!(
            "skipped: set MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP to run against a real subscription"
        );
        return None;
    };
    if let Err(why) = az(Path::new("."), &["group", "show", "--name", &group]) {
        panic!(
            "MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP={group}, and az cannot see that group: {why}"
        );
    }
    Some(group)
}

const IDENTITY: &str = "meridian-real-arm";

fn identity_exists(group: &str) -> bool {
    az(
        Path::new("."),
        &[
            "identity",
            "show",
            "--resource-group",
            group,
            "--name",
            IDENTITY,
        ],
    )
    .is_ok()
}

fn delete_identity(group: &str) {
    let _ = az(
        Path::new("."),
        &[
            "identity",
            "delete",
            "--resource-group",
            group,
            "--name",
            IDENTITY,
        ],
    );
}

/// Deletes what the test deployed when it ends, however it ends.
struct TearDown(String);

impl Drop for TearDown {
    fn drop(&mut self) {
        delete_identity(&self.0);
    }
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "identity".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn plan(dir: &Path, artifact: &Artifact) -> Plan {
    let log = KeptLog::default();
    ArmRunner::from_env()
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn apply(dir: &Path, artifact: &Artifact) -> Applied {
    let log = KeptLog::default();
    ArmRunner::from_env()
        .apply(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn changes(list: &[Change]) -> Vec<(ChangeAction, &str)> {
    list.iter()
        .map(|c| (c.action, c.resource.as_str()))
        .collect()
}

fn output<'a>(outputs: &'a [Output], name: &str) -> &'a str {
    outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name:?} in {outputs:?}"))
        .value
        .as_str()
        .expect("the output is text")
}

#[ignore = "needs a real az; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_deployment_is_previewed_made_read_back_and_changed_in_azure() {
    let Some(group) = resource_group() else {
        return;
    };
    delete_identity(&group);
    let _tear_down = TearDown(group.clone());
    let dir = scratch(TMP, "real-azure");
    let document = |stage: &str| {
        let text = format!(
            "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: identity\nspec:\n  target: arm\n  resources:\n    - id: deployment\n      type: deployment\n      params:\n        name: meridian-real-arm\n        resourceGroup: {group}\n    - id: stage\n      type: parameter\n      params:\n        type: string\n        defaultValue: {stage}\n    - id: identity\n      type: Microsoft.ManagedIdentity/userAssignedIdentities\n      params:\n        apiVersion: \"2023-01-31\"\n        name: {IDENTITY}\n        location: \"[resourceGroup().location]\"\n        tags:\n          stage: ${{stage}}\n  outputs:\n    - name: principal\n      value: ${{identity.properties.principalId}}\n    - name: name\n      value: ${{identity.name}}\n"
        );
        compile(&graph_from_text("identity.yaml", &text, "arm"))
    };

    // What-if says what would be created, under the document's id, and
    // creates nothing.
    let first = document("one");
    let preview = plan(&dir, &first);
    assert!(!preview.unknown);
    assert_eq!(
        changes(&preview.changes),
        [(ChangeAction::Create, "identity")]
    );
    assert!(!identity_exists(&group), "a plan creates nothing");

    // The deployment creates it and reports the outputs Azure computed.
    let applied = apply(&dir, &first);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert_eq!(output(&applied.outputs, "name"), IDENTITY);
    assert_eq!(output(&applied.outputs, "principal").len(), 36);

    // Outputs can be read again from the deployment's record.
    let read = ArmRunner::from_env()
        .outputs(&context(&dir))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(output(&read, "name"), IDENTITY);

    // Nothing has changed.
    let again = plan(&dir, &first);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // A changed tag is an update of the one resource, and the plan names
    // the property that changes.
    let second = document("two");
    let preview = plan(&dir, &second);
    assert_eq!(
        changes(&preview.changes),
        [(ChangeAction::Update, "identity")]
    );
    assert!(
        preview.changes[0].summary.contains("tags.stage"),
        "{:?}",
        preview.changes
    );
    apply(&dir, &second);
    let again = plan(&dir, &second);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // A destroy previews what the deployment made, deletes it, and then has
    // nothing left to remove.
    let log = KeptLog::default();
    let runner = ArmRunner::from_env();
    let preview = runner
        .plan_destroy(&context(&dir), &second, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(
        changes(&preview.changes),
        [(ChangeAction::Delete, "identity")]
    );
    let removed = runner
        .destroy(&context(&dir), &second, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(changes(&removed), changes(&preview.changes));
    assert!(!identity_exists(&group), "the identity is gone");
    let after = runner
        .plan_destroy(&context(&dir), &second, &log)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(after.changes.is_empty(), "{:?}", after.changes);
}

#[ignore = "needs a real az; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_is_a_securestring_parameter_the_reader_understands() {
    if !asked_for_the_reader() {
        return;
    }
    let dir = scratch(TMP, "real-read-secret");
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: vault\nspec:\n  target: arm\n  resources:\n    - id: deployment\n      type: deployment\n      params:\n        resourceGroup: any-rg\n    - id: token\n      type: Microsoft.KeyVault/vaults/secrets\n      params:\n        apiVersion: \"2023-07-01\"\n        name: app-vault/token\n        properties:\n          value: placeholder\n";
    let mut graph = graph_from_text("vault.yaml", text, "arm");
    let slot = secret_in_param(&mut graph, "token", "properties", "token");
    let token = graph
        .resources
        .iter_mut()
        .find(|r| r.id == "token")
        .unwrap();
    token.params.insert(
        "properties".to_owned(),
        Value::Map([("value".to_owned(), Value::Secret(slot))].into()),
    );
    let artifact = compile(&graph);
    let template = &artifact.files[TEMPLATE_PATH];
    assert!(
        template.contains("\"value\": \"[parameters('meridian_secret_token')]\""),
        "{template}"
    );
    let bicep = read_back(&dir, &artifact).unwrap_or_else(|why| {
        panic!("the template could not be read:\n{why}\n--- {TEMPLATE_PATH} ---\n{template}")
    });
    assert!(
        bicep.contains("@secure()\nparam meridian_secret_token string"),
        "{bicep}"
    );
}

/// The schema source against the published type definitions, read for a
/// template: the API version is a param. Needs the network and nothing
/// else.
#[ignore = "needs the network; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_azure_type_definitions_are_exported_with_the_api_version_as_a_param() {
    use meridian_sdk::{SchemaRequest, SchemaSource};
    use meridian_target_bicep::{AzureSchemas, Dialect};
    if std::env::var("MERIDIAN_TEST_REAL_ARM").ok().as_deref() != Some("1") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_ARM=1 to fetch the real type definitions");
        return;
    }
    let dir = scratch(TMP, "real-schemas");
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: site\nspec:\n  target: arm\n  resources:\n    - id: deployment\n      type: deployment\n      params:\n        name: site\n        resourceGroup: rg\n    - id: storage\n      type: Microsoft.Storage/storageAccounts\n      params:\n        apiVersion: 2023-01-01\n        name: site\n        location: westeurope\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("site.yaml", text, "arm")],
    );
    let log = KeptLog::default();
    let export = AzureSchemas::new(Dialect::Arm)
        .export(&request, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    let storage = export
        .schemas
        .iter()
        .find(|s| s.r#type == "Microsoft.Storage/storageAccounts")
        .unwrap_or_else(|| panic!("the storage account is described: {:?}", export.notes));
    assert!(!storage.open);
    let version = storage.find_param("apiVersion").expect("apiVersion");
    assert!(version.required);
    assert!(
        version.values.iter().any(|v| v == "2023-01-01"),
        "{:?}",
        version.values
    );
    assert!(storage.find_param("sku").is_some());
    // Every other type is listed by name, once, with its versions.
    let network = export
        .schemas
        .iter()
        .find(|s| s.r#type == "Microsoft.Network/virtualNetworks")
        .expect("listed by name");
    assert!(network.open);
    assert!(!network.find_param("apiVersion").unwrap().values.is_empty());
    assert!(!export.schemas.iter().any(|s| s.r#type.contains('@')));
}
