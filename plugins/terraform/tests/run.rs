//! The running half, against a stand-in for `terraform`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, Value};
use meridian_target_terraform::TerraformRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[("main.tf.json", "{}\n")])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-vm".into(),
        work_dir: dir.join("work"),
        inputs: [("region".to_owned(), Value::String("us-east-1".into()))].into(),
        ..RunContext::default()
    }
}

/// A stand-in that appends each invocation to `calls` and answers by
/// subcommand.
fn runner(dir: &Path, cases: &str) -> TerraformRunner {
    let program = stand_in(
        dir,
        "terraform",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\necho \"$TF_VAR_region\" > \"{dir}/region\"\ncase \"$1\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    TerraformRunner {
        program: program.to_str().unwrap().to_owned(),
    }
}

const HAPPY: &str = r#"init) echo 'Terraform has been successfully initialized!' ;;
plan) echo 'Plan: 1 to add, 0 to change, 0 to destroy.' ;;
show) echo '{"resource_changes":[{"address":"aws_instance.web_vm","mode":"managed","type":"aws_instance","name":"web_vm","change":{"actions":["create"]}}]}' ;;
apply) echo 'Apply complete! Resources: 1 added, 0 changed, 0 destroyed.' ;;
output) echo '{"web_vm_ip":{"sensitive":false,"type":"string","value":"203.0.113.7"},"admin_password":{"sensitive":true,"type":"string","value":"hunter2"}}' ;;"#;

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_plan_initialises_plans_and_reads_the_saved_plan() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "init -input=false -no-color",
            "plan -input=false -no-color -out=meridian.tfplan",
            "show -json meridian.tfplan",
        ]
    );
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].to_string(), "create web_vm: aws_instance");
    assert!(!plan.unknown);
    // The JSON is data: only what Terraform says to a person is relayed.
    assert_eq!(
        log.text(),
        [
            "Terraform has been successfully initialized!",
            "Plan: 1 to add, 0 to change, 0 to destroy."
        ]
    );
    assert!(dir.join("work/main.tf.json").exists());
    assert_eq!(
        fs::read_to_string(dir.join("region")).unwrap().trim(),
        "us-east-1"
    );
}

#[test]
fn an_apply_applies_the_plan_it_just_showed_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, HAPPY)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir)[3..],
        [
            "apply -input=false -no-color meridian.tfplan",
            "output -json -no-color"
        ]
    );
    assert_eq!(applied.changes.len(), 1);
    let ip = applied
        .outputs
        .iter()
        .find(|o| o.name == "web_vm_ip")
        .unwrap();
    assert_eq!(ip.value, Value::String("203.0.113.7".into()));
    assert!(!ip.sensitive);
    assert!(applied
        .outputs
        .iter()
        .any(|o| o.name == "admin_password" && o.sensitive));
}

#[test]
fn outputs_are_read_without_planning_anything() {
    let dir = scratch(TMP, "outputs");
    let err = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );

    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/main.tf.json"), "{}").unwrap();
    let outputs = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap();
    assert_eq!(calls(&dir), ["output -json -no-color"]);
    assert_eq!(outputs.len(), 2);
}

#[test]
fn a_failed_step_says_which_and_stops_there() {
    let dir = scratch(TMP, "failed");
    let cases =
        "init) echo ok ;;\nplan) echo 'Error: No valid credential sources found' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "terraform plan failed");
    assert_eq!(err.exit_code, Some(1));
    assert!(err.output.contains("No valid credential sources found"));
    assert!(err
        .command
        .ends_with("plan -input=false -no-color -out=meridian.tfplan"));
    assert_eq!(calls(&dir).len(), 2, "nothing runs after the failure");
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, HAPPY)
        .plan(
            &context(&dir),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no main.tf.json; it was not compiled by this plugin"
    );
}
