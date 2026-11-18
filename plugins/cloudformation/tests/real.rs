//! The plugin against a real `aws` command line and a CloudFormation service
//! that creates what it is asked to.
//!
//! The service is whatever the `aws` command reaches. In CI that is
//! LocalStack, an emulator of AWS running in a container on the runner, so
//! no account and no credentials are needed; `MERIDIAN_AWS_ARGS` points the
//! command at it:
//!
//! ```sh
//! docker run -d -p 4566:4566 localstack/localstack:4.14.0
//! AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test AWS_DEFAULT_REGION=us-east-1 \
//! MERIDIAN_AWS_ARGS='--endpoint-url http://localhost:4566' \
//! MERIDIAN_TEST_REAL_CLOUDFORMATION=1 \
//!     cargo test -p meridian-target-cloudformation --test real
//! ```
//!
//! Without `MERIDIAN_TEST_REAL_CLOUDFORMATION=1` the tests pass without
//! doing anything. With it, a service that cannot be reached is a failure.
//! Pointed at a real account they create a queue, a topic and a parameter in
//! a stack named `meridian-real`, and delete the stack when they end.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    graph_from_text, repo_root, scratch, secret_in_param, secrets, KeptLog,
};
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, NoLog, Output, Plan, RunContext,
    Runner, SecretRef,
};
use meridian_target_cloudformation::{CloudFormation, CloudFormationRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const DOCUMENT: &str = "plugins/cloudformation/tests/documents/messaging.yaml";
const STACK: &str = "meridian-real";

/// One `aws` call of the test's own, with the runner's program and extra
/// arguments, returning what it printed.
fn aws(args: &[&str]) -> Result<String, String> {
    let runner = CloudFormationRunner::from_env();
    let finished = Tool::new(&runner.program)
        .args(args.iter().copied())
        .args(["--region", "us-east-1"])
        .args(runner.extra_args.iter().cloned())
        .env("AWS_PAGER", "")
        .capture_stdout()
        .run(&NoLog)
        .map_err(|e| e.to_string())?;
    if finished.success() {
        Ok(finished.stdout)
    } else {
        Err(format!("{}{}", finished.stdout, finished.stderr))
    }
}

/// Whether to run. Asked for and no service is a failure, not a skip.
fn service() -> bool {
    if std::env::var("MERIDIAN_TEST_REAL_CLOUDFORMATION")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!(
            "skipped: set MERIDIAN_TEST_REAL_CLOUDFORMATION=1 to run against a real CloudFormation"
        );
        return false;
    }
    if let Err(why) = aws(&["cloudformation", "list-stacks", "--output", "json"]) {
        panic!("MERIDIAN_TEST_REAL_CLOUDFORMATION=1, and CloudFormation cannot be reached: {why}");
    }
    true
}

fn stack_exists(name: &str) -> bool {
    aws(&["cloudformation", "describe-stacks", "--stack-name", name]).is_ok()
}

fn delete_stack(name: &str) {
    let _ = aws(&["cloudformation", "delete-stack", "--stack-name", name]);
    let _ = aws(&[
        "cloudformation",
        "wait",
        "stack-delete-complete",
        "--stack-name",
        name,
    ]);
}

/// Deletes a stack when the test ends, however it ends.
struct TearDown(&'static str);

impl Drop for TearDown {
    fn drop(&mut self) {
        delete_stack(self.0);
    }
}

fn compile(text: &str) -> Artifact {
    CloudFormation
        .emit(
            &graph_from_text(DOCUMENT, text, "cloudformation"),
            &Data::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "messaging".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn plan(dir: &Path, artifact: &Artifact) -> Plan {
    let log = KeptLog::default();
    CloudFormationRunner::from_env()
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn apply(dir: &Path, artifact: &Artifact) -> Applied {
    let log = KeptLog::default();
    CloudFormationRunner::from_env()
        .apply(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn changes(list: &[Change]) -> Vec<(ChangeAction, &str)> {
    let mut seen: Vec<_> = list
        .iter()
        .map(|c| (c.action, c.resource.as_str()))
        .collect();
    seen.sort_by_key(|(_, resource)| *resource);
    seen
}

fn output<'a>(outputs: &'a [Output], name: &str) -> &'a str {
    outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name:?} in {outputs:?}"))
        .value
        .as_str()
        .expect("a stack's output is text")
}

/// The value of the parameter the stack records the queue's name in.
fn recorded() -> Result<String, String> {
    aws(&[
        "ssm",
        "get-parameter",
        "--name",
        "/meridian-real/jobs-queue",
        "--query",
        "Parameter.Value",
        "--output",
        "text",
    ])
    .map(|value| value.trim().to_owned())
}

#[ignore = "needs a real aws and CloudFormation; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_stack_is_previewed_created_read_back_changed_and_trimmed() {
    if !service() {
        return;
    }
    delete_stack(STACK);
    let _tear_down = TearDown(STACK);
    let dir = scratch(TMP, "real-messaging");
    let text = fs::read_to_string(repo_root().join(DOCUMENT)).unwrap();
    let first = compile(&text);

    // A plan is a change set that is read and thrown away. For a stack that
    // does not exist, the empty stack the change set made goes with it.
    let preview = plan(&dir, &first);
    assert!(!preview.unknown);
    assert_eq!(
        changes(&preview.changes),
        [
            (ChangeAction::Create, "alerts"),
            (ChangeAction::Create, "jobs"),
            (ChangeAction::Create, "jobs_queue_name"),
        ]
    );
    assert!(!stack_exists(STACK), "a plan leaves no stack behind");
    assert!(recorded().is_err(), "a plan creates nothing");

    // An apply executes the same kind of change set and reads the outputs,
    // under the names the document gave them.
    let applied = apply(&dir, &first);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert_eq!(output(&applied.outputs, "queue_name"), "meridian-real-jobs");
    assert!(
        output(&applied.outputs, "queue_arn").ends_with(":meridian-real-jobs"),
        "{:?}",
        applied.outputs
    );
    assert_eq!(
        output(&applied.outputs, "recorded_in"),
        "/meridian-real/jobs-queue"
    );
    assert_eq!(
        output(&applied.outputs, "topic"),
        "topic meridian-real-alerts in us-east-1"
    );
    // The reference between two resources was resolved by the service.
    assert_eq!(recorded().as_deref(), Ok("meridian-real-jobs"));

    // Outputs can be read again later, without a change set.
    let read = CloudFormationRunner::from_env()
        .outputs(&context(&dir))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(output(&read, "queue_name"), "meridian-real-jobs");

    // Nothing has changed. CloudFormation says so by failing the change
    // set, which is not a failure of the plan.
    let again = plan(&dir, &first);
    assert!(again.changes.is_empty(), "{:?}", again.changes);
    let applied = apply(&dir, &first);
    assert!(applied.changes.is_empty(), "{:?}", applied.changes);
    assert_eq!(output(&applied.outputs, "queue_name"), "meridian-real-jobs");

    // One property of one resource changes: an update of that resource.
    let second = compile(&text.replace("VisibilityTimeout: 30", "VisibilityTimeout: 45"));
    let preview = plan(&dir, &second);
    assert_eq!(changes(&preview.changes), [(ChangeAction::Update, "jobs")]);
    let applied = apply(&dir, &second);
    assert_eq!(changes(&applied.changes), [(ChangeAction::Update, "jobs")]);

    // A resource removed from the document is deleted from the stack.
    let trimmed = second_without_the_topic(&text);
    let third = compile(&trimmed);
    let preview = plan(&dir, &third);
    assert_eq!(
        changes(&preview.changes),
        [(ChangeAction::Delete, "Alerts")],
        "a resource the document no longer has is named as CloudFormation names it"
    );
    let applied = apply(&dir, &third);
    assert!(
        applied.outputs.iter().all(|o| o.name != "topic"),
        "{:?}",
        applied.outputs
    );
}

/// The document with its topic, and the output that reads it, removed.
fn second_without_the_topic(text: &str) -> String {
    let text = text.replace("VisibilityTimeout: 30", "VisibilityTimeout: 45");
    let start = text.find("    - id: alerts").expect("the topic is there");
    let end = text
        .find("    - id: jobs_queue_name")
        .expect("the parameter follows the topic");
    let mut trimmed = format!("{}{}", &text[..start], &text[end..]);
    let output = trimmed
        .find("    - name: topic")
        .expect("the topic's output is last");
    trimmed.truncate(output);
    trimmed
}

#[ignore = "needs a real aws and CloudFormation; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_template_cloudformation_rejects_fails_the_plan_and_leaves_no_stack() {
    if !service() {
        return;
    }
    const REJECTED: &str = "meridian-real-rejected";
    delete_stack(REJECTED);
    let _tear_down = TearDown(REJECTED);
    let dir = scratch(TMP, "real-rejected");
    // The param is CloudFormation's own function, written out, so the
    // compile hands it over as it is. Only CloudFormation knows the template
    // has no resource called Nowhere.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: messaging\nspec:\n  target: cloudformation\n  resources:\n    - id: deployment\n      type: stack\n      params:\n        name: meridian-real-rejected\n        region: us-east-1\n    - id: thing\n      type: AWS::SSM::Parameter\n      params:\n        Name: /meridian/real/rejected\n        Type: String\n        Value:\n          Fn::GetAtt: [Nowhere, Arn]\n";
    let artifact = compile(text);
    let error = CloudFormationRunner::from_env()
        .plan(&context(&dir), &artifact, &NoLog)
        .expect_err("CloudFormation refuses the template");
    assert!(
        format!("{} {}", error.msg, error.output).contains("Nowhere"),
        "{error:?}"
    );
    assert!(!stack_exists(REJECTED), "a refused plan leaves no stack");
}

/// Unlike anything the stack or the service prints, so that finding it is
/// finding a leak.
const SECRET: &str = "zzq-Vt9-not-a-real-token-4kLm";
const SECRET_STACK: &str = "meridian-real-secret";

#[ignore = "needs a real aws and CloudFormation; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_the_stack_as_a_noecho_parameter_and_is_in_nothing_shown() {
    if !service() {
        return;
    }
    delete_stack(SECRET_STACK);
    let _tear_down = TearDown(SECRET_STACK);
    let dir = scratch(TMP, "real-secret");
    // A parameter in Systems Manager whose value is the secret: the way a
    // value reaches what reads it from there.
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: secret\nspec:\n  target: cloudformation\n  resources:\n    - id: deployment\n      type: stack\n      params:\n        name: {SECRET_STACK}\n        region: us-east-1\n    - id: token\n      type: AWS::SSM::Parameter\n      params:\n        Name: /{SECRET_STACK}/token\n        Type: String\n        Value: placeholder\n"
    );
    let mut graph = graph_from_text(DOCUMENT, &text, "cloudformation");
    secret_in_param(&mut graph, "token", "Value", "token");
    let artifact = CloudFormation
        .emit(&graph, &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    assert!(!artifact.files["template.yaml"].contains(SECRET));

    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "token", SECRET)]);
    let runner = CloudFormationRunner::from_env();
    let log = KeptLog::default();
    let preview = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(changes(&preview.changes), [(ChangeAction::Create, "token")]);
    let applied = runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(changes(&applied.changes), [(ChangeAction::Create, "token")]);
    // The service was given the value: the parameter holds it.
    let stored = aws(&[
        "ssm",
        "get-parameter",
        "--name",
        &format!("/{SECRET_STACK}/token"),
        "--query",
        "Parameter.Value",
        "--output",
        "text",
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stored.trim(), SECRET);
    // The stack's own description does not show it: NoEcho.
    let described = aws(&[
        "cloudformation",
        "describe-stacks",
        "--stack-name",
        SECRET_STACK,
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(described.contains("MeridianSecretToken"), "{described}");
    assert!(
        !described.contains(SECRET),
        "describe-stacks shows the secret"
    );
    // Nothing the plugin printed or wrote holds it.
    assert!(
        !log.text().iter().any(|l| l.contains(SECRET)),
        "the secret reached the log:\n{}",
        log.text().join("\n")
    );
    assert!(!fs::read_to_string(dir.join("work/template.yaml"))
        .unwrap()
        .contains(SECRET));
}
