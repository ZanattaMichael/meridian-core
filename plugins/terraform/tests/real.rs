//! The whole plugin against a real `terraform`: compile, plan, apply, outputs,
//! and the schema export an editor reads.
//!
//! Everything here uses `terraform_data`, which is built into Terraform, or
//! the `hashicorp/local` provider, which needs no account, so the tests create
//! nothing outside their scratch directory.
//!
//! They run when `MERIDIAN_TEST_REAL_TERRAFORM=1`, as the `terraform` workflow
//! sets after installing it:
//!
//! ```sh
//! MERIDIAN_TEST_REAL_TERRAFORM=1 cargo test -p meridian-target-terraform --test real
//! ```
//!
//! Without the variable they pass without doing anything, so `cargo test`
//! works on a machine that has no Terraform. With it, a missing `terraform`
//! is a failure. `MERIDIAN_TERRAFORM` names another program, such as `tofu`.
//! Set `MERIDIAN_TEST_REAL_TERRAFORM_NETWORK=1` as well to include the test
//! that downloads a provider from the registry.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use meridian_plugin_host::{Client, RunFailure, StartOptions};
use meridian_plugin_testkit::{
    graph_from_file, graph_from_text, plugin_binary, repo_root, scratch, secret_in_param, secrets,
};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, LogLine, Output, Plan, RunContext,
    SchemaRequest, Value,
};
use meridian_target_terraform::PROBE_PATH;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const DOCUMENT: &str = "plugins/terraform/tests/documents/local-state.yaml";

/// The program to test against, or `None` to skip.
fn terraform() -> Option<String> {
    if std::env::var("MERIDIAN_TEST_REAL_TERRAFORM")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_TERRAFORM=1 to run against a real terraform");
        return None;
    }
    let program = std::env::var("MERIDIAN_TERRAFORM")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "terraform".to_owned());
    let runs = Command::new(&program)
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        runs,
        "MERIDIAN_TEST_REAL_TERRAFORM=1, and {program:?} does not run"
    );
    Some(program)
}

/// Starts the plugin's real binary, told which program to run.
fn start(program: &str) -> Client {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("TF_VAR_"))
        .collect();
    env.push(("MERIDIAN_TERRAFORM".to_owned(), program.to_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let binary = plugin_binary("meridian-target-terraform");
    Client::start(&binary.to_string_lossy(), &options).expect("the Terraform plugin starts")
}

fn context(dir: &Path, document: &str) -> RunContext {
    RunContext {
        document: document.to_owned(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// Compiles across the process boundary, as the host does before a run.
fn compile(client: &Client, path: &str, text: &str) -> Artifact {
    client
        .emit(&graph_from_text(path, text, "terraform"), &Data::new())
        .expect("the document compiles")
        .artifact
}

fn explain(what: &str, failure: &RunFailure, printed: &[String]) -> String {
    let detail = match failure {
        RunFailure::Tool(e) => format!("{}\n{}\n{}", e.msg, e.command, e.output),
        RunFailure::Plugin(e) => e.to_string(),
        RunFailure::Stopped(e) => e.to_string(),
    };
    format!(
        "{what} failed: {detail}\n--- printed ---\n{}",
        printed.join("\n")
    )
}

/// A plan that is expected to succeed, with what Terraform printed.
fn plan_ok(
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

fn output<'a>(outputs: &'a [Output], name: &str) -> &'a Value {
    &outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name:?}"))
        .value
}

#[ignore = "needs a real terraform; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_document_is_planned_applied_read_back_and_changed() {
    let Some(program) = terraform() else { return };
    let dir = scratch(TMP, "real-local-state");
    let client = start(&program);
    let context = context(&dir, "local-state");
    let artifact = client
        .emit(&graph_from_file(DOCUMENT, "terraform"), &Data::new())
        .expect("the document compiles")
        .artifact;

    // A plan says what would be created and creates nothing.
    let (plan, printed) = plan_ok("the first plan", &client, &context, &artifact);
    assert!(!plan.unknown);
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Create, "config"),
            (ChangeAction::Create, "release"),
            (ChangeAction::Create, "service"),
        ]
    );
    assert!(
        !context.work_dir.join("terraform.tfstate").exists(),
        "a plan writes no state"
    );
    assert!(
        printed.iter().any(|line| line.contains("Plan: 3 to add")),
        "the tool's own words are relayed: {printed:?}"
    );

    // An apply does what the plan said and reports the outputs, typed.
    let applied = apply_ok("the apply", &client, &context, &artifact);
    assert_eq!(changes(&applied.changes), changes(&plan.changes));
    assert_eq!(
        output(&applied.outputs, "config"),
        &Value::String("v1".into())
    );
    assert_eq!(
        output(&applied.outputs, "release"),
        &Value::String("web-v1".into())
    );
    assert_eq!(
        output(&applied.outputs, "ports"),
        &Value::List(vec![Value::Int(80), Value::Int(443)])
    );
    assert!(context.work_dir.join("terraform.tfstate").exists());

    // Outputs can be read again without planning anything.
    let read = client
        .outputs(&context)
        .unwrap_or_else(|e| panic!("{}", explain("reading outputs", &e, &[])));
    assert_eq!(output(&read, "release"), &Value::String("web-v1".into()));

    // What it manages is reported by resource id, for `meridian retarget`.
    let found = client
        .identifiers(&context)
        .unwrap_or_else(|e| panic!("{}", explain("reading identifiers", &e, &[])));
    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        ["config", "release", "service"]
    );
    assert!(found.values().all(|id| !id.is_empty()), "{found:?}");

    // Nothing has changed, so a second plan is empty.
    let (again, _) = plan_ok("the second plan", &client, &context, &artifact);
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    // The configuration changes. It is updated in place; the resource it
    // notifies and the one that names it in triggers_replace are replaced,
    // which a plan must call a replacement and not an update.
    let text = fs::read_to_string(repo_root().join(DOCUMENT)).unwrap();
    let second = compile(&client, DOCUMENT, &text.replace("input: v1", "input: v2"));
    let (plan, _) = plan_ok("the third plan", &client, &context, &second);
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Update, "config"),
            (ChangeAction::Replace, "release"),
            (ChangeAction::Replace, "service"),
        ],
        "the compile warned that the service would be replaced, and it is"
    );
    let applied = apply_ok("the second apply", &client, &context, &second);
    assert_eq!(
        output(&applied.outputs, "release"),
        &Value::String("web-v2".into())
    );

    // A resource removed from the document is destroyed.
    let trimmed = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: local-state\nspec:\n  target: terraform\n  resources:\n    - id: config\n      type: terraform_data\n      params:\n        input: v2\n  outputs:\n    - name: config\n      value: ${config.output}\n";
    let third = compile(&client, DOCUMENT, trimmed);
    let (plan, _) = plan_ok("the last plan", &client, &context, &third);
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Delete, "release"),
            (ChangeAction::Delete, "service"),
        ]
    );

    client.close().expect("the plugin exits cleanly");
}

#[ignore = "needs a real terraform; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_configuration_terraform_rejects_fails_the_plan_with_its_reason() {
    let Some(program) = terraform() else { return };
    let dir = scratch(TMP, "real-rejected");
    let client = start(&program);
    // The compile cannot know that terraform_data has no such argument; only
    // Terraform, which has the schema, can.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: rejected\nspec:\n  target: terraform\n  resources:\n    - id: note\n      type: terraform_data\n      params:\n        no_such_argument: 1\n";
    let artifact = compile(&client, "rejected.yaml", text);
    match client.plan(&context(&dir, "rejected"), &artifact, &mut |_| {}) {
        Err(RunFailure::Tool(e)) => {
            assert_eq!(e.msg, "terraform plan failed");
            assert!(e.exit_code.is_some_and(|code| code != 0));
            assert!(e.output.contains("no_such_argument"), "{}", e.output);
            assert!(e.changes.is_empty());
        }
        other => panic!("expected the plan to fail, got {other:?}"),
    }
}

#[ignore = "needs a real terraform; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_provider_from_the_registry_writes_a_real_file() {
    let Some(program) = terraform() else { return };
    if std::env::var_os("MERIDIAN_TEST_REAL_TERRAFORM_NETWORK").is_none() {
        eprintln!("skipped: MERIDIAN_TEST_REAL_TERRAFORM_NETWORK is not set");
        return;
    }
    let dir = scratch(TMP, "real-provider");
    let client = start(&program);
    let file = dir.join("motd");
    let document = |content: &str| {
        format!(
            "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: provider\nspec:\n  target: terraform\n  resources:\n    - id: local\n      type: provider\n      params:\n        source: hashicorp/local\n        version: \">= 2.5.0\"\n    - id: motd\n      type: local_file\n      params:\n        filename: {}\n        content: {content}\n        file_permission: \"0644\"\n    - id: seen\n      type: data.local_file\n      params:\n        filename: ${{motd.filename}}\n  outputs:\n    - name: read_back\n      value: ${{seen.content}}\n    - name: digest\n      value: ${{motd.content_sha256}}\n",
            file.display()
        )
    };
    let context = context(&dir, "provider");

    let first = compile(&client, "provider.yaml", &document("hello"));
    let applied = apply_ok("the apply", &client, &context, &first);
    assert_eq!(
        changes(&applied.changes),
        [(ChangeAction::Create, "motd")],
        "a data source is read, never created"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
    assert_eq!(
        output(&applied.outputs, "read_back"),
        &Value::String("hello".into())
    );
    assert_eq!(
        output(&applied.outputs, "digest").as_str().map(str::len),
        Some(64)
    );

    // local_file cannot be changed in place: new content is a replacement.
    let second = compile(&client, "provider.yaml", &document("goodbye"));
    let (plan, _) = plan_ok("the plan", &client, &context, &second);
    assert_eq!(changes(&plan.changes), [(ChangeAction::Replace, "motd")]);
    apply_ok("the second apply", &client, &context, &second);
    assert_eq!(fs::read_to_string(&file).unwrap(), "goodbye");

    // Drift is seen: with the file gone, a plan wants to create it again.
    fs::remove_file(&file).unwrap();
    let (plan, _) = plan_ok("the plan after drift", &client, &context, &second);
    assert_eq!(changes(&plan.changes), [(ChangeAction::Create, "motd")]);
}

#[ignore = "needs a real terraform; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_terraform_through_a_private_variables_file_and_is_in_nothing_it_prints() {
    let Some(program) = terraform() else { return };
    let dir = scratch(TMP, "real-secret");
    const VALUE: &str = "s3cr3t-not-a-real-password-Q9";
    let client = start(&program);
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: secret\nspec:\n  target: terraform\n  resources:\n    - id: token\n      type: terraform_data\n      params:\n        input: placeholder\n";
    let mut graph = graph_from_text("secret.yaml", text, "terraform");
    let slot = secret_in_param(&mut graph, "token", "input", "api_token");
    let artifact = client
        .emit(&graph, &Data::new())
        .expect("the document compiles")
        .artifact;
    let config = &artifact.files["main.tf.json"];
    assert!(
        config.contains("\"${var.meridian_secret_api_token}\""),
        "{config}"
    );
    assert!(!config.contains(VALUE));
    let mut context = context(&dir, "secret");
    context.secrets = secrets(&[(slot, "api_token", VALUE)]);

    let (plan, printed) = plan_ok("the plan", &client, &context, &artifact);
    assert_eq!(changes(&plan.changes), [(ChangeAction::Create, "token")]);
    assert!(
        !printed.iter().any(|line| line.contains(VALUE)),
        "terraform printed the secret: {printed:?}"
    );
    assert!(
        !context.work_dir.join("meridian.tfplan").exists(),
        "the saved plan, which holds every variable, was left behind"
    );

    let mut printed = Vec::new();
    client
        .apply(&context, &artifact, &mut |line: LogLine| {
            printed.push(line.text)
        })
        .unwrap_or_else(|e| panic!("{}", explain("the apply", &e, &printed)));
    assert!(
        !printed.iter().any(|line| line.contains(VALUE)),
        "terraform printed the secret: {printed:?}"
    );
    assert!(!context.work_dir.join("meridian.tfplan").exists());
    // The value reached Terraform: it is in the state Terraform keeps for
    // its own resource, which is the tool's own business and the reason a
    // document that holds secrets should declare a backend that protects
    // them.
    let state = fs::read_to_string(context.work_dir.join("terraform.tfstate")).unwrap();
    assert!(state.contains(VALUE), "the value did not reach terraform");
    assert!(!fs::read_to_string(context.work_dir.join("main.tf.json"))
        .unwrap()
        .contains(VALUE));
}

#[ignore = "needs a real terraform; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_providers_schemas_are_exported_for_an_editor() {
    let Some(program) = terraform() else { return };
    let dir = scratch(TMP, "real-schemas");
    let client = start(&program);
    assert!(
        client.info().exports_schemas,
        "the plugin says it exports schemas"
    );

    // The built-in provider needs no download, so this part runs offline.
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_file(DOCUMENT, "terraform")],
    );
    let export = client
        .export_schemas(&request)
        .unwrap_or_else(|e| panic!("{}", explain("exporting schemas", &e, &[])));
    assert_eq!(export.tool, "terraform");
    assert!(
        export
            .tool_version
            .starts_with(|c: char| c.is_ascii_digit()),
        "{:?}",
        export.tool_version
    );
    let data = export
        .schemas
        .iter()
        .find(|s| s.r#type == "terraform_data")
        .unwrap_or_else(|| panic!("terraform_data is described: {:?}", export.notes));
    assert!(data.find_param("input").is_some_and(|p| !p.computed));
    assert!(data.find_param("output").is_some_and(|p| p.computed));
    assert!(data.find_param("triggers_replace").is_some());
    assert!(data.find_param("lifecycle").is_some());
    assert!(dir.join("schemas").join(PROBE_PATH).is_file());

    if std::env::var_os("MERIDIAN_TEST_REAL_TERRAFORM_NETWORK").is_none() {
        eprintln!("skipped the registry part: MERIDIAN_TEST_REAL_TERRAFORM_NETWORK is not set");
        return;
    }
    // A provider from the registry is fetched into the work directory, its
    // version is pinned, and every one of its types is described.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: provider\nspec:\n  target: terraform\n  resources:\n    - id: local\n      type: provider\n      params:\n        source: hashicorp/local\n        version: \">= 2.5.0\"\n    - id: motd\n      type: local_file\n      params:\n        filename: motd\n        content: hello\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("provider.yaml", text, "terraform")],
    );
    let export = client
        .export_schemas(&request)
        .unwrap_or_else(|e| panic!("{}", explain("exporting the registry provider", &e, &[])));
    let pinned = export
        .pins
        .get("hashicorp/local")
        .unwrap_or_else(|| panic!("hashicorp/local is pinned: {:?}", export.pins));
    assert!(pinned.starts_with("2."), "{pinned}");
    let types: Vec<&str> = export.schemas.iter().map(|s| s.r#type.as_str()).collect();
    assert!(types.contains(&"local_file"), "{types:?}");
    assert!(
        types.contains(&"local_sensitive_file"),
        "every type is listed: {types:?}"
    );
    assert!(types.contains(&"data.local_file"), "{types:?}");
    let file = export
        .schemas
        .iter()
        .find(|s| s.r#type == "local_file")
        .unwrap();
    assert_eq!(file.source, format!("hashicorp/local {pinned}"));
    assert!(file.find_param("filename").is_some_and(|p| p.required));
    assert!(file
        .find_param("content_sha256")
        .is_some_and(|p| p.computed));
    assert!(!file.open);
    // A type the document does not use is known by name until it is used.
    let other = export
        .schemas
        .iter()
        .find(|s| s.r#type == "local_sensitive_file")
        .unwrap();
    assert!(other.open && other.params.is_empty());
    assert!(
        export
            .notes
            .iter()
            .any(|n| n.contains("listed by name only")),
        "{:?}",
        export.notes
    );
    client.close().expect("the plugin exits cleanly");
}

/// The common infrastructure types compile to configurations the real AWS and
/// Azure providers accept: `init` downloads each provider, and `validate`
/// checks every block and argument against its schema. Nothing is planned,
/// so no account is needed.
#[ignore = "needs a real terraform; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_common_types_are_configurations_the_real_providers_validate() {
    use meridian_sdk::Emitter;
    let Some(program) = terraform() else { return };
    if std::env::var_os("MERIDIAN_TEST_REAL_TERRAFORM_NETWORK").is_none() {
        eprintln!("skipped: MERIDIAN_TEST_REAL_TERRAFORM_NETWORK is not set");
        return;
    }
    for name in ["web-aws", "web-azure", "web-gcp"] {
        let dir = scratch(TMP, &format!("real-common-{name}"));
        let graph = graph_from_file(&format!("examples/common/{name}.yaml"), "terraform");
        let emitted = meridian_target_terraform::Terraform
            .emit(&graph, &Data::new())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for (path, content) in &emitted.artifact.files {
            fs::write(dir.join(path), content).unwrap();
        }
        let run = |args: &[&str]| {
            let out = Command::new(&program)
                .args(args)
                .current_dir(&dir)
                .env("TF_IN_AUTOMATION", "1")
                .output()
                .expect("terraform runs");
            assert!(
                out.status.success(),
                "{name}: {program} {args:?} failed:\n{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["init", "-input=false", "-no-color", "-backend=false"]);
        let validated = run(&["validate", "-json", "-no-color"]);
        assert!(validated.contains("\"valid\": true"), "{name}: {validated}");
    }
}
