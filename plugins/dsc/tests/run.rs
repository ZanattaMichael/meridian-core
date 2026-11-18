//! The running half, against a stand-in for `pwsh`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner};
use meridian_target_dsc::DscRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("Datum.yml", "PipelineRunnerSettings: {}\n"),
        ("Projects/Present/web.yml", "resources: []\n"),
    ])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that appends the script it was handed to `calls` and answers
/// by the mode the script asks for.
fn runner(dir: &Path, cases: &str) -> DscRunner {
    let program = stand_in(
        dir,
        "pwsh",
        &format!(
            "echo \"$5\" >> \"{dir}/calls\"\ncase \"$5\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    DscRunner {
        program: program.to_str().unwrap().to_owned(),
        module: "Dsc.PipelineRunner".into(),
    }
}

fn result(results: &str) -> String {
    format!(
        "echo 'Processing node: web'; echo 'MERIDIAN-RESULT {{\"Status\":\"Completed\",\"Configurations\":[{{\"Status\":\"Completed\",\"Results\":[{results}]}}]}}'"
    )
}

const OK: &str = r#"{"ResourceType":"PSDscResources/Service","InstanceName":"w3svc","Status":"OK","ErrorMessage":null}"#;
const DRIFT: &str = r#"{"ResourceType":"PSDscResources/File","InstanceName":"site_config","Status":"FAIL","ErrorMessage":"not in the desired state"}"#;
const SET: &str = r#"{"ResourceType":"PSDscResources/File","InstanceName":"site_config","Status":"OK","ErrorMessage":null}"#;

#[test]
fn a_plan_is_the_runners_test_mode() {
    let dir = scratch(TMP, "plan");
    let cases = format!("*'-Mode Test'*) {} ;;", result(&format!("{OK},{DRIFT}")));
    let plan = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        ["update site_config: PSDscResources/File: not in the desired state"]
    );
    assert!(!plan.unknown);

    let script = fs::read_to_string(dir.join("calls")).unwrap();
    assert!(
        script.starts_with("$ErrorActionPreference = 'Stop'; $module = @(Import-Module 'Dsc.PipelineRunner' -PassThru)[0]; $run = Invoke-DscRunner -ConfigurationSourcePath '"),
        "{script}"
    );
    assert!(
        script.replace('\\', "/").contains("/work' -Mode Test;"),
        "{script}"
    );
    assert!(dir.join("work/Projects/Present/web.yml").exists());
}

/// What an apply's PowerShell prints when the runner is too old to say what
/// had drifted: the `Test` it runs first, then the `Set`.
fn test_then_set(test: &str, set: &str) -> String {
    format!(
        "echo 'MERIDIAN-TEST {{\"Configurations\":[{{\"Status\":\"Completed\",\"Results\":[{test}]}}]}}'; {}",
        result(set)
    )
}

#[test]
fn an_apply_is_one_powershell_that_sets_and_reports_what_had_drifted() {
    let dir = scratch(TMP, "apply");
    let set_said = r#"{"ResourceType":"PSDscResources/Service","InstanceName":"w3svc","Status":"OK","ErrorMessage":null,"Drifted":false,"SetInvoked":false},{"ResourceType":"PSDscResources/File","InstanceName":"site_config","Status":"OK","ErrorMessage":null,"Drifted":true,"SetInvoked":true}"#;
    let cases = format!("*'-Mode Set'*) {} ;;", result(set_said));
    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();

    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 1, "{calls}");
    // The Test runs only when the module is too old to report Drifted.
    assert!(
        calls.contains("Get-Command -Name Resolve-DscV3ResourcePath")
            && calls.contains("if (-not $tellsDrift) {"),
        "{calls}"
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].resource, "site_config");
}

#[test]
fn an_apply_with_an_older_runner_tests_first_in_the_same_powershell() {
    let dir = scratch(TMP, "apply-older");
    let cases = format!(
        "*'-Mode Set'*) {} ;;",
        test_then_set(&format!("{OK},{DRIFT}"), &format!("{OK},{SET}"))
    );
    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].resource, "site_config");
}

/// DscPipelineRunner 1.4.0 gives `"Drifted": null` for a resource it never
/// tested, such as one a condition skipped: that is an answer, not a runner
/// too old to give one.
#[test]
fn an_apply_takes_a_skipped_resources_null_drift_as_no_change() {
    let dir = scratch(TMP, "apply-skipped");
    let set_said = r#"{"ResourceType":"PSDscResources/File","InstanceName":"site_config","Status":"OK","ErrorMessage":null,"Drifted":true,"SetInvoked":true},{"ResourceType":"PSDscResources/Service","InstanceName":"w3svc","Status":"SKIP","ErrorMessage":"Resource skipped due to preCondition","Drifted":null,"SetInvoked":false}"#;
    let cases = format!("*'-Mode Set'*) {} ;;", result(set_said));
    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].resource, "site_config");
}

#[test]
fn an_apply_whose_set_says_nothing_about_drift_is_an_error() {
    let dir = scratch(TMP, "apply-unknown");
    let cases = format!("*'-Mode Set'*) {} ;;", result(&format!("{OK},{SET}")));
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "DscPipelineRunner's Set result does not say whether \"w3svc\" had drifted, and no Test was run first"
    );
}

#[test]
fn a_resource_that_could_not_be_set_fails_the_apply() {
    let dir = scratch(TMP, "failed");
    let cases = format!(
        "*'-Mode Set'*) {} ;;",
        test_then_set(
            &format!("{OK},{DRIFT}"),
            &format!(
                "{OK},{}",
                DRIFT.replace("not in the desired state", "Access is denied")
            )
        )
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "DscPipelineRunner could not set \"site_config\"");
    assert_eq!(
        err.output,
        "PSDscResources/File/site_config: Access is denied"
    );
    assert!(err.changes.is_empty());
}

#[test]
fn a_run_that_throws_keeps_what_powershell_said() {
    let dir = scratch(TMP, "throws");
    let cases = "*) echo 'Invoke-DscRunner: Datum compilation failed' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "DscPipelineRunner could not run the configuration");
    assert!(
        err.output.contains("Datum compilation failed"),
        "{}",
        err.output
    );
}
