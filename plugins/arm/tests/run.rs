//! The running half, against a stand-in for `az`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, secrets, stand_in, KeptLog};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner, SecretRef, Value};
use meridian_target_arm::ArmRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

const GROUP: &str = r#"{"name":"web-vm-prod","scope":"resourceGroup","resourceGroup":"web-rg","location":"","subscription":"","mode":"Incremental",
"parameters":{"admin_password":"securestring","vm_size":"string"},
"resources":{"web_vnet":{"type":"Microsoft.Network/virtualNetworks","name":"web-vnet"},
"web_db":{"type":"Microsoft.Sql/servers/databases","name":"sql/orders"},
"web_logs":{"type":"Microsoft.Storage/storageAccounts","name":"[uniqueString(resourceGroup().id)]"}}}"#;

const SUBSCRIPTION: &str = r#"{"name":"landing-zone","scope":"subscription","resourceGroup":"","location":"australiaeast","subscription":"prod","mode":"Incremental",
"parameters":{},"resources":{"network_rg":{"type":"Microsoft.Resources/resourceGroups","name":"network-rg"}}}"#;

fn compiled(deployment: &str) -> Artifact {
    artifact(&[
        ("template.json", "{\"resources\": []}\n"),
        ("deployment.json", deployment),
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
fn runner(dir: &Path, cases: &str) -> ArmRunner {
    let program = stand_in(
        dir,
        "az",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\ncase \"$3\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    ArmRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: vec!["--only-show-errors".into()],
        parameters: Vec::new(),
    }
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

const ID: &str = "/subscriptions/0000/resourceGroups/web-rg/providers";

fn what_if(changes: &str) -> String {
    format!(
        "what-if) echo '{{\"status\":\"Succeeded\",\"error\":null,\"changes\":[{changes}]}}' ;;"
    )
}

fn changes() -> String {
    what_if(&format!(
        r#"{{"changeType":"Create","resourceId":"{ID}/Microsoft.Network/virtualNetworks/web-vnet"}},
{{"changeType":"Modify","resourceId":"{ID}/microsoft.sql/servers/SQL/databases/Orders","delta":[{{"path":"sku.name"}},{{"path":"tags.a"}},{{"path":"tags.b"}},{{"path":"tags.c"}},{{"path":"tags.d"}},{{"path":"tags.e"}}]}},
{{"changeType":"Deploy","resourceId":"{ID}/Microsoft.Storage/storageAccounts/abc123"}},
{{"changeType":"Delete","resourceId":"{ID}/Microsoft.Network/publicIPAddresses/old-ip"}},
{{"changeType":"NoChange","resourceId":"{ID}/Microsoft.Network/networkInterfaces/web-nic"}},
{{"changeType":"Ignore","resourceId":"{ID}/Microsoft.KeyVault/vaults/someone-elses"}}"#
    ))
}

const CREATED: &str = r#"create) echo '{"properties":{"provisioningState":"Succeeded","outputs":{"web_vm_ip":{"type":"String","value":"203.0.113.7"},"token":{"type":"SecureString"}}}}' ;;"#;

const COMMON: &str = "--name web-vm-prod --resource-group web-rg --only-show-errors --output json";

#[test]
fn a_plan_is_a_what_if_reported_in_the_documents_own_ids() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, &changes())
        .plan(&context(&dir), &compiled(GROUP), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [format!(
            "deployment group what-if {COMMON} --template-file template.json --mode Incremental --no-pretty-print"
        )]
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            "create web_vnet: Microsoft.Network/virtualNetworks".to_owned(),
            // Azure's ids do not keep the case a template wrote.
            "update web_db: microsoft.sql/servers/databases, changes sku.name, tags.a, tags.b, tags.c, tags.d and 1 more".to_owned(),
            // A name only known when deploying cannot be traced back.
            format!("update {ID}/Microsoft.Storage/storageAccounts/abc123: Microsoft.Storage/storageAccounts, deployed again; the preview cannot tell what changes"),
            format!("delete {ID}/Microsoft.Network/publicIPAddresses/old-ip: Microsoft.Network/publicIPAddresses"),
        ]
    );
    assert!(!plan.unknown);
    assert_eq!(
        log.text(),
        ["deployment web-vm-prod: asking Resource Manager what would change in resource group web-rg"]
    );
    // The artifact is on disk for az to read.
    assert!(dir.join("work/template.json").exists());
}

#[test]
fn a_plan_with_nothing_to_change_is_in_sync() {
    let dir = scratch(TMP, "in-sync");
    let cases = what_if(&format!(
        r#"{{"changeType":"NoChange","resourceId":"{ID}/Microsoft.Network/virtualNetworks/web-vnet"}}"#
    ));
    let plan = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync());
}

#[test]
fn a_resource_the_preview_cannot_see_makes_the_plan_unknown() {
    let dir = scratch(TMP, "unsupported");
    let cases = what_if(&format!(
        r#"{{"changeType":"Unsupported","resourceId":"{ID}/Microsoft.Resources/deployments/nested","unsupportedReason":"nested"}}"#
    ));
    let plan = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap();
    assert!(plan.changes.is_empty());
    assert!(plan.unknown);
    assert!(!plan.in_sync());
}

#[test]
fn an_apply_previews_then_deploys_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
    let log = KeptLog::default();
    let applied = runner(&dir, &format!("{}\n{CREATED}", changes()))
        .apply(&context(&dir), &compiled(GROUP), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            format!("deployment group what-if {COMMON} --template-file template.json --mode Incremental --no-pretty-print"),
            format!("deployment group create {COMMON} --template-file template.json --mode Incremental"),
        ]
    );
    assert_eq!(applied.changes.len(), 4);
    assert_eq!(applied.changes[0].action, ChangeAction::Create);
    assert_eq!(applied.outputs.len(), 2);
    assert_eq!(applied.outputs[1].name, "web_vm_ip");
    assert_eq!(
        applied.outputs[1].value,
        Value::String("203.0.113.7".into())
    );
    assert!(!applied.outputs[1].sensitive);
    // Azure does not return the value of a secure output, and it is marked.
    assert_eq!(applied.outputs[0].name, "token");
    assert_eq!(applied.outputs[0].value, Value::Null);
    assert!(applied.outputs[0].sensitive);
    assert_eq!(
        log.text()[1],
        "deployment web-vm-prod: deploying 4 changes to resource group web-rg"
    );
}

#[test]
fn a_subscription_deployment_uses_its_own_commands_and_a_location() {
    let dir = scratch(TMP, "subscription");
    let cases = format!(
        "{}\n{CREATED}",
        what_if(
            r#"{"changeType":"Create","resourceId":"/subscriptions/0000/resourceGroups/network-rg"}"#
        )
    );
    let log = KeptLog::default();
    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(SUBSCRIPTION), &log)
        .unwrap();
    let common = "--name landing-zone --subscription prod --only-show-errors --output json --template-file template.json --location australiaeast";
    assert_eq!(
        calls(&dir),
        [
            format!("deployment sub what-if {common} --no-pretty-print"),
            format!("deployment sub create {common}"),
        ]
    );
    // A resource group's id has no provider in it and is still traced back.
    assert_eq!(
        applied.changes[0].to_string(),
        "create network_rg: Microsoft.Resources/resourceGroups"
    );
    assert_eq!(
        log.text()[0],
        "deployment landing-zone: asking Resource Manager what would change in the subscription"
    );
}

#[test]
fn parameters_come_from_the_operator_and_from_earlier_stages() {
    let dir = scratch(TMP, "parameters");
    let mut az = runner(&dir, &what_if(""));
    az.parameters = vec!["@prod.parameters.json".into(), "admin_user=ops".into()];
    let mut context = context(&dir);
    // Outputs of earlier stages: two are parameters of this template.
    context
        .inputs
        .insert("vm_size".into(), Value::String("Standard_B2s".into()));
    context
        .inputs
        .insert("admin_password".into(), Value::String("hunter2".into()));
    context.inputs.insert("unrelated".into(), Value::Int(3));
    az.plan(&context, &compiled(GROUP), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [format!(
            "deployment group what-if {COMMON} --template-file template.json --mode Incremental \
             --parameters @prod.parameters.json --parameters admin_user=ops \
             --parameters admin_password=\"hunter2\" --parameters vm_size=\"Standard_B2s\" --no-pretty-print"
        )]
    );
}

#[test]
fn a_secret_parameter_never_appears_in_an_error() {
    let dir = scratch(TMP, "secret");
    let mut az = runner(&dir, "what-if) echo 'InvalidTemplate: no' >&2; exit 1 ;;");
    az.parameters = vec!["operator_secret=s3cr3t".into()];
    let mut context = context(&dir);
    context
        .inputs
        .insert("admin_password".into(), Value::String("hunter2".into()));
    context
        .inputs
        .insert("vm_size".into(), Value::String("Standard_B2s".into()));
    let err = az
        .plan(&context, &compiled(GROUP), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "az could not preview the deployment");
    assert_eq!(err.exit_code, Some(1));
    assert!(err.output.contains("InvalidTemplate: no"), "{}", err.output);
    assert!(!err.command.contains("hunter2"), "{}", err.command);
    assert!(!err.command.contains("s3cr3t"), "{}", err.command);
    // A value that is not a secret is shown, so the command can be rerun.
    assert!(err.command.contains("Standard_B2s"), "{}", err.command);
}

#[test]
fn a_template_resource_manager_rejects_reports_its_reason() {
    let dir = scratch(TMP, "rejected");
    let cases = r#"what-if) echo '{"status":"Failed","error":{"code":"InvalidTemplate","message":"The resource type is not valid."},"changes":[]}' ;;"#;
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "Resource Manager rejected the template: InvalidTemplate: The resource type is not valid."
    );
    // Nothing is deployed after a preview that failed.
    assert_eq!(calls(&dir).len(), 1);
}

#[test]
fn a_deployment_that_fails_is_an_error_with_where_to_look() {
    let dir = scratch(TMP, "failed");
    let cases = format!(
        "{}\ncreate) echo 'DeploymentFailed: quota exceeded' >&2; exit 1 ;;",
        changes()
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap_err();
    assert!(err.msg.starts_with("the deployment failed"), "{}", err.msg);
    assert!(err.output.contains("quota exceeded"), "{}", err.output);
}

#[test]
fn output_that_is_not_json_is_an_error_and_not_a_panic() {
    let dir = scratch(TMP, "garbled");
    let err = runner(&dir, "what-if) echo 'WARNING: upgrade available' ;;")
        .plan(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with("the preview could not be read"),
        "{}",
        err.msg
    );
}

#[test]
fn an_artifact_from_elsewhere_is_refused() {
    let dir = scratch(TMP, "foreign");
    let az = runner(&dir, "");
    let err = az
        .plan(
            &context(&dir),
            &artifact(&[("main.tf.json", "{}")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no template.json and deployment.json; it was not compiled by this plugin"
    );
    for bad in ["not json", "[]", "{\"scope\":\"resourceGroup\"}"] {
        let err = az
            .plan(&context(&dir), &compiled(bad), &KeptLog::default())
            .unwrap_err();
        assert_eq!(
            err.msg, "deployment.json is not one this plugin compiled",
            "{bad}"
        );
    }
    assert!(!dir.join("calls").exists(), "az was never run");
}

#[test]
fn outputs_need_an_earlier_apply_and_then_read_the_record() {
    let dir = scratch(TMP, "outputs");
    let az = runner(
        &dir,
        r#"show) echo '{"properties":{"outputs":{"web_vm_ip":{"type":"String","value":"203.0.113.7"}}}}' ;;"#,
    );
    let err = az.outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );
    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/deployment.json"), GROUP).unwrap();
    let outputs = az.outputs(&context(&dir)).unwrap();
    assert_eq!(calls(&dir), [format!("deployment group show {COMMON}")]);
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].name, "web_vm_ip");

    let gone = runner(&dir, "show) echo 'DeploymentNotFound' >&2; exit 3 ;;");
    let err = gone.outputs(&context(&dir)).unwrap_err();
    assert_eq!(err.msg, "az could not read the deployment web-vm-prod");
    assert_eq!(err.exit_code, Some(3));
}

#[test]
fn a_missing_az_says_so() {
    let dir = scratch(TMP, "missing");
    let az = ArmRunner {
        program: dir.join("no-such-az").to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
        parameters: Vec::new(),
    };
    let err = az
        .plan(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap_err();
    assert!(err.msg.contains("no-such-az"), "{}", err.msg);
}

// ---- secrets ------------------------------------------------------------

/// A stand-in that also keeps a copy of the parameters file it is given,
/// its mode and the environment.
fn runner_keeping_parameters(dir: &Path, cases: &str) -> ArmRunner {
    let program = stand_in(
        dir,
        "az",
        &format!(
            "keep() {{\n  while [ $# -gt 0 ]; do\n    case \"$1\" in\n      --parameters)\n        f=\"${{2#@}}\"\n        echo \"$f\" > \"{dir}/secrets-path\"\n        cp \"$f\" \"{dir}/secrets.json\"\n        stat -c %a \"$f\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$f\" > \"{dir}/mode\"\n        shift;;\n    esac\n    shift\n  done\n}}\nkeep \"$@\"\nenv > \"{dir}/env\"\necho \"$@\" >> \"{dir}/calls\"\ncase \"$3\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    ArmRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: vec!["--only-show-errors".into()],
        parameters: Vec::new(),
    }
}

fn with_secret(dir: &Path) -> RunContext {
    let mut context = context(dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2\"\\")]);
    context
}

#[test]
fn secrets_reach_resource_manager_as_a_private_parameters_file_and_nowhere_else() {
    let dir = scratch(TMP, "secrets");
    let log = KeptLog::default();
    runner_keeping_parameters(&dir, &format!("{}\n{CREATED}", what_if("")))
        .apply(&with_secret(&dir), &compiled(GROUP), &log)
        .unwrap();
    let calls = calls(&dir);
    assert_eq!(calls.len(), 2, "{calls:?}");
    // Both the preview and the deployment read the same file.
    for (call, head) in calls.iter().zip(["what-if", "create"]) {
        assert!(
            call.starts_with(&format!(
                "deployment group {head} {COMMON} --template-file template.json --parameters @"
            )),
            "{call}"
        );
        assert!(call.contains(" --mode Incremental"), "{call}");
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
    assert!(!fs::read_to_string(dir.join("work/template.json"))
        .unwrap()
        .contains("hunter2"));
    let path = fs::read_to_string(dir.join("secrets-path")).unwrap();
    assert!(
        !Path::new(path.trim()).parent().unwrap().exists(),
        "{path} was left behind"
    );
}

fn shown_record() -> String {
    format!(
        r#"show) echo '{{"properties":{{"outputResources":[
  {{"id":"{ID}/Microsoft.Network/virtualNetworks/web-vnet"}},
  {{"id":"{ID}/Microsoft.Sql/servers/sql/databases/orders"}}]}}}}' ;;"#
    )
}

#[test]
fn a_destroy_deletes_what_the_record_says_children_first_then_the_record() {
    let dir = scratch(TMP, "destroy");
    let runner = runner(
        &dir,
        &format!(
            "{}\n--ids) case \"$4\" in *orders) echo 'ResourceNotFound' >&2; exit 3 ;; esac ;;",
            shown_record()
        ),
    );
    assert!(runner.destroys());
    let removed = runner
        .destroy(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "deployment group show --name web-vm-prod --resource-group web-rg --only-show-errors --output json".to_owned(),
            format!("resource delete --ids {ID}/Microsoft.Sql/servers/sql/databases/orders --only-show-errors"),
            format!("resource delete --ids {ID}/Microsoft.Network/virtualNetworks/web-vnet --only-show-errors"),
            "deployment group delete --name web-vm-prod --resource-group web-rg --only-show-errors --output json".to_owned(),
        ]
    );
    let resources: Vec<(&str, ChangeAction)> = removed
        .iter()
        .map(|c| (c.resource.as_str(), c.action))
        .collect();
    assert_eq!(
        resources,
        [
            ("web_db", ChangeAction::Delete),
            ("web_vnet", ChangeAction::Delete)
        ]
    );
}

#[test]
fn a_destroy_preview_deletes_nothing_and_a_missing_deployment_has_nothing_to_remove() {
    let dir = scratch(TMP, "destroy-preview");
    let plan = runner(&dir, &shown_record())
        .plan_destroy(&context(&dir), &compiled(GROUP), &KeptLog::default())
        .unwrap();
    assert_eq!(calls(&dir).len(), 1);
    assert_eq!(plan.changes.len(), 2);

    let dir = scratch(TMP, "destroy-missing");
    let removed = runner(
        &dir,
        "show) echo \"ERROR: (DeploymentNotFound) Deployment 'web-vm-prod' could not be found.\" >&2; exit 3 ;;",
    )
    .destroy(&context(&dir), &compiled(GROUP), &KeptLog::default())
    .unwrap();
    assert!(removed.is_empty());
    assert_eq!(calls(&dir).len(), 1);
}

#[test]
fn a_resource_that_cannot_be_deleted_stops_the_destroy_and_says_what_went() {
    let dir = scratch(TMP, "destroy-fails");
    let err = runner(
        &dir,
        &format!(
            "{}\n--ids) case \"$4\" in *web-vnet) echo 'InUseSubnetCannotBeDeleted' >&2; exit 1 ;; esac ;;",
            shown_record()
        ),
    )
    .destroy(&context(&dir), &compiled(GROUP), &KeptLog::default())
    .unwrap_err();
    assert!(err.msg.starts_with("az could not delete"), "{err:?}");
    assert_eq!(err.changes.len(), 1);
    assert_eq!(err.changes[0].resource, "web_db");
    assert!(!calls(&dir)
        .iter()
        .any(|c| c.starts_with("deployment group delete")));
}
