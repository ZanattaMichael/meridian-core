//! The running half, against a stand-in for `aws`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, Value};
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
            "echo \"$@\" | sed 's/meridian-[0-9]*/meridian-N/' >> \"{dir}/calls\"\ncase \"$2\" in\n{cases}\nesac",
            dir = dir.display()
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
