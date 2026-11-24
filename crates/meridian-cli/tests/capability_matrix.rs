//! The capability matrix in the documentation is generated from what each
//! plugin declares, and this test fails when the two differ. Record it again
//! with `UPDATE_GOLDEN=1` after a plugin's capabilities change on purpose.

use std::fs;

use meridian_plugin_testkit::repo_root;
use meridian_sdk::{Adoption, Emitter, Importer, Runner, SchemaSource};

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
  target's page says what it looks like. *Meridian-run* means the plugin
  performs the action after the stage applies, when the tool can do neither.
  See [Meridian-run notifies](../concepts/dependencies.md#meridian-run-notifies).
- **runtimeWhen** is a condition the tool checks on the node at the moment it
  applies a resource.
- **plan** asks the tool what an apply would change, without changing it.
- **saved plan** is how `meridian apply --plan` holds the target to its
  plan, after checking the artifact hash: *yes*, the tool saves its own plan
  and applies exactly that; *guarded* (Meridian-run), the target is planned
  again and the apply refused when the changes differ. See
  [a saved plan](../operations/pull-requests.md).
- **outputs** are values a later stage of a workflow can read.
- **inspect** says `meridian inspect` can read one resource of the target as
  it is now, without changing it. See [`meridian inspect`](cli.md#inspect).
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
- **adopt** is what the target does with a resource's `adopt`: *import*
  compiles it into the tool's own import, so the tool takes the existing
  thing into its state; *converge* means the tool keeps no state and makes
  the thing what the document says whether it existed or not, so `adopt`
  changes nothing; *no* refuses it. See
  [Adopting what exists](dsl/resources.md#adopting-what-exists).
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
    let kinds: Vec<&str> = [
        (capabilities.native_notify, "native"),
        (capabilities.synthetic_notify, "synthetic"),
        (!runner.acts().is_empty(), "Meridian-run"),
    ]
    .into_iter()
    .filter(|(has, _)| *has)
    .map(|(_, kind)| kind)
    .collect();
    let notifies = match kinds.as_slice() {
        [] => "no".to_owned(),
        [one] => (*one).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
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
        "| [{name}](../targets/{name}.md) | {category} | {types} | {notifies} | {runtime} | {plan} | {saved} | {apply} | {outputs} | {inspect} | {inputs} | {secrets} | {credentials} | {cloud} | {adopt} | {import} | {schemas} |\n",
        schemas = yes(schemas.is_some()),
        saved = if runner.saves_plans() {
            "yes"
        } else if operations.plan && operations.apply {
            "guarded"
        } else {
            "no"
        },
        credentials = yes(capabilities.credentials),
        cloud = yes(capabilities.cloud_credentials),
        name = emitter.name(),
        category = emitter.category(),
        types = types.join(", "),
        runtime = yes(capabilities.runtime_condition),
        plan = yes(operations.plan),
        apply = yes(operations.apply),
        outputs = yes(operations.outputs),
        inspect = yes(runner.inspects()),
        inputs = yes(runner.accepts_inputs()),
        secrets = yes(capabilities.secrets),
        adopt = match capabilities.adopt {
            Adoption::None => "no",
            other => other.as_str(),
        },
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
    let mut debug: Vec<String> = meridian_all_targets::ALL
        .iter()
        .map(|target| debug_row(target.emitter().name(), target.runner().as_ref()))
        .collect();
    debug.sort();
    format!(
        "{INTRODUCTION}| Target | Category | Resource types | notifies | runtimeWhen | plan | saved plan | apply | outputs | inspect | inputs | secrets | credentials | cloud | adopt | import | schemas |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n{}",
        rows.concat(),
    ) + DEBUG_INTRODUCTION
        + "| Target | 1 | 2 | 3 |\n|---|---|---|---|\n"
        + &debug.concat()
}

const DEBUG_INTRODUCTION: &str = "
## `--tool-debug`

What each level of [`--tool-debug`](cli.md#asking-a-tool-for-more-output)
adds to the target's tool. A level above the last one a target has uses its
last; *none* means the target has no debug output and the flag is warned
about.

";

fn debug_row(name: &str, runner: &dyn Runner) -> String {
    let levels: Vec<String> =
        meridian_sdk::debug::describe_all(runner.debug_levels(), runner.more_debug_levels())
            .iter()
            .map(|level| format!("`{}`", level.replace('|', "\\|")))
            .collect();
    let cell = |n: usize| match (levels.get(n), levels.last()) {
        (Some(level), _) => level.clone(),
        (None, Some(_)) => format!("as {}", levels.len()),
        (None, None) => "none".to_owned(),
    };
    format!(
        "| [{name}](../targets/{name}.md) | {} | {} | {} |\n",
        cell(0),
        cell(1),
        cell(2)
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
