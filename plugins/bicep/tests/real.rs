//! The plugin against the real Bicep compiler, and, when an Azure
//! subscription is available, against Azure itself.
//!
//! Two sets of tests, each asked for by its own variable.
//!
//! `MERIDIAN_TEST_REAL_BICEP=1` runs what needs no account: every Bicep file
//! the plugin compiles is built by the real compiler, `az bicep build`, which
//! parses it, checks each resource against Azure's published types and
//! produces the ARM template Azure would be sent. The `azure` workflow runs
//! these on every pull request that touches the plugin.
//!
//! `MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP=<name>` runs the runner for real:
//! a what-if, a deployment and its outputs, in that resource group, with the
//! `az` command already signed in. The group must exist. The test creates one
//! user-assigned managed identity in it, which costs nothing, and deletes it.
//!
//! ```sh
//! MERIDIAN_TEST_REAL_BICEP=1 cargo test -p meridian-target-bicep --test real
//! az login
//! MERIDIAN_TEST_REAL_AZURE_RESOURCE_GROUP=meridian-ci \
//!     cargo test -p meridian-target-bicep --test real
//! ```
//!
//! Without either variable the tests pass without doing anything.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::path::Path;

use meridian_plugin_testkit::{
    graph_from_file, graph_from_text, scratch, secret_in_param, KeptLog,
};
use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Data, Emitter, NoLog, Output, Plan,
    ResourceGraph, RunContext, Runner, Value,
};
use meridian_target_bicep::{Bicep, BicepRunner, TEMPLATE_PATH};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// One `az` call of the test's own, returning what it printed on stdout.
fn az(dir: &Path, args: &[&str]) -> Result<String, String> {
    let finished = Tool::new(BicepRunner::from_env().program)
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
    Bicep
        .emit(graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

/// Builds a compiled Bicep file with the real compiler and returns the ARM
/// template it produces.
fn build(dir: &Path, artifact: &Artifact) -> Value {
    write_artifact(dir, artifact).unwrap();
    let json = az(dir, &["bicep", "build", "--file", TEMPLATE_PATH, "--stdout"])
        .unwrap_or_else(|why| {
            panic!(
                "the Bicep compiler rejected what the plugin compiled:\n{why}\n--- {TEMPLATE_PATH} ---\n{}",
                artifact.files[TEMPLATE_PATH]
            )
        });
    decode_value(json.trim().as_bytes())
        .unwrap_or_else(|e| panic!("the compiler's output is not JSON: {e}\n{json}"))
}

/// The `type` and `name` of every resource of a built template, sorted.
fn resources(template: &Value) -> Vec<(String, String)> {
    let text = |resource: &Value, key: &str| {
        resource
            .as_map()
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let listed = template.as_map().and_then(|t| t.get("resources"));
    // A template is a list of resources, or, in the newer language version,
    // a map of them by symbolic name.
    let mut seen: Vec<(String, String)> = match listed {
        Some(Value::List(list)) => list
            .iter()
            .map(|r| (text(r, "type"), text(r, "name")))
            .collect(),
        Some(Value::Map(map)) => map
            .values()
            .map(|r| (text(r, "type"), text(r, "name")))
            .collect(),
        _ => Vec::new(),
    };
    seen.sort();
    seen
}

fn output_names(template: &Value) -> Vec<String> {
    template
        .as_map()
        .and_then(|t| t.get("outputs"))
        .and_then(Value::as_map)
        .map(|outputs| outputs.keys().cloned().collect())
        .unwrap_or_default()
}

fn asked_for_the_compiler() -> bool {
    if std::env::var("MERIDIAN_TEST_REAL_BICEP").ok().as_deref() != Some("1") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_BICEP=1 to run against the real Bicep compiler");
        return false;
    }
    if let Err(why) = az(Path::new("."), &["bicep", "version"]) {
        panic!("MERIDIAN_TEST_REAL_BICEP=1, and `az bicep` does not run: {why}");
    }
    true
}

#[ignore = "needs a real az and the Bicep compiler; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_example_is_bicep_the_real_compiler_builds() {
    if !asked_for_the_compiler() {
        return;
    }
    let dir = scratch(TMP, "real-build-example");
    let artifact = compile(&graph_from_file("examples/bicep/web-vm.yaml", "bicep"));
    let template = build(&dir, &artifact);
    assert_eq!(
        resources(&template),
        [
            ("Microsoft.Authorization/locks", "keep-web-ip"),
            ("Microsoft.Network/networkInterfaces", "web-nic"),
            ("Microsoft.Network/publicIPAddresses", "web-ip"),
            ("Microsoft.Network/virtualNetworks", "web-vnet"),
        ]
        .map(|(t, n)| (t.to_owned(), n.to_owned()))
    );
    assert_eq!(output_names(&template), ["web_url", "web_vm_ip"]);
    // The references between resources became dependencies the compiler
    // worked out, and the explicit one survived beside them.
    let text = template.canonical();
    assert!(text.contains("dependsOn"), "{text}");
}

#[ignore = "needs a real az and the Bicep compiler; see docs/contributing/real-tool-tests.md"]
#[test]
fn every_kind_of_value_the_plugin_writes_is_valid_bicep() {
    if !asked_for_the_compiler() {
        return;
    }
    let dir = scratch(TMP, "real-build-values");
    // Strings that need escaping, numbers, booleans, nested lists and maps,
    // keys that are not identifiers, interpolation, Bicep's own functions,
    // and a reference used as a whole value. A tag may only read what is
    // known when the deployment starts, which a resource's name is.
    let text = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: values
spec:
  target: bicep
  resources:
    - id: target
      type: deployment
      params:
        resourceGroup: any-rg
    - id: store
      type: Microsoft.Storage/storageAccounts@2023-05-01
      params:
        name: st${uniqueString(resourceGroup().id)}
        location: ${resourceGroup().location}
        kind: StorageV2
        sku:
          name: Standard_LRS
        tags:
          it's: "a 'quoted' value with a \\ backslash"
          cost-center: "42"
          multi-line: "one\ntwo"
          env.name: prod
        properties:
          allowBlobPublicAccess: false
          minimumTlsVersion: TLS1_2
          supportsHttpsTrafficOnly: true
          networkAcls:
            defaultAction: Deny
            bypass: AzureServices
            ipRules: []
            virtualNetworkRules: []
    - id: blobs
      type: Microsoft.Storage/storageAccounts/blobServices@2023-05-01
      params:
        name: ${store.name}/default
        properties:
          deleteRetentionPolicy:
            enabled: true
            days: 7
    - id: identity
      type: Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31
      params:
        name: values-identity
        location: ${resourceGroup().location}
        tags:
          store: ${store.name}
          upper: ${toUpper(store.name)}
      dependsOn:
        - resource: blobs
  outputs:
    - name: endpoint
      value: ${store.properties.primaryEndpoints.blob}
    - name: principal
      value: ${identity.properties.principalId}
    - name: label
      value: store ${store.name} in ${resourceGroup().location}
"#;
    let artifact = compile(&graph_from_text("values.yaml", text, "bicep"));
    let template = build(&dir, &artifact);
    let types: Vec<String> = resources(&template).into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        types,
        [
            "Microsoft.ManagedIdentity/userAssignedIdentities",
            "Microsoft.Storage/storageAccounts",
            "Microsoft.Storage/storageAccounts/blobServices",
        ]
    );
    assert_eq!(output_names(&template), ["endpoint", "label", "principal"]);
    // What the author wrote reaches the template character for character.
    let built = template.canonical();
    for expected in [
        r#"a 'quoted' value with a \\ backslash"#,
        r#"one\ntwo"#,
        "env.name",
        "it's",
    ] {
        assert!(built.contains(expected), "{expected:?} is not in {built}");
    }
}

#[ignore = "needs a real az and the Bicep compiler; see docs/contributing/real-tool-tests.md"]
#[test]
fn what_the_compiler_rejects_is_reported_by_the_compiler() {
    if !asked_for_the_compiler() {
        return;
    }
    let dir = scratch(TMP, "real-build-rejected");
    // The plugin passes Bicep's expression language through. Whether a
    // function exists is the compiler's to say, and it does.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: rejected\nspec:\n  target: bicep\n  resources:\n    - id: target\n      type: deployment\n      params:\n        resourceGroup: any-rg\n    - id: identity\n      type: Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31\n      params:\n        name: ${noSuchFunction('x')}\n        location: ${resourceGroup().location}\n";
    let artifact = compile(&graph_from_text("rejected.yaml", text, "bicep"));
    write_artifact(&dir, &artifact).unwrap();
    let why = az(
        &dir,
        &["bicep", "build", "--file", TEMPLATE_PATH, "--stdout"],
    )
    .expect_err("there is no such function");
    assert!(why.contains("noSuchFunction"), "{why}");
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

const IDENTITY: &str = "meridian-real-bicep";

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
    BicepRunner::from_env()
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn apply(dir: &Path, artifact: &Artifact) -> Applied {
    let log = KeptLog::default();
    BicepRunner::from_env()
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

#[ignore = "needs a real az and the Bicep compiler; see docs/contributing/real-tool-tests.md"]
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
            "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: identity\nspec:\n  target: bicep\n  resources:\n    - id: target\n      type: deployment\n      params:\n        resourceGroup: {group}\n    - id: identity\n      type: Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31\n      params:\n        name: {IDENTITY}\n        location: ${{resourceGroup().location}}\n        tags:\n          stage: {stage}\n  outputs:\n    - name: principal\n      value: ${{identity.properties.principalId}}\n    - name: name\n      value: ${{identity.name}}\n"
        );
        compile(&graph_from_text("identity.yaml", &text, "bicep"))
    };

    // What-if says what would be created, under the document's id, and
    // creates nothing.
    let first = document("one");
    let preview = plan(&dir, &first);
    assert_eq!(
        changes(&preview.changes),
        [(ChangeAction::Create, "identity")]
    );
    assert!(
        az(
            Path::new("."),
            &[
                "identity",
                "show",
                "--resource-group",
                &group,
                "--name",
                IDENTITY
            ]
        )
        .is_err(),
        "a plan creates nothing"
    );

    // The deployment creates it and reports the outputs Azure computed.
    let applied = apply(&dir, &first);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert_eq!(output(&applied.outputs, "name"), IDENTITY);
    assert_eq!(output(&applied.outputs, "principal").len(), 36);

    // Outputs can be read again from the deployment's record.
    let read = BicepRunner::from_env()
        .outputs(&context(&dir))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(output(&read, "name"), IDENTITY);

    // Nothing has changed.
    let again = plan(&dir, &first);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // A changed tag is an update of the one resource.
    let second = document("two");
    let preview = plan(&dir, &second);
    assert_eq!(
        changes(&preview.changes),
        [(ChangeAction::Update, "identity")]
    );
    apply(&dir, &second);
    let again = plan(&dir, &second);
    assert!(again.changes.is_empty(), "{:?}", again.changes);
}

#[ignore = "needs a real az and the Bicep compiler; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_is_a_secure_parameter_the_compiler_accepts() {
    if !asked_for_the_compiler() {
        return;
    }
    let dir = scratch(TMP, "real-build-secret");
    // A Key Vault secret whose value is a Meridian secret: the way a value
    // reaches the vault without being written anywhere on the way.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: vault\nspec:\n  target: bicep\n  resources:\n    - id: target\n      type: deployment\n      params:\n        resourceGroup: any-rg\n    - id: token\n      type: Microsoft.KeyVault/vaults/secrets@2023-07-01\n      params:\n        name: app-vault/token\n        properties:\n          value: placeholder\n";
    let mut graph = graph_from_text("vault.yaml", text, "bicep");
    let slot = secret_in_param(&mut graph, "token", "properties", "token");
    // The secret is inside the properties map, as the type wants it.
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
    let bicep = &artifact.files[TEMPLATE_PATH];
    assert!(
        bicep.contains("@secure()\nparam meridian_secret_token string\n"),
        "{bicep}"
    );
    assert!(bicep.contains("value: meridian_secret_token\n"), "{bicep}");
    let template = build(&dir, &artifact);
    // The compiler made a securestring parameter of it.
    let parameter = template
        .as_map()
        .and_then(|t| t.get("parameters"))
        .and_then(Value::as_map)
        .and_then(|p| p.get("meridian_secret_token"))
        .and_then(Value::as_map)
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str);
    assert_eq!(parameter, Some("securestring"), "{}", template.canonical());
    assert!(
        template
            .canonical()
            .contains("[parameters('meridian_secret_token')]"),
        "{}",
        template.canonical()
    );
}
