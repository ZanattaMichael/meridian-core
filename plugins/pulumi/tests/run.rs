//! The running half, against a stand-in for `pulumi`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, Value};
use meridian_target_pulumi::PulumiRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled_with(stack: &str) -> Artifact {
    artifact(&[
        ("Pulumi.yaml", "name: web-vm\nruntime: yaml\n"),
        ("stack.json", stack),
    ])
}

fn compiled() -> Artifact {
    compiled_with(r#"{"project":"web-vm","stack":"prod","backend":"","secretsProvider":""}"#)
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-vm".into(),
        work_dir: dir.join("work"),
        inputs: [
            ("region".to_owned(), Value::String("us-east-1".into())),
            ("replicas".to_owned(), Value::Int(3)),
        ]
        .into(),
        ..RunContext::default()
    }
}

/// A stand-in that appends each invocation to `calls`, records the
/// environment Pulumi would see, and answers by subcommand.
fn stand_in_for_pulumi(dir: &Path, cases: &str) -> String {
    let program = stand_in(
        dir,
        "pulumi",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\n\
             echo \"backend=${{PULUMI_BACKEND_URL-unset}} passphrase=${{PULUMI_CONFIG_PASSPHRASE-unset}} check=$PULUMI_SKIP_UPDATE_CHECK\" > \"{dir}/env\"\n\
             pwd > \"{dir}/cwd\"\n\
             case \"$1 $2\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    program.to_str().unwrap().to_owned()
}

fn runner(dir: &Path, cases: &str) -> PulumiRunner {
    PulumiRunner {
        program: stand_in_for_pulumi(dir, cases),
        backend_chosen: false,
        passphrase_chosen: false,
    }
}

const HAPPY: &str = r#""stack select") echo "Created stack 'prod'" ;;
"preview "*) echo '{"steps":[
  {"op":"create","urn":"urn:pulumi:prod::web-vm::pulumi:pulumi:Stack::web-vm-prod","newState":{"type":"pulumi:pulumi:Stack"}},
  {"op":"create","urn":"urn:pulumi:prod::web-vm::aws:ec2/instance:Instance::web_vm","newState":{"type":"aws:ec2/instance:Instance"}}],
  "diagnostics":[{"message":"the ami is deprecated","severity":"warning","urn":"urn:pulumi:prod::web-vm::aws:ec2/instance:Instance::web_vm"}],
  "changeSummary":{"create":2}}' ;;
"up "*) echo 'Updating (prod):'; echo '    + 2 created' ;;
"stack output") case "$*" in
  *--show-secrets*) echo '{"web_vm_ip":"203.0.113.7","admin_password":"hunter2"}' ;;
  *) echo '{"web_vm_ip":"203.0.113.7","admin_password":"[secret]"}' ;;
  esac ;;"#;

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(|l| l.replace(" --non-interactive --color never", ""))
        .collect()
}

fn seen_env(dir: &Path) -> String {
    fs::read_to_string(dir.join("env"))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn a_plan_selects_the_stack_and_reads_the_preview() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "stack select prod --create",
            "preview --stack prod --json --config region=us-east-1 --config replicas=3",
        ]
    );
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(
        plan.changes[0].to_string(),
        "create web_vm: aws:ec2/instance:Instance"
    );
    assert!(!plan.unknown);
    // The JSON is data: only what is said to a person is relayed.
    assert_eq!(
        log.text(),
        [
            "Created stack 'prod'",
            "stack prod: asking Pulumi what would change",
            "warning: web_vm: the ami is deprecated",
        ]
    );
    assert!(dir.join("work/Pulumi.yaml").exists());
    assert!(dir.join("work/stack.json").exists());
    let work = meridian_sdk::tool::absolute_path(&dir.join("work"));
    assert_eq!(
        fs::read_to_string(dir.join("cwd")).unwrap().trim(),
        work.to_str().unwrap()
    );
}

#[test]
fn state_stays_with_the_project_unless_someone_says_otherwise() {
    // Nobody chose: state is kept in the work directory, behind an empty
    // passphrase, so no account and no prompt stand in the way.
    let dir = scratch(TMP, "backend-default");
    runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let work = meridian_sdk::tool::absolute_path(&dir.join("work"));
    assert_eq!(
        seen_env(&dir),
        // A URL is written with forward slashes on every system.
        format!(
            "backend=file://{} passphrase= check=true",
            work.display().to_string().replace('\\', "/")
        )
    );

    // The document chose a backend: the project file carries it, and the
    // runner does not override it.
    let dir = scratch(TMP, "backend-document");
    runner(&dir, HAPPY)
        .plan(
            &context(&dir),
            &compiled_with(r#"{"stack":"prod","backend":"s3://acme-state"}"#),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(seen_env(&dir), "backend=unset passphrase= check=true");

    // The operator chose, through Pulumi's own variables: both are left be.
    let dir = scratch(TMP, "backend-operator");
    PulumiRunner {
        program: stand_in_for_pulumi(&dir, HAPPY),
        backend_chosen: true,
        passphrase_chosen: true,
    }
    .plan(&context(&dir), &compiled(), &KeptLog::default())
    .unwrap();
    assert_eq!(seen_env(&dir), "backend=unset passphrase=unset check=true");
}

#[test]
fn a_new_stack_is_created_with_the_documents_secrets_provider() {
    let dir = scratch(TMP, "secrets-provider");
    runner(&dir, HAPPY)
        .plan(
            &context(&dir),
            &compiled_with(r#"{"stack":"acme/prod","secretsProvider":"awskms://alias/pulumi"}"#),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        calls(&dir)[0],
        "stack select acme/prod --create --secrets-provider awskms://alias/pulumi"
    );
}

#[test]
fn an_apply_previews_updates_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
    let log = KeptLog::default();
    let applied = runner(&dir, HAPPY)
        .apply(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir)[2..],
        [
            "up --stack prod --yes --skip-preview --config region=us-east-1 --config replicas=3",
            "stack output --stack prod --json",
            "stack output --stack prod --json --show-secrets",
        ]
    );
    assert_eq!(applied.changes.len(), 1);
    assert!(log.text().contains(&"    + 2 created".to_owned()));
    // An output's value never reaches the log, secret or not.
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));

    let ip = applied
        .outputs
        .iter()
        .find(|o| o.name == "web_vm_ip")
        .unwrap();
    assert_eq!(ip.value, Value::String("203.0.113.7".into()));
    assert!(!ip.sensitive);
    let masked = applied
        .outputs
        .iter()
        .find(|o| o.name == "admin_password")
        .unwrap();
    assert!(masked.value == Value::String("hunter2".into()));
    assert!(masked.sensitive);
}

#[test]
fn outputs_are_read_without_previewing_anything() {
    let dir = scratch(TMP, "outputs");
    let err = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );

    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/stack.json"), r#"{"stack":"prod"}"#).unwrap();
    let outputs = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap();
    assert_eq!(
        calls(&dir),
        [
            "stack output --stack prod --json",
            "stack output --stack prod --json --show-secrets"
        ]
    );
    assert_eq!(outputs.len(), 2);
}

#[test]
fn a_failed_preview_says_what_pulumi_said_and_stops_there() {
    let dir = scratch(TMP, "failed-preview");
    let cases = r#""stack select") echo ok ;;
"preview "*) echo '{"steps":[],"diagnostics":[{"message":"no valid credential sources found","severity":"error"}]}'; exit 255 ;;"#;
    let log = KeptLog::default();
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &log)
        .unwrap_err();
    assert_eq!(err.msg, "pulumi preview failed");
    assert_eq!(err.exit_code, Some(255));
    assert_eq!(err.output, "error: no valid credential sources found\n");
    assert!(log
        .text()
        .contains(&"error: no valid credential sources found".to_owned()));
    // A value handed over from an earlier stage is never shown.
    assert!(
        err.command
            .ends_with("preview --stack prod --non-interactive --color never --json --config '***' --config '***'"),
        "{}",
        err.command
    );
    assert_eq!(calls(&dir).len(), 2, "nothing runs after the failure");
}

#[test]
fn a_preview_that_fails_without_json_keeps_what_was_printed() {
    let dir = scratch(TMP, "failed-plain");
    let cases = r#""stack select") echo ok ;;
"preview "*) echo 'panic: something broke'; echo 'error: could not load plugin' >&2; exit 1 ;;"#;
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "pulumi preview failed");
    assert_eq!(
        err.output,
        "error: could not load plugin\npanic: something broke\n"
    );
}

#[test]
fn a_preview_that_fails_with_json_but_no_diagnostics_keeps_the_json() {
    let dir = scratch(TMP, "failed-silent");
    let cases = "\"stack select\") echo ok ;;\n\"preview \"*) echo '{\"steps\":[],\"error\":\"no such type\"}'; exit 1 ;;";
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "pulumi preview failed");
    assert_eq!(err.output, "{\"steps\":[],\"error\":\"no such type\"}\n");
}

#[test]
fn a_preview_that_succeeds_with_nonsense_is_an_error() {
    let dir = scratch(TMP, "nonsense");
    let cases = "\"stack select\") echo ok ;;\n\"preview \"*) echo 'Previewing update' ;;";
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with("pulumi's preview could not be read"),
        "{}",
        err.msg
    );
}

#[test]
fn a_failed_update_says_which_step_failed() {
    let dir = scratch(TMP, "failed-up");
    let cases = r#""stack select") echo ok ;;
"preview "*) echo '{"steps":[]}' ;;
"up "*) echo 'error: update failed' >&2; exit 255 ;;"#;
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "pulumi up failed");
    assert!(err.output.contains("error: update failed"));
    assert_eq!(calls(&dir).len(), 3, "outputs are not read after a failure");
}

#[test]
fn a_stack_that_cannot_be_selected_stops_everything() {
    let dir = scratch(TMP, "failed-select");
    let cases = "\"stack select\") echo 'error: PULUMI_ACCESS_TOKEN must be set' >&2; exit 255 ;;";
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "pulumi could not select or create the stack");
    assert_eq!(calls(&dir).len(), 1);
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, HAPPY)
        .plan(
            &context(&dir),
            &artifact(&[("main.tf.json", "{}")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no Pulumi.yaml and stack.json; it was not compiled by this plugin"
    );
    assert!(!dir.join("calls").exists());

    let err = runner(&dir, HAPPY)
        .plan(
            &context(&dir),
            &compiled_with("not json"),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "stack.json could not be read; it was not compiled by this plugin"
    );
}

#[test]
fn pulumi_not_being_installed_is_said_plainly() {
    let dir = scratch(TMP, "missing");
    let err = PulumiRunner {
        program: "meridian-no-such-pulumi".into(),
        backend_chosen: false,
        passphrase_chosen: false,
    }
    .plan(&context(&dir), &compiled(), &KeptLog::default())
    .unwrap_err();
    assert!(
        err.msg
            .starts_with("could not run meridian-no-such-pulumi: it is not installed"),
        "{}",
        err.msg
    );
}
