//! The running half, against a stand-in for `az`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, secrets, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, SecretRef, Value};
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

// ---- secrets ------------------------------------------------------------

/// A stand-in that also keeps a copy of the parameters file it is given,
/// its mode and the environment.
fn runner_keeping_parameters(dir: &Path, cases: &str) -> BicepRunner {
    let program = stand_in(
        dir,
        "az",
        &format!(
            "keep() {{\n  while [ $# -gt 0 ]; do\n    case \"$1\" in\n      --parameters)\n        f=\"${{2#@}}\"\n        echo \"$f\" > \"{dir}/secrets-path\"\n        cp \"$f\" \"{dir}/secrets.json\"\n        stat -c %a \"$f\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$f\" > \"{dir}/mode\"\n        shift;;\n    esac\n    shift\n  done\n}}\nkeep \"$@\"\nenv > \"{dir}/env\"\necho \"$@\" >> \"{dir}/calls\"\ncase \"$3\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    BicepRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

fn with_secret(dir: &Path) -> RunContext {
    let mut context = context(dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2\"\\")]);
    context
}

#[test]
fn secrets_reach_azure_as_a_private_parameters_file_and_nowhere_else() {
    let dir = scratch(TMP, "secrets");
    let log = KeptLog::default();
    runner_keeping_parameters(&dir, &format!("{WHAT_IF}\n{CREATE}"))
        .apply(&with_secret(&dir), &compiled(), &log)
        .unwrap();
    let calls = calls(&dir);
    assert_eq!(calls.len(), 2, "{calls:?}");
    // Both the preview and the deployment read the same file.
    for (call, head) in calls.iter().zip(["what-if", "create"]) {
        assert!(
            call.starts_with(&format!("{head} --template-file main.bicep --parameters @")),
            "{call}"
        );
        assert!(!call.contains("hunter2"), "{call}");
    }
    assert_eq!(
        fs::read_to_string(dir.join("secrets.json")).unwrap(),
        "{\"$schema\":\"https://schema.management.azure.com/schemas/2019-04-01/deploymentParameters.json#\",\"contentVersion\":\"1.0.0.0\",\"parameters\":{\"meridian_secret_db_password\":{\"value\":\"hunter2\\\"\\\\\"}}}\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("hunter2"));
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
    assert!(!fs::read_to_string(dir.join("work/main.bicep"))
        .unwrap()
        .contains("hunter2"));
    let path = fs::read_to_string(dir.join("secrets-path")).unwrap();
    assert!(
        !Path::new(path.trim()).parent().unwrap().exists(),
        "{path} was left behind"
    );
}

#[test]
fn a_run_without_secrets_names_no_parameters_file() {
    let dir = scratch(TMP, "no-secrets");
    runner_keeping_parameters(&dir, WHAT_IF)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        ["what-if --template-file main.bicep --no-pretty-print --output json"]
    );
    assert!(!dir.join("secrets.json").exists());
}

const RG: &str = "/subscriptions/0000/resourceGroups/web-prod/providers";

fn shown_record() -> String {
    format!(
        r#"show) echo '{{"properties":{{"outputResources":[
  {{"id":"{RG}/Microsoft.Network/virtualNetworks/web-vnet"}},
  {{"id":"{RG}/Microsoft.Network/virtualNetworks/web-vnet/subnets/a"}},
  {{"id":"{RG}/Microsoft.Network/publicIPAddresses/web-ip"}}]}}}}' ;;"#
    )
}

#[test]
fn a_destroy_deletes_what_the_record_says_children_first_then_the_record() {
    let dir = scratch(TMP, "destroy");
    let runner = runner(&dir, &shown_record());
    assert!(runner.destroys());
    let removed = runner
        .destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "show --output json".to_owned(),
            format!(
                "resource delete --ids {RG}/Microsoft.Network/virtualNetworks/web-vnet/subnets/a"
            ),
            format!("resource delete --ids {RG}/Microsoft.Network/publicIPAddresses/web-ip"),
            format!("resource delete --ids {RG}/Microsoft.Network/virtualNetworks/web-vnet"),
            "delete".to_owned(),
        ]
    );
    let said: Vec<String> = removed.iter().map(ToString::to_string).collect();
    assert_eq!(
        said[1],
        "delete web_ip: Microsoft.Network/publicIPAddresses"
    );
    assert_eq!(said.len(), 3);
}

#[test]
fn a_destroy_preview_deletes_nothing_and_a_missing_deployment_has_nothing_to_remove() {
    let dir = scratch(TMP, "destroy-preview");
    let plan = runner(&dir, &shown_record())
        .plan_destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(calls(&dir), ["show --output json"]);
    assert_eq!(plan.changes.len(), 3);

    let dir = scratch(TMP, "destroy-missing");
    let removed = runner(
        &dir,
        "show) echo \"ERROR: (DeploymentNotFound) Deployment 'web-vm' could not be found.\" >&2; exit 3 ;;",
    )
    .destroy(&context(&dir), &compiled(), &KeptLog::default())
    .unwrap();
    assert!(removed.is_empty());
    assert_eq!(calls(&dir), ["show --output json"]);
}
