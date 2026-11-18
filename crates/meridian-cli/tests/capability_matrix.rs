//! The capability matrix in the documentation is generated from what each
//! plugin declares, and this test fails when the two differ. Record it again
//! with `UPDATE_GOLDEN=1` after a plugin's capabilities change on purpose.

use std::fs;

use meridian_plugin_testkit::repo_root;
use meridian_sdk::{Emitter, Importer, Runner, SchemaSource};

const PATH: &str = "docs/reference/capability-matrix.md";

const INTRODUCTION: &str = "\
# Capability matrix

What each target can express, as each plugin itself declares it. This page is
generated: `crates/meridian-cli/tests/capability_matrix.rs` writes it from the
plugins and fails when it is out of date, so it cannot drift from the code.

A cell that says **no** is not a missing feature waiting to be papered over.
A document that asks a target for something it cannot do is refused when it is
compiled, with the reason. Each target's own page says how it does what it can.

- **notifies** is *native* when the tool has its own way to react to a change,
  and *synthetic* when Meridian builds the reaction out of something else the
  tool has. A synthetic notify shows up in the tool's own output, and the
  target's page says what it looks like.
- **runtimeWhen** is a condition the tool checks on the node at the moment it
  applies a resource.
- **plan** asks the tool what an apply would change, without changing it.
- **saved plan** says the tool saves its plan when `meridian plan --out` is
  run, and applies exactly that plan on `meridian apply --plan`. Every target
  is checked by its artifact hash; this column says which also hand the tool
  its own plan back. See [a saved plan](../operations/pull-requests.md).
- **outputs** are values a later stage of a workflow can read.
- **inputs** says the target hands its tool the values a document's
  `spec.imports` brings from other documents, as the tool's own variables or
  parameters.
- **secrets** says a param may hold a secret: the target compiles a
  reference to it, never the value, and hands the value to its tool by a
  private channel when it runs. See
  [Secrets](dsl/secrets.md#handing-a-secret-to-a-tool).
- **credentials** says the target reaches its host as the credential
  `spec.targetHost.credential` names: it compiles the credential's plain
  fields, such as the user, and hands its secret fields to the tool by a
  private channel when it runs. See
  [Credentials](dsl/credentials.md).
- **cloud** says the target reaches its cloud as the credential
  `spec.endpoint.credential` names (an `aws`, `azure` or `gcp` credential):
  it hands the credential to its tool in the tool's own environment, the
  one way the providers take a login, and the plan line says so. See
  [Reaching a cloud as a credential](dsl/credentials.md#reaching-a-cloud-as-a-credential).
- **import** is what `meridian import` can read of the tool's existing
  configuration and turn into a document for the target. See
  [Importing existing configuration](../operations/importing.md).
- **schemas** says the plugin exports its tool's own resource schemas, so
  `meridian schema sync` can cache them for the editor tools. See
  [Tool schemas](../tooling/schema-sync.md).

";

fn row(
    emitter: &dyn Emitter,
    runner: &dyn Runner,
    importer: Option<&dyn Importer>,
    schemas: Option<&dyn SchemaSource>,
) -> String {
    let capabilities = emitter.capabilities();
    let notifies = match (capabilities.native_notify, capabilities.synthetic_notify) {
        (true, true) => "native and synthetic",
        (true, false) => "native",
        (false, true) => "synthetic",
        (false, false) => "no",
    };
    let yes = |b: bool| if b { "yes" } else { "no" };
    let operations = runner.operations();
    let types: Vec<String> = emitter
        .supported_resource_types()
        .iter()
        .map(|t| format!("`{}`", t.replace('<', "‹").replace('>', "›")))
        .collect();
    let import = match importer {
        Some(importer) => importer
            .formats()
            .iter()
            .map(|f| f.description.clone())
            .collect::<Vec<_>>()
            .join("; "),
        None => "no".to_owned(),
    };
    format!(
        "| [{name}](../targets/{name}.md) | {category} | {types} | {notifies} | {runtime} | {plan} | {saved} | {apply} | {outputs} | {inputs} | {secrets} | {credentials} | {cloud} | {import} | {schemas} |\n",
        schemas = yes(schemas.is_some()),
        saved = yes(runner.saves_plans()),
        credentials = yes(capabilities.credentials),
        cloud = yes(capabilities.cloud_credentials),
        name = emitter.name(),
        category = emitter.category(),
        types = types.join(", "),
        runtime = yes(capabilities.runtime_condition),
        plan = yes(operations.plan),
        apply = yes(operations.apply),
        outputs = yes(operations.outputs),
        inputs = yes(runner.accepts_inputs()),
        secrets = yes(capabilities.secrets),
    )
}

fn matrix() -> String {
    let mut rows: Vec<String> = meridian_all_targets::ALL
        .iter()
        .map(|target| {
            let importer = target.importer();
            let schemas = target.schemas();
            row(
                target.emitter().as_ref(),
                target.runner().as_ref(),
                importer.as_deref().map(|i| i as &dyn Importer),
                schemas.as_deref(),
            )
        })
        .collect();
    rows.sort();
    format!(
        "{INTRODUCTION}| Target | Category | Resource types | notifies | runtimeWhen | plan | saved plan | apply | outputs | inputs | secrets | credentials | cloud | import | schemas |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n{}",
        rows.concat()
    )
}

#[test]
fn the_documented_matrix_is_what_the_plugins_declare() {
    let path = repo_root().join(PATH);
    let generated = matrix();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with what the plugins declare; run the tests with UPDATE_GOLDEN=1 to record it again"
    );
}
