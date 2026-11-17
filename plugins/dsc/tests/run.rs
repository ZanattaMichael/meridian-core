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
        script.starts_with("$ErrorActionPreference = 'Stop'; Import-Module 'Dsc.PipelineRunner'; $run = Invoke-DscRunner -ConfigurationSourcePath '"),
        "{script}"
    );
    assert!(
        script.replace('\\', "/").contains("/work' -Mode Test;"),
        "{script}"
    );
    assert!(dir.join("work/Projects/Present/web.yml").exists());
}

#[test]
fn an_apply_tests_then_sets_and_reports_what_had_drifted() {
    let dir = scratch(TMP, "apply");
    let cases = format!(
        "*'-Mode Test'*) {} ;;\n*'-Mode Set'*) {} ;;",
        result(&format!("{OK},{DRIFT}")),
        result(&format!("{OK},{SET}"))
    );
    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let modes: Vec<bool> = fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(|l| l.contains("-Mode Set"))
        .collect();
    assert_eq!(modes, [false, true]);
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].resource, "site_config");
}

#[test]
fn a_resource_that_could_not_be_set_fails_the_apply() {
    let dir = scratch(TMP, "failed");
    let cases = format!(
        "*'-Mode Test'*) {} ;;\n*'-Mode Set'*) {} ;;",
        result(&format!("{OK},{DRIFT}")),
        result(&format!(
            "{OK},{}",
            DRIFT.replace("not in the desired state", "Access is denied")
        ))
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
