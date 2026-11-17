//! The running half, against a stand-in for `az`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, Value};
use meridian_target_bicep::BicepRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("main.bicep", "// empty\n"),
        (
            "deployment.json",
            r#"{"name":"web-vm","resourceGroup":"web-prod","subscription":"","resources":{"microsoft.network/publicipaddresses/web-ip":"web_ip"}}"#,
        ),
    ])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-vm".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that appends each call to `calls` and answers by operation
/// (`$3`, after `deployment group`).
fn runner(dir: &Path, cases: &str) -> BicepRunner {
    let program = stand_in(
        dir,
        "az",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\ncase \"$3\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    BicepRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(|l| {
            l.replace("deployment group ", "")
                .replace(" --resource-group web-prod --name web-vm", "")
        })
        .collect()
}

const WHAT_IF: &str = r#"what-if) echo '{"status":"Succeeded","changes":[
 {"resourceId":"/subscriptions/0000/resourceGroups/web-prod/providers/Microsoft.Network/publicIPAddresses/web-ip","changeType":"Create"},
 {"resourceId":"/subscriptions/0000/resourceGroups/web-prod/providers/Microsoft.Network/virtualNetworks/web-vnet","changeType":"Modify"},
 {"resourceId":"/subscriptions/0000/resourceGroups/web-prod/providers/Microsoft.Storage/storageAccounts/old","changeType":"Ignore"},
 {"resourceId":"/subscriptions/0000/resourceGroups/web-prod/providers/Microsoft.Network/networkInterfaces/web-nic","changeType":"NoChange"}]}' ;;"#;

const CREATE: &str = r#"create) echo '{"properties":{"provisioningState":"Succeeded","outputs":{"web_vm_ip":{"type":"String","value":"203.0.113.7"},"admin_key":{"type":"SecureString","value":"hunter2"}}}}' ;;"#;

#[test]
fn a_plan_is_a_what_if() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, WHAT_IF)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        ["what-if --template-file main.bicep --no-pretty-print --output json"]
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            // Named by the document's id where the document named the resource.
            "create web_ip: Microsoft.Network/publicIPAddresses",
            "update Microsoft.Network/virtualNetworks/web-vnet: Microsoft.Network/virtualNetworks",
        ]
    );
    assert!(!plan.unknown);
    assert_eq!(
        log.text()[0],
        "resource group web-prod: asking Azure what would change"
    );
    assert!(dir.join("work/main.bicep").exists());
}

#[test]
fn a_resource_azure_cannot_analyse_makes_the_plan_unknown() {
    let dir = scratch(TMP, "unknown");
    let cases = r#"what-if) echo '{"changes":[{"resourceId":"/subscriptions/0/resourceGroups/rg/providers/Microsoft.Web/sites/x","changeType":"Deploy"}]}' ;;"#;
    let plan = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.unknown);
    assert!(!plan.in_sync());
}

#[test]
fn an_apply_previews_then_deploys_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, &format!("{WHAT_IF}\n{CREATE}"))
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir)[1..],
        ["create --template-file main.bicep --output json"]
    );
    assert_eq!(applied.changes.len(), 2);
    let ip = applied
        .outputs
        .iter()
        .find(|o| o.name == "web_vm_ip")
        .unwrap();
    assert_eq!(ip.value, Value::String("203.0.113.7".into()));
    assert!(applied
        .outputs
        .iter()
        .any(|o| o.name == "admin_key" && o.sensitive));
}

#[test]
fn a_failed_deployment_keeps_what_azure_said() {
    let dir = scratch(TMP, "failed");
    let cases = format!(
        "{WHAT_IF}\ncreate) echo 'ERROR: {{\"code\": \"InvalidTemplateDeployment\"}}' >&2; exit 1 ;;"
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "az could not complete the deployment");
    assert!(err.output.contains("InvalidTemplateDeployment"));
}

#[test]
fn outputs_are_read_from_the_last_deployment() {
    let dir = scratch(TMP, "outputs");
    let az = runner(&dir, &CREATE.replace("create)", "show)"));
    assert!(az.outputs(&context(&dir)).is_err());
    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(
        dir.join("work/deployment.json"),
        &compiled().files["deployment.json"],
    )
    .unwrap();
    let outputs = az.outputs(&context(&dir)).unwrap();
    assert_eq!(calls(&dir), ["show --output json"]);
    assert_eq!(outputs.len(), 2);
}
