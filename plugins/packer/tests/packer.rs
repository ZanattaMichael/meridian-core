//! The plugin against the real `packer`.
//!
//! These tests need Packer installed, so they are ignored by default and run
//! by the `packer` workflow, and by hand with:
//!
//! ```text
//! cargo test -p meridian-target-packer --test packer -- --ignored
//! ```
//!
//! `MERIDIAN_PACKER` names the program when it is not `packer` on `PATH`.
//! Nothing here needs a cloud account, a Packer plugin or the network: the
//! builds use the null builder and run their steps on this machine.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use meridian_plugin_testkit::{graph_from_file, graph_from_text, scratch, KeptLog};
use meridian_sdk::{Artifact, Data, Emitter, RunContext, Runner, Value};
use meridian_target_packer::{Packer, PackerRunner, PROGRAM_VARIABLE};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn example(name: &str) -> Artifact {
    let graph = graph_from_file(&format!("examples/packer/{name}.yaml"), "packer");
    Packer.emit(&graph, &Data::new()).unwrap().artifact
}

fn document(spec: &str) -> Artifact {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: img\nspec:\n  target: packer\n{spec}"
    );
    Packer
        .emit(
            &graph_from_text("image.yaml", &text, "packer"),
            &Data::new(),
        )
        .unwrap()
        .artifact
}

fn context(dir: &Path, inputs: &[(&str, &str)]) -> RunContext {
    RunContext {
        document: "hello".into(),
        work_dir: dir.join("work"),
        inputs: inputs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect(),
        ..RunContext::default()
    }
}

/// Every example is a template Packer parses: the block structure, the
/// labels and every expression in it. Checking only the syntax needs neither
/// the plugins an example requires nor an account to build it in.
#[test]
#[ignore = "needs packer installed"]
fn packer_accepts_the_syntax_of_every_example() {
    let program = PackerRunner::from_env().program;
    for name in ["web-image", "app-container", "hello"] {
        let dir = scratch(TMP, &format!("syntax-{name}"));
        for (path, text) in &example(name).files {
            fs::write(dir.join(path), text).unwrap();
        }
        let output = Command::new(&program)
            .args(["validate", "-syntax-only", "."])
            .current_dir(&dir)
            .env_remove(PROGRAM_VARIABLE)
            .output()
            .expect("packer runs");
        assert!(
            output.status.success(),
            "{name}:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "needs packer installed"]
fn the_hello_example_is_planned_built_and_read_back_by_packer_itself() {
    let dir = scratch(TMP, "real-hello");
    let runner = PackerRunner::from_env();
    let artifact = example("hello");
    let context = context(&dir, &[("greeting", "good morning")]);

    let plan = runner
        .plan(&context, &artifact, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].to_string(), "create box: null image");
    assert!(
        !dir.join("work/steps.txt").exists(),
        "a plan runs no step of the build"
    );

    let log = KeptLog::default();
    let applied = runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}"));
    // The variable came from the input, the local from the variable, the
    // provisioners ran in dependency order and the post-processors after
    // them, in sequence.
    assert_eq!(
        fs::read_to_string(dir.join("work/steps.txt")).unwrap(),
        "1 good morning from PACKER\n2 null.box\n3 post-processed\n4 and again\n",
        "{:#?}",
        log.text()
    );
    assert_eq!(applied.changes, plan.changes);
    let value = |name: &str| {
        applied
            .outputs
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("no output {name}"))
            .value
            .clone()
    };
    assert_eq!(value("builder"), Value::String("null".into()));
    let Value::String(artifact_id) = value("artifact") else {
        panic!("the artifact id is text");
    };
    assert!(!artifact_id.is_empty());
    assert_eq!(
        value("summary"),
        Value::String(format!("built {artifact_id} with null"))
    );
    assert!(!log.text().is_empty(), "packer's output is relayed");

    assert_eq!(runner.outputs(&context).unwrap(), applied.outputs);

    // A second build appends to the manifest; the outputs still describe one
    // run, the last.
    let again = runner
        .apply(&context, &artifact, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(again.changes.len(), 1);
    assert_eq!(again.outputs.len(), 3);
}

#[test]
#[ignore = "needs packer installed"]
fn a_template_packer_rejects_fails_the_plan_with_packers_own_words() {
    let dir = scratch(TMP, "real-rejected");
    let artifact = document(
        "  resources:\n    - id: box\n      type: source.null\n      params:\n        communicator: none\n        no_such_argument: true\n",
    );
    let err = PackerRunner::from_env()
        .plan(&context(&dir, &[]), &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "packer validate failed");
    assert!(err.output.contains("no_such_argument"), "{}", err.output);
}

#[test]
#[ignore = "needs packer installed"]
fn a_failing_provisioner_fails_the_apply_and_runs_the_error_cleanup() {
    let dir = scratch(TMP, "real-failed");
    let artifact = document(
        "  resources:\n    - id: box\n      type: source.null\n      params: {communicator: none}\n    - id: boom\n      type: provisioner.shell-local\n      params: {inline: [\"exit 7\"]}\n    - id: tidy\n      type: error-cleanup-provisioner.shell-local\n      params: {inline: [\"echo cleaned > cleaned.txt\"]}\n  outputs:\n    - name: artifact\n      value: ${box.artifact_id}\n",
    );
    let context = context(&dir, &[]);
    let err = PackerRunner::from_env()
        .apply(&context, &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "packer build failed");
    assert_ne!(err.exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(dir.join("work/cleaned.txt"))
            .unwrap()
            .trim(),
        "cleaned"
    );
    // Nothing was built, so there is nothing to read back.
    assert!(PackerRunner::from_env().outputs(&context).is_err());
}
