//! The running half, against a stand-in for `aws`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    artifact, cloud_credential, cloud_env, nothing_under_holds, scratch, secrets, stand_in, KeptLog,
};
use meridian_sdk::{Artifact, RunContext, Runner, SecretRef, Value};
use meridian_target_cloudformation::CloudFormationRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("template.yaml", "Resources: {}\n"),
        (
            "stack.json",
            r#"{"name":"web-vm","region":"us-east-1","capabilities":["CAPABILITY_IAM"],"resources":{"WebVm":"web_vm","WebIp":"web_ip"},"outputs":{"WebVmIp":"web_vm_ip"}}"#,
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
/// (`$2`, after `cloudformation`).
fn runner(dir: &Path, cases: &str) -> CloudFormationRunner {
    let program = stand_in(
        dir,
        "aws",
        &format!(
            "echo \"$@\" | sed 's/meridian-[0-9]*/meridian-N/' >> \"{dir}/calls\"\n{record}case \"$2\" in\n{cases}\nesac",
            dir = dir.display(),
            record = meridian_plugin_testkit::record_cloud_env(dir, "$2"),
        ),
    );
    CloudFormationRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: vec!["--profile".into(), "prod".into()],
    }
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(|l| {
            l.replace("cloudformation ", "")
                .replace(" --stack-name web-vm --region us-east-1 --profile prod", "")
        })
        .collect()
}

const CHANGES: &str = r#"describe-change-set) echo '{"Status":"CREATE_COMPLETE","Changes":[
  {"ResourceChange":{"Action":"Add","LogicalResourceId":"WebVm","ResourceType":"AWS::EC2::Instance"}},
  {"ResourceChange":{"Action":"Modify","LogicalResourceId":"WebIp","ResourceType":"AWS::EC2::EIP","Replacement":"True"}},
  {"ResourceChange":{"Action":"Remove","LogicalResourceId":"Gone","ResourceType":"AWS::S3::Bucket"}}]}' ;;"#;

const NEW_STACK: &str =
    "describe-stacks) echo 'Stack with id web-vm does not exist' >&2; exit 254 ;;";

const EXISTING: &str = r#"describe-stacks) echo '{"Stacks":[{"StackStatus":"UPDATE_COMPLETE","Outputs":[{"OutputKey":"WebVmIp","OutputValue":"203.0.113.7"}]}]}' ;;"#;

#[test]
fn a_plan_for_a_new_stack_previews_and_cleans_up_after_itself() {
    let dir = scratch(TMP, "plan-new");
    let log = KeptLog::default();
    let plan = runner(&dir, &format!("{NEW_STACK}\n{CHANGES}"))
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "describe-stacks --output json",
            "create-change-set --change-set-name meridian-N --change-set-type CREATE --template-body file://template.yaml --capabilities CAPABILITY_IAM",
            "wait change-set-create-complete --change-set-name meridian-N",
            "describe-change-set --change-set-name meridian-N --output json",
            "delete-change-set --change-set-name meridian-N",
            // Creating a change set for a stack that does not exist leaves an
            // empty one in review; a plan must not leave that behind.
            "delete-stack",
        ]
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            "create web_vm: AWS::EC2::Instance",
            "replace web_ip: AWS::EC2::EIP",
            "delete Gone: AWS::S3::Bucket",
        ]
    );
    assert_eq!(
        log.text(),
        ["stack web-vm: asking CloudFormation what would change"]
    );
}

#[test]
fn an_apply_executes_the_change_set_it_read_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, &format!("{EXISTING}\n{CHANGES}"))
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir)[1..],
        [
            "create-change-set --change-set-name meridian-N --change-set-type UPDATE --template-body file://template.yaml --capabilities CAPABILITY_IAM",
            "wait change-set-create-complete --change-set-name meridian-N",
            "describe-change-set --change-set-name meridian-N --output json",
            "execute-change-set --change-set-name meridian-N",
            "wait stack-update-complete",
            "describe-stacks --output json",
        ]
    );
    assert_eq!(applied.changes.len(), 3);
    assert_eq!(applied.outputs.len(), 1);
    // The output is reported under the name the author wrote.
    assert_eq!(applied.outputs[0].name, "web_vm_ip");
    assert_eq!(
        applied.outputs[0].value,
        Value::String("203.0.113.7".into())
    );
}

#[test]
fn a_change_set_with_nothing_in_it_is_in_sync_not_a_failure() {
    let dir = scratch(TMP, "in-sync");
    let cases = format!(
        "{EXISTING}\nwait) exit 255 ;;\ndescribe-change-set) echo '{{\"Status\":\"FAILED\",\"StatusReason\":\"The submitted information didn'\\''t contain changes. Submit different information to create a change set.\",\"Changes\":[]}}' ;;"
    );
    let plan = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync());
    let all = calls(&dir);
    assert_eq!(
        all.last().unwrap(),
        "delete-change-set --change-set-name meridian-N"
    );
    assert!(
        !all.contains(&"delete-stack".to_owned()),
        "an existing stack is never deleted"
    );

    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(applied.changes.is_empty());
    assert_eq!(applied.outputs.len(), 1);
    assert!(!calls(&dir)
        .iter()
        .any(|c| c.starts_with("execute-change-set")));
}

#[test]
fn a_rejected_change_set_reports_cloudformations_reason() {
    let dir = scratch(TMP, "rejected");
    let cases = format!(
        "{EXISTING}\ndescribe-change-set) echo '{{\"Status\":\"FAILED\",\"StatusReason\":\"Requires capabilities : [CAPABILITY_NAMED_IAM]\"}}' ;;"
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "CloudFormation rejected the change set: Requires capabilities : [CAPABILITY_NAMED_IAM]"
    );
}

#[test]
fn a_stack_that_fails_to_deploy_is_an_error_with_where_to_look() {
    let dir = scratch(TMP, "rolled-back");
    let cases = format!(
        "{EXISTING}\n{CHANGES}\nwait) if [ \"$3\" = stack-update-complete ]; then echo 'Waiter StackUpdateComplete failed' >&2; exit 255; fi ;;"
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg
            .starts_with("the stack did not reach a complete state"),
        "{}",
        err.msg
    );
    assert!(err.output.contains("Waiter StackUpdateComplete failed"));
}

#[test]
fn outputs_need_an_earlier_apply() {
    let dir = scratch(TMP, "outputs");
    let aws = runner(&dir, EXISTING);
    let err = aws.outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );
    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/stack.json"), &compiled().files["stack.json"]).unwrap();
    let outputs = aws.outputs(&context(&dir)).unwrap();
    assert_eq!(outputs[0].name, "web_vm_ip");
}

#[test]
fn a_refused_change_set_takes_the_stack_it_left_in_review_with_it() {
    let dir = scratch(TMP, "refused");
    // As LocalStack answers: the request is refused, and the stack exists
    // afterwards all the same, in review.
    let cases = format!(
        "describe-stacks) if [ -f \"{dir}/refused\" ]; then echo '{{\"Stacks\":[{{\"StackStatus\":\"REVIEW_IN_PROGRESS\"}}]}}'; else echo 'Stack with id web-vm does not exist' >&2; exit 254; fi ;;\n\
         create-change-set) touch \"{dir}/refused\"; echo 'An error occurred (ValidationError) when calling the CreateChangeSet operation: Template format error: Unresolved resource dependencies [Nowhere] in the Resources block of the template' >&2; exit 254 ;;",
        dir = dir.display()
    );
    let err = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "aws could not create a change set");
    assert!(err.output.contains("[Nowhere]"), "{}", err.output);
    let calls = calls(&dir);
    assert_eq!(
        calls[calls.len() - 2..],
        ["describe-stacks --output json", "delete-stack"]
    );
}

// ---- secrets ------------------------------------------------------------

/// A stand-in that also keeps a copy of the parameters file it is given,
/// its mode and the environment.
fn runner_keeping_parameters(dir: &Path, cases: &str) -> CloudFormationRunner {
    let program = stand_in(
        dir,
        "aws",
        &format!(
            "keep() {{\n  while [ $# -gt 0 ]; do\n    case \"$1\" in\n      --parameters)\n        f=\"${{2#file://}}\"\n        echo \"$f\" > \"{dir}/secrets-path\"\n        cp \"$f\" \"{dir}/secrets.json\"\n        stat -c %a \"$f\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$f\" > \"{dir}/mode\"\n        shift;;\n    esac\n    shift\n  done\n}}\nkeep \"$@\"\nenv > \"{dir}/env\"\necho \"$@\" | sed 's/meridian-[0-9]*/meridian-N/' >> \"{dir}/calls\"\ncase \"$2\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    CloudFormationRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: vec!["--profile".into(), "prod".into()],
    }
}

fn with_secret(dir: &Path) -> RunContext {
    let mut context = context(dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2\"\\")]);
    context
}

#[test]
fn secrets_reach_cloudformation_as_a_private_parameters_file_and_nowhere_else() {
    let dir = scratch(TMP, "secrets");
    let log = KeptLog::default();
    runner_keeping_parameters(
        &dir,
        &format!("{EXISTING}\n{CHANGES}\nexecute-change-set) ;;"),
    )
    .apply(&with_secret(&dir), &compiled(), &log)
    .unwrap();
    let calls = calls(&dir);
    let create = &calls[1];
    assert!(
        create.starts_with(
            "create-change-set --change-set-name meridian-N --change-set-type UPDATE \
             --template-body file://template.yaml --parameters file://"
        ) && create.ends_with(" --capabilities CAPABILITY_IAM"),
        "{create}"
    );
    // The change set carries the values; executing it names nothing more.
    assert_eq!(calls[4], "execute-change-set --change-set-name meridian-N");
    assert_eq!(
        fs::read_to_string(dir.join("secrets.json")).unwrap(),
        "[{\"ParameterKey\":\"MeridianSecretDbPassword\",\"ParameterValue\":\"hunter2\\\"\\\\\"}]\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    for call in &calls {
        assert!(!call.contains("hunter2"), "{call}");
    }
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("hunter2"));
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
    assert!(!fs::read_to_string(dir.join("work/template.yaml"))
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
    runner_keeping_parameters(&dir, &format!("{NEW_STACK}\n{CHANGES}"))
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(
        !calls(&dir)[1].contains("--parameters"),
        "{:?}",
        calls(&dir)
    );
    assert!(!dir.join("secrets.json").exists());
}

#[test]
fn an_aws_credential_reaches_every_call_in_its_environment_and_nowhere_else() {
    let dir = scratch(TMP, "aws-credential");
    let mut context = context(&dir);
    context.credential = Some(cloud_credential(
        "aws",
        &[("accessKeyId", "AKIA1")],
        &[
            ("secretAccessKey", "s3cr3t-9fXw"),
            ("sessionToken", "t0k3n-9fXw"),
        ],
    ));
    let log = KeptLog::default();
    runner(&dir, &format!("{NEW_STACK}\n{CHANGES}"))
        .plan(&context, &compiled(), &log)
        .unwrap();
    for call in [
        "describe-stacks",
        "create-change-set",
        "wait",
        "describe-change-set",
        "delete-change-set",
        "delete-stack",
    ] {
        assert_eq!(
            cloud_env(&dir, call),
            [
                "AWS_ACCESS_KEY_ID=AKIA1",
                "AWS_SECRET_ACCESS_KEY=s3cr3t-9fXw",
                "AWS_SESSION_TOKEN=t0k3n-9fXw",
            ],
            "{call}"
        );
        fs::remove_file(dir.join(format!("cloud-env.{call}"))).unwrap();
    }
    assert!(!calls(&dir).iter().any(|c| c.contains("9fXw")));
    assert!(!log.text().iter().any(|l| l.contains("9fXw")));
    nothing_under_holds(&dir.join("work"), "9fXw");
}

#[test]
fn a_credential_for_another_cloud_stops_the_run_before_the_tool_starts() {
    let dir = scratch(TMP, "gcp-credential");
    let mut context = context(&dir);
    context.credential = Some(cloud_credential(
        "gcp",
        &[("project", "shop")],
        &[("serviceAccountKey", "{}")],
    ));
    let err = runner(&dir, &format!("{NEW_STACK}\n{CHANGES}"))
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the credential \"prod\": aws cloudformation reaches AWS only, as an aws credential, \
         not gcp"
    );
    assert!(!dir.join("calls").exists(), "the tool was started");
}
