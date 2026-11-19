//! The running half, and the schema export, against the real `pulumi`.
//!
//! Every other test of the runner uses a stand-in, which proves the runner
//! does what it was written to do. This one proves that what it was written
//! to do is what Pulumi wants: that the compiled program is one Pulumi
//! accepts, that the commands exist and print what the runner reads.
//!
//! It needs `pulumi` on `PATH` and a network to fetch the random provider,
//! so it runs only when `MERIDIAN_TEST_REAL_PULUMI` is set, as the `pulumi`
//! workflow does. It needs no cloud account: state is kept in a scratch
//! directory and the random provider touches nothing outside it.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::path::Path;

use meridian_plugin_testkit::{
    graph_from_file, graph_from_text, scratch, secret_in_param, secrets, KeptLog,
};
use meridian_sdk::{
    Artifact, ChangeAction, Data, Emitter, Output, ParamKind, Plan, RunContext, Runner,
    SchemaRequest, SchemaSource, Value,
};
use meridian_target_pulumi::{Pulumi, PulumiRunner, PulumiSchemas};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Whether the test should run, said once per test when it does not.
fn enabled(test: &str) -> bool {
    let enabled = std::env::var_os("MERIDIAN_TEST_REAL_PULUMI").is_some_and(|v| !v.is_empty());
    if !enabled {
        eprintln!("{test}: skipped; set MERIDIAN_TEST_REAL_PULUMI=1 to run it against pulumi");
    }
    enabled
}

/// The runner as a machine with nothing configured would have it: state
/// beside the project, behind an empty passphrase.
fn runner() -> PulumiRunner {
    PulumiRunner {
        program: std::env::var("MERIDIAN_PULUMI").unwrap_or_else(|_| "pulumi".to_owned()),
        extra_args: Vec::new(),
        aws: "aws".to_owned(),
        backend_chosen: false,
        passphrase_chosen: false,
    }
}

fn context(dir: &Path, inputs: &[(&str, &str)]) -> RunContext {
    RunContext {
        document: "pets".into(),
        work_dir: dir.join("work"),
        inputs: inputs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect::<Data>(),
        ..RunContext::default()
    }
}

fn pets() -> Artifact {
    let graph = graph_from_file("examples/pulumi/pets.yaml", "pulumi");
    Pulumi.emit(&graph, &Data::new()).unwrap().artifact
}

/// An infrastructure document called `pets` with the given `spec` body.
fn compiled(spec: &str) -> Artifact {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: pets\nspec:\n  target: pulumi\n{spec}"
    );
    let graph = graph_from_text("infra.yaml", &text, "pulumi");
    Pulumi.emit(&graph, &Data::new()).unwrap().artifact
}

/// A plan's changes as sorted `action id` lines.
fn changes(plan: &Plan) -> Vec<String> {
    let mut lines: Vec<String> = plan
        .changes
        .iter()
        .map(|c| format!("{} {}", c.action, c.resource))
        .collect();
    lines.sort();
    lines
}

fn output<'a>(outputs: &'a [Output], name: &str) -> &'a Output {
    outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name:?} among {outputs:?}"))
}

/// An output's value as text. Nothing about the output is printed when it is
/// not text: one of them is a generated password.
fn text(output: &Output) -> &str {
    output.value.as_str().expect("the output is a string")
}

#[ignore = "needs a real pulumi; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_document_is_planned_applied_and_changed() {
    if !enabled("a_document_is_planned_applied_and_changed") {
        return;
    }
    let dir = scratch(TMP, "real-lifecycle");
    let runner = runner();
    let log = KeptLog::default();
    let fail = |what: &str, err: meridian_sdk::RunError| -> ! {
        panic!("{what}: {err}\n--- log ---\n{}", log.text().join("\n"))
    };

    // Nothing exists yet: both resources would be created, and nothing else
    // is reported, neither Pulumi's stack nor the provider it loads itself.
    let first = context(&dir, &[]);
    let plan = runner
        .plan(&first, &pets(), &log)
        .unwrap_or_else(|e| fail("the first plan", e));
    assert_eq!(changes(&plan), ["create password", "create pet"]);
    assert!(
        plan.changes
            .iter()
            .all(|c| c.summary.starts_with("random:index")),
        "{:?}",
        plan.changes
    );

    // Applying does what the plan said and reports the outputs.
    let applied = runner
        .apply(&first, &pets(), &log)
        .unwrap_or_else(|e| fail("the first apply", e));
    assert_eq!(applied.changes.len(), 2);
    let name = text(output(&applied.outputs, "pet_name")).to_owned();
    assert!(name.starts_with("web-"), "{name}");
    assert!(!output(&applied.outputs, "pet_name").sensitive);
    // ${pulumi.stack} is Pulumi's own name and was handed over as written.
    assert_eq!(
        text(output(&applied.outputs, "greeting")),
        format!("hello from {name} on dev")
    );
    // The provider marks the password a secret; the runner sees through the
    // mask to the value and remembers that it was masked.
    let generated = output(&applied.outputs, "password");
    assert!(generated.sensitive);
    // Checked without `assert_eq!`, which would print the value on failure.
    assert!(
        text(generated).len() == 24,
        "the password has the wrong length"
    );
    assert!(
        text(generated) != "[secret]",
        "the password is still masked"
    );
    assert!(
        !log.text().iter().any(|l| l.contains(text(generated))),
        "the password reached the log"
    );

    // Nothing changed, so a second plan finds the stack in sync.
    let plan = runner
        .plan(&first, &pets(), &log)
        .unwrap_or_else(|e| fail("the second plan", e));
    assert!(plan.in_sync(), "{:?}", plan.changes);

    // Outputs can be read again without planning anything.
    let read = runner
        .outputs(&first)
        .unwrap_or_else(|e| fail("reading outputs", e));
    assert_eq!(read, applied.outputs);

    // What it manages is reported by resource id, for `meridian retarget`;
    // the stack and the provider are not resources of the document.
    let found = runner
        .identifiers(&first)
        .unwrap_or_else(|e| fail("reading identifiers", e));
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.values().all(|id| !id.is_empty()), "{found:?}");

    // A value handed over from an earlier stage replaces the default of the
    // configuration value the document declares. A pet cannot be renamed, so
    // Pulumi replaces it, and the password is left alone.
    let second = context(&dir, &[("prefix", "db")]);
    let applied = runner
        .apply(&second, &pets(), &log)
        .unwrap_or_else(|e| fail("the apply with an input", e));
    assert_eq!(
        applied
            .changes
            .iter()
            .map(|c| (c.resource.as_str(), c.action))
            .collect::<Vec<_>>(),
        [("pet", ChangeAction::Replace)]
    );
    let renamed = text(output(&applied.outputs, "pet_name"));
    assert!(renamed.starts_with("db-"), "{renamed}");
    assert!(
        text(output(&applied.outputs, "password")) == text(generated),
        "the password was replaced, and it should not have been"
    );

    // A resource taken out of the document is deleted.
    let smaller = compiled(
        "  resources:\n    - id: prefix\n      type: config\n      params: {type: string, default: web}\n    - id: pet\n      type: random:index:RandomPet\n      params:\n        prefix: ${prefix}\n        length: 2\n  outputs:\n    - name: pet_name\n      value: ${pet.id}\n",
    );
    let plan = runner
        .plan(&second, &smaller, &log)
        .unwrap_or_else(|e| fail("the plan without the password", e));
    assert_eq!(changes(&plan), ["delete password"]);

    // A destroy previews what it would remove, removes it, and leaves the
    // stack with nothing to remove.
    let preview = runner
        .plan_destroy(&second, &pets(), &log)
        .unwrap_or_else(|e| fail("the destroy preview", e));
    assert_eq!(changes(&preview), ["delete password", "delete pet"]);
    let removed = runner
        .destroy(&second, &pets(), &log)
        .unwrap_or_else(|e| fail("the destroy", e));
    assert_eq!(removed.len(), 2, "{removed:?}");
    let after = runner
        .plan_destroy(&second, &pets(), &log)
        .unwrap_or_else(|e| fail("the preview after the destroy", e));
    assert!(after.changes.is_empty(), "{:?}", after.changes);
}

#[ignore = "needs a real pulumi; see docs/contributing/real-tool-tests.md"]
#[test]
fn names_options_and_functions_are_ones_pulumi_accepts() {
    if !enabled("names_options_and_functions_are_ones_pulumi_accepts") {
        return;
    }
    let dir = scratch(TMP, "real-program");
    let log = KeptLog::default();
    // A hyphenated id, a named stack, a reference inside longer text, a
    // dependsOn the properties do not imply, a resource option, one of
    // Pulumi's fn:: functions, and text that only looks like a reference.
    let artifact = compiled(
        "  resources:\n    - id: deployment\n      type: stack\n      params: {name: staging}\n    - id: my-pet\n      type: random:index:RandomPet\n      params:\n        length: 2\n        separator: \"_\"\n        resourceOptions:\n          protect: false\n          ignoreChanges: [keepers]\n    - id: tag\n      type: random:index:RandomString\n      params:\n        length: 8\n        special: false\n        keepers:\n          pet: owner-${my-pet.id}\n          literal: $${not_a_reference}\n          joined:\n            fn::join:\n              - \"-\"\n              - [a, b]\n      dependsOn:\n        - resource: my-pet\n    - id: plain\n      type: random:index:RandomInteger\n      params: {min: 1, max: 6}\n      dependsOn:\n        - resource: tag\n  outputs:\n    - name: pet\n      value: ${my-pet.id}\n    - name: keepers\n      value: ${tag.keepers}\n    - name: roll\n      value: ${plain.result}\n",
    );
    let context = context(&dir, &[]);
    let applied = runner()
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("applying: {e}\n--- log ---\n{}", log.text().join("\n")));
    let mut created: Vec<&str> = applied
        .changes
        .iter()
        .map(|c| c.resource.as_str())
        .collect();
    created.sort_unstable();
    assert_eq!(created, ["my-pet", "plain", "tag"]);
    assert!(log.text().iter().any(|l| l.contains("staging")));

    let pet = text(output(&applied.outputs, "pet")).to_owned();
    assert!(pet.contains('_'), "{pet}");
    let keepers = output(&applied.outputs, "keepers")
        .value
        .as_map()
        .expect("keepers is a map")
        .clone();
    assert_eq!(keepers["pet"], Value::String(format!("owner-{pet}")));
    assert_eq!(
        keepers["literal"],
        Value::String("${not_a_reference}".into())
    );
    assert_eq!(keepers["joined"], Value::String("a-b".into()));
    // A number stays a number on its way out.
    let roll = &output(&applied.outputs, "roll").value;
    assert!(matches!(roll, Value::Int(1..=6)), "{roll:?}");
}

#[ignore = "needs a real pulumi; see docs/contributing/real-tool-tests.md"]
#[test]
fn what_pulumi_refuses_comes_back_as_its_own_words() {
    if !enabled("what_pulumi_refuses_comes_back_as_its_own_words") {
        return;
    }
    let dir = scratch(TMP, "real-refused");
    let log = KeptLog::default();
    // The type is well formed, so Meridian compiles it; only Pulumi knows
    // the provider has no such thing.
    let artifact = compiled(
        "  resources:\n    - id: pet\n      type: random:index:NoSuchThing\n      params: {length: 2}\n",
    );
    let err = runner()
        .plan(&context(&dir, &[]), &artifact, &log)
        .expect_err("pulumi should refuse the program");
    assert_eq!(err.msg, "pulumi preview failed", "{err}");
    assert!(err.exit_code.is_some_and(|code| code != 0), "{err}");
    // Printed so the workflow's log shows what a real refusal looks like.
    eprintln!(
        "--- pulumi's refusal ---\n{err}\n--- log ---\n{}",
        log.text().join("\n")
    );
    assert!(
        err.output.contains("NoSuchThing"),
        "the failure does not say what was wrong:\n{err}"
    );
    assert!(
        !err.failures.is_empty() && err.failures[0].cause.as_str() == "invalid-config",
        "{:?}",
        err.failures
    );
}

#[ignore = "needs a real pulumi; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_packages_schemas_are_exported_for_an_editor() {
    if !enabled("the_packages_schemas_are_exported_for_an_editor") {
        return;
    }
    let dir = scratch(TMP, "real-schemas");
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_file("examples/pulumi/pets.yaml", "pulumi")],
    );
    let log = KeptLog::default();
    let export = PulumiSchemas::from_env()
        .export(&request, &log)
        .unwrap_or_else(|e| {
            panic!(
                "exporting schemas: {e}\n--- log ---\n{}",
                log.text().join("\n")
            )
        });
    assert_eq!(export.tool, "pulumi");
    assert!(
        export
            .tool_version
            .starts_with(|c: char| c.is_ascii_digit()),
        "{:?}",
        export.tool_version
    );
    let pinned = export
        .pins
        .get("random")
        .unwrap_or_else(|| panic!("the random package is pinned: {:?}", export.pins));
    assert!(pinned.starts_with(|c: char| c.is_ascii_digit()), "{pinned}");

    // The types the document uses are described property by property.
    let pet = export
        .schemas
        .iter()
        .find(|s| s.r#type == "random:index/randomPet:RandomPet")
        .unwrap_or_else(|| panic!("RandomPet is described: {:?}", export.notes));
    assert_eq!(pet.source, format!("random {pinned}"));
    assert!(!pet.open);
    assert!(pet
        .find_param("length")
        .is_some_and(|p| p.kind == ParamKind::Integer));
    assert!(pet.find_param("id").is_some_and(|p| p.computed));
    assert!(pet.find_param("resourceOptions").is_some());
    let password = export
        .schemas
        .iter()
        .find(|s| s.r#type == "random:index/randomPassword:RandomPassword")
        .unwrap();
    assert!(password.find_param("length").is_some_and(|p| p.required));
    assert!(password
        .find_param("result")
        .is_some_and(|p| p.computed && p.sensitive));

    // The rest of the package is offered by name.
    let other = export
        .schemas
        .iter()
        .find(|s| s.r#type == "random:index/randomUuid:RandomUuid")
        .unwrap();
    assert!(other.open && other.params.is_empty());
}

/// Unlike anything the program or Pulumi prints, so that finding it is
/// finding a leak.
const SECRET: &str = "zzq-Vt9-not-a-real-token-4kLm";

#[ignore = "needs a real pulumi; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_the_program_as_secret_configuration_and_is_in_nothing_kept_in_the_open() {
    if !enabled(
        "a_secret_reaches_the_program_as_secret_configuration_and_is_in_nothing_kept_in_the_open",
    ) {
        return;
    }
    let dir = scratch(TMP, "real-secret");
    let runner = runner();
    // The secret is a random string's `overrideSpecial`, a plain string
    // input the provider hands back as an output of the same name, which
    // Pulumi keeps secret as it kept the input; the string itself is
    // nothing secret, and with `special: false` is not shaped by it. A
    // resource whose ID is made from the secret, such as a pet with a
    // secret prefix, would put it in the open, as Pulumi never keeps an ID
    // secret; and a secret inside a map is not followed into the map that
    // comes back.
    let document = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: pets\nspec:\n  target: pulumi\n  resources:\n    - id: tag\n      type: random:index:RandomString\n      params:\n        length: 8\n        special: false\n        overrideSpecial: placeholder\n  outputs:\n    - name: given\n      value: ${tag.overrideSpecial}\n    - name: tag\n      value: ${tag.result}\n";
    let mut graph = graph_from_text("infra.yaml", document, "pulumi");
    let slot = secret_in_param(&mut graph, "tag", "overrideSpecial", "token");
    let artifact = Pulumi.emit(&graph, &Data::new()).unwrap().artifact;
    assert!(!artifact.files["Pulumi.yaml"].contains(SECRET));

    let mut context = context(&dir, &[]);
    context.secrets = secrets(&[(slot, "token", SECRET)]);
    let log = KeptLog::default();
    let fail = |what: &str, err: meridian_sdk::RunError| -> ! {
        panic!("{what}: {err}\n--- log ---\n{}", log.text().join("\n"))
    };
    let plan = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| fail("the plan", e));
    assert_eq!(changes(&plan), ["create tag"]);
    let applied = runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| fail("the apply", e));
    // The program was given the value: it comes back as the secret output
    // of the same name. The string is nothing secret, so the masking tells
    // the two apart.
    let masking: Vec<String> = applied
        .outputs
        .iter()
        .map(|o| format!("{} sensitive={}", o.name, o.sensitive))
        .collect();
    let given = output(&applied.outputs, "given");
    assert!(
        given.sensitive,
        "the secret input is a secret output; the outputs: {masking:?}"
    );
    assert_eq!(text(given), SECRET);
    let tag = output(&applied.outputs, "tag");
    assert!(!tag.sensitive, "the outputs: {masking:?}");
    assert_eq!(text(tag).len(), 8);
    // Nothing printed holds it, and nothing left in the open: the stack's
    // settings file no longer names the key, and the state holds the value
    // encrypted, as Pulumi keeps every secret.
    assert!(
        !log.text().iter().any(|l| l.contains(SECRET)),
        "the secret reached the log:\n{}",
        log.text().join("\n")
    );
    let mut pending = vec![dir.join("work")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(SECRET),
                    "{} holds the secret in the open",
                    path.display()
                );
                if path.file_name().is_some_and(|n| {
                    n.to_string_lossy().starts_with("Pulumi.") && n != "Pulumi.yaml"
                }) {
                    assert!(
                        !String::from_utf8_lossy(&bytes).contains("meridian_secret_token"),
                        "{} still names the secret after the run",
                        path.display()
                    );
                }
            }
        }
    }
}

#[ignore = "needs a real pulumi; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_replace_notify_replaces_the_notified_resource_when_the_notifier_changes() {
    if !enabled("a_replace_notify_replaces_the_notified_resource_when_the_notifier_changes") {
        return;
    }
    let dir = scratch(TMP, "real-replace");
    let runner = runner();
    let log = KeptLog::default();
    let fail = |what: &str, err: meridian_sdk::RunError| -> ! {
        panic!("{what}: {err}\n--- log ---\n{}", log.text().join("\n"))
    };
    let document = |length: u32| {
        compiled(&format!(
            "  resources:\n    - id: name\n      type: random:index:RandomPet\n      params: {{length: {length}}}\n      notifies:\n        - resource: seed\n          action: replace\n    - id: seed\n      type: random:index:RandomInteger\n      params: {{min: 1, max: 1000000}}\n"
        ))
    };
    let ctx = context(&dir, &[]);
    runner
        .apply(&ctx, &document(2), &log)
        .unwrap_or_else(|e| fail("the first apply", e));
    let plan = runner
        .plan(&ctx, &document(2), &log)
        .unwrap_or_else(|e| fail("the plan with nothing changed", e));
    assert!(plan.in_sync(), "{:?}", plan.changes);

    let applied = runner
        .apply(&ctx, &document(3), &log)
        .unwrap_or_else(|e| fail("the apply with the notifier changed", e));
    let mut replaced: Vec<(&str, ChangeAction)> = applied
        .changes
        .iter()
        .map(|c| (c.resource.as_str(), c.action))
        .collect();
    replaced.sort_by_key(|(id, _)| *id);
    assert_eq!(
        replaced,
        [
            ("name", ChangeAction::Replace),
            ("seed", ChangeAction::Replace)
        ]
    );
    runner
        .destroy(&ctx, &document(3), &log)
        .unwrap_or_else(|e| fail("the destroy", e));
}
