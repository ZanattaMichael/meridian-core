//! The running half, against a stand-in for `tofu`.
//!
//! These prove what the runner asks of the tool and what it makes of the
//! answers, on a machine with nothing installed. `tests/real.rs` runs the
//! same operations against a real `tofu`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use meridian_plugin_testkit::{
    artifact, cloud_credential, cloud_env, nothing_under_holds, scratch, secrets, stand_in, KeptLog,
};
use meridian_sdk::{Artifact, ChangeAction, RunContext, Runner, SecretRef, Value};
use meridian_target_opentofu::OpenTofuRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// A scratch directory, and a guard that keeps the other tests of this file
/// from running until it is dropped.
///
/// A stand-in is a script written and then executed. If another thread starts
/// a process while the script is still open for writing, the new process
/// briefly holds that handle too, and executing the script in that instant
/// fails with "text file busy". One test at a time leaves no other thread to
/// start one.
fn scratch_alone(name: &str) -> (PathBuf, MutexGuard<'static, ()>) {
    static ALONE: Mutex<()> = Mutex::new(());
    let guard = ALONE.lock().unwrap_or_else(PoisonError::into_inner);
    (scratch(TMP, name), guard)
}

fn compiled() -> Artifact {
    artifact(&[("main.tofu.json", "{}\n")])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-vm".into(),
        work_dir: dir.join("work"),
        inputs: [
            ("region".to_owned(), Value::String("us-east-1".into())),
            ("replicas".to_owned(), Value::Int(3)),
            (
                "zones".to_owned(),
                Value::List(vec![Value::String("a".into()), Value::String("b".into())]),
            ),
        ]
        .into(),
        ..RunContext::default()
    }
}

/// A stand-in that appends each invocation to `calls`, records the
/// environment it was given, and answers by subcommand.
fn runner(dir: &Path, cases: &str) -> OpenTofuRunner {
    let program = stand_in(
        dir,
        "tofu",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\n\
             echo \"$TF_VAR_region|$TF_VAR_replicas|$TF_VAR_zones|$TF_IN_AUTOMATION|$TF_INPUT\" > \"{dir}/env\"\n\
             pwd > \"{dir}/pwd\"\n{record}\
             case \"$1\" in\n{cases}\nesac",
            dir = dir.display(),
            record = meridian_plugin_testkit::record_cloud_env(dir, "$1"),
        ),
    );
    OpenTofuRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

const HAPPY: &str = r#"init) echo 'OpenTofu has been successfully initialized!' ;;
plan) echo '{"@level":"info","@message":"Plan: 1 to add, 0 to change, 0 to destroy.","type":"change_summary"}' ;;
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
    let (dir, _serial) = scratch_alone("plan");
    let log = KeptLog::default();
    let plan = runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "init -input=false -no-color",
            "plan -input=false -no-color -json -out=meridian.tfplan",
            "show -json meridian.tfplan",
        ]
    );
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].to_string(), "create web_vm: aws_instance");
    assert!(!plan.unknown);
    // The JSON is data: only what OpenTofu says to a person is relayed.
    assert_eq!(
        log.text(),
        [
            "OpenTofu has been successfully initialized!",
            "Plan: 1 to add, 0 to change, 0 to destroy."
        ]
    );
    assert!(dir.join("work/main.tofu.json").exists());
}

#[test]
fn the_tool_runs_in_the_work_directory_without_prompting() {
    let (dir, _serial) = scratch_alone("environment");
    runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let pwd = fs::read_to_string(dir.join("pwd")).unwrap();
    assert_eq!(
        Path::new(pwd.trim()).canonicalize().unwrap(),
        dir.join("work").canonicalize().unwrap()
    );
    // Inputs from earlier stages arrive as input variables: a string as it
    // is, anything else in the JSON form OpenTofu parses for a typed variable.
    assert_eq!(
        fs::read_to_string(dir.join("env")).unwrap().trim(),
        "us-east-1|3|[\"a\",\"b\"]|1|0"
    );
}

#[test]
fn an_apply_applies_the_plan_it_just_showed_and_reports_outputs() {
    let (dir, _serial) = scratch_alone("apply");
    let applied = runner(&dir, HAPPY)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir)[3..],
        [
            "apply -input=false -no-color -json meridian.tfplan",
            "output -json -no-color"
        ]
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(applied.changes[0].action, ChangeAction::Create);
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
fn a_plan_with_nothing_to_do_reports_no_changes() {
    let (dir, _serial) = scratch_alone("in-sync");
    let cases = r#"init) echo ok ;;
plan) echo 'No changes. Your infrastructure matches the configuration.' ;;
show) echo '{"resource_changes":[{"address":"aws_instance.web_vm","mode":"managed","type":"aws_instance","name":"web_vm","change":{"actions":["no-op"]}}]}' ;;"#;
    let plan = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.changes.is_empty());
    assert!(!plan.unknown);
}

#[test]
fn every_kind_of_change_is_told_apart() {
    let (dir, _serial) = scratch_alone("changes");
    let cases = r#"init) echo ok ;;
plan) echo ok ;;
show) echo '{"resource_changes":[
 {"address":"aws_instance.a","mode":"managed","type":"aws_instance","name":"a","change":{"actions":["update"]}},
 {"address":"aws_instance.b","mode":"managed","type":"aws_instance","name":"b","change":{"actions":["delete","create"]}},
 {"address":"aws_instance.c","mode":"managed","type":"aws_instance","name":"c","change":{"actions":["create","delete"]}},
 {"address":"aws_instance.d","mode":"managed","type":"aws_instance","name":"d","change":{"actions":["delete"]}},
 {"address":"aws_instance.e","mode":"managed","type":"aws_instance","name":"e","change":{"actions":["forget"]}},
 {"address":"module.net.aws_vpc.this","module_address":"module.net","mode":"managed","type":"aws_vpc","name":"this","change":{"actions":["create"]}}
]}' ;;"#;
    let plan = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let seen: Vec<(ChangeAction, &str)> = plan
        .changes
        .iter()
        .map(|c| (c.action, c.resource.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            (ChangeAction::Update, "a"),
            (ChangeAction::Replace, "b"),
            (ChangeAction::Replace, "c"),
            (ChangeAction::Delete, "d"),
            // A resource inside a module keeps OpenTofu's full address.
            (ChangeAction::Create, "module.net.aws_vpc.this"),
        ]
    );
}

#[test]
fn outputs_are_read_without_planning_anything() {
    let (dir, _serial) = scratch_alone("outputs");
    let err = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );

    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/main.tofu.json"), "{}").unwrap();
    let outputs = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap();
    assert_eq!(calls(&dir), ["output -json -no-color"]);
    assert_eq!(outputs.len(), 2);
}

#[test]
fn a_failed_step_says_which_and_stops_there() {
    for (step, cases, ran) in [
        (
            "tofu init failed",
            "init) echo 'Error: Failed to query available provider packages' >&2; exit 1 ;;",
            1,
        ),
        (
            "tofu plan failed",
            "init) echo ok ;;\nplan) echo 'Error: No valid credential sources found' >&2; exit 1 ;;",
            2,
        ),
        (
            "tofu could not show the plan it saved",
            "init) echo ok ;;\nplan) echo ok ;;\nshow) echo 'Error: decryption failed' >&2; exit 1 ;;",
            3,
        ),
        (
            "tofu apply failed",
            "init) echo ok ;;\nplan) echo ok ;;\nshow) echo '{}' ;;\napply) echo 'Error: quota exceeded' >&2; exit 1 ;;",
            4,
        ),
        (
            "tofu could not report its outputs",
            "init) echo ok ;;\nplan) echo ok ;;\nshow) echo '{}' ;;\napply) echo ok ;;\noutput) echo 'Error: state is locked' >&2; exit 1 ;;",
            5,
        ),
    ] {
        let (dir, _serial) = scratch_alone(&format!("failed-{ran}"));
        let err = runner(&dir, cases)
            .apply(&context(&dir), &compiled(), &KeptLog::default())
            .unwrap_err();
        assert_eq!(err.msg, step);
        assert_eq!(err.exit_code, Some(1), "{step}");
        assert!(err.output.contains("Error: "), "{step}: {}", err.output);
        assert_eq!(calls(&dir).len(), ran, "{step}: nothing runs after the failure");
    }
}

#[test]
fn a_failure_names_the_command_that_failed() {
    let (dir, _serial) = scratch_alone("failed-command");
    let cases = "init) echo ok ;;\nplan) echo 'Error: nope' >&2; exit 3 ;;";
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.exit_code, Some(3));
    assert!(err
        .command
        .ends_with("plan -input=false -no-color -json -out=meridian.tfplan"));
}

#[test]
fn a_plan_that_is_not_json_is_an_error_and_not_an_empty_plan() {
    let (dir, _serial) = scratch_alone("garbled");
    let cases = "init) echo ok ;;\nplan) echo ok ;;\nshow) echo 'Warning: something' ;;";
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with("tofu's plan could not be read"),
        "{}",
        err.msg
    );
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let (dir, _serial) = scratch_alone("foreign");
    for foreign in ["playbook.yml", "main.tf.json"] {
        let err = runner(&dir, HAPPY)
            .plan(
                &context(&dir),
                &artifact(&[(foreign, "")]),
                &KeptLog::default(),
            )
            .unwrap_err();
        assert_eq!(
            err.msg,
            "the artifact has no main.tofu.json; it was not compiled by this plugin"
        );
    }
    assert!(!dir.join("calls").exists(), "the tool was never run");
}

#[test]
fn a_missing_program_says_what_was_looked_for() {
    let (dir, _serial) = scratch_alone("missing");
    let runner = OpenTofuRunner {
        program: dir.join("no-such-tofu").to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    };
    let err = runner
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(err.command.contains("no-such-tofu"), "{err:?}");
    assert_eq!(err.exit_code, None);
}

/// Answers like `HAPPY`, keeps a copy of the variables file a plan is
/// pointed at, and saves a plan file as the real tool would.
fn keeping_secrets(dir: &Path) -> String {
    format!(
        "init) echo initialised ;;\n\
         plan) for a in \"$@\"; do case \"$a\" in -var-file=*) cp \"${{a#-var-file=}}\" \"{dir}/vars\";; esac; done; echo PLANDATA > meridian.tfplan; echo 'Plan: 1 to add, 0 to change, 0 to destroy.' ;;\n\
         show) echo '{{\"resource_changes\":[]}}' ;;\n\
         apply) echo applied ;;\n\
         output) echo '{{}}' ;;",
        dir = dir.display()
    )
}

#[test]
fn secrets_reach_the_tool_in_a_private_variables_file_and_the_saved_plan_is_scrubbed() {
    let (dir, _alone) = scratch_alone("secrets");
    let runner = runner(&dir, &keeping_secrets(&dir));
    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2")]);
    let log = KeptLog::default();
    runner.plan(&context, &compiled(), &log).unwrap();
    // The tool read the value from the file, under the variable's name,
    // and was told the file's path and nothing more.
    assert_eq!(
        fs::read_to_string(dir.join("vars")).unwrap(),
        "{\"meridian_secret_db_password\":\"hunter2\"}\n"
    );
    let made = calls(&dir);
    assert!(
        made[1].starts_with("plan -input=false -no-color -json -out=meridian.tfplan -var-file="),
        "{made:?}"
    );
    assert!(!made.iter().any(|c| c.contains("hunter2")), "{made:?}");
    let path = made[1]
        .split_whitespace()
        .find_map(|a| a.strip_prefix("-var-file="))
        .unwrap();
    assert!(!Path::new(path).exists(), "{path} was left behind");
    // The saved plan holds every variable's value, so it does not outlive
    // its use.
    assert!(!dir.join("work").join("meridian.tfplan").exists());
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));

    runner.apply(&context, &compiled(), &log).unwrap();
    assert!(calls(&dir).iter().any(|c| c.starts_with("apply ")));
    assert!(!dir.join("work").join("meridian.tfplan").exists());
}

#[test]
fn a_run_without_secrets_keeps_its_saved_plan_and_passes_no_variables_file() {
    let (dir, _alone) = scratch_alone("no-secrets");
    let runner = runner(&dir, &keeping_secrets(&dir));
    runner
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(!calls(&dir).iter().any(|c| c.contains("-var-file")));
    assert!(dir.join("work").join("meridian.tfplan").exists());
}

#[test]
fn an_azure_credential_reaches_every_call_in_its_environment_and_nowhere_else() {
    let dir = scratch(TMP, "azure-credential");
    let runner = runner(&dir, &keeping_secrets(&dir));
    let mut context = context(&dir);
    context.credential = Some(cloud_credential(
        "azure",
        &[
            ("clientId", "c1"),
            ("tenantId", "t1"),
            ("subscriptionId", "s1"),
        ],
        &[("clientSecret", "sp-secret-9fXw")],
    ));
    let log = KeptLog::default();
    runner.plan(&context, &compiled(), &log).unwrap();
    for call in ["init", "plan", "show"] {
        assert_eq!(
            cloud_env(&dir, call),
            [
                "ARM_CLIENT_ID=c1",
                "ARM_CLIENT_SECRET=sp-secret-9fXw",
                "ARM_SUBSCRIPTION_ID=s1",
                "ARM_TENANT_ID=t1",
            ],
            "{call}"
        );
        fs::remove_file(dir.join(format!("cloud-env.{call}"))).unwrap();
    }
    assert!(!calls(&dir).iter().any(|c| c.contains("9fXw")));
    assert!(!log.text().iter().any(|l| l.contains("9fXw")));
    nothing_under_holds(&dir.join("work"), "9fXw");
    runner.apply(&context, &compiled(), &log).unwrap();
    assert!(cloud_env(&dir, "apply").contains(&"ARM_CLIENT_SECRET=sp-secret-9fXw".to_owned()));
}

#[test]
fn an_azure_certificate_is_a_private_file_the_environment_names() {
    let dir = scratch(TMP, "azure-certificate");
    let runner = runner(&dir, &keeping_secrets(&dir));
    let mut context = context(&dir);
    context.credential = Some(cloud_credential(
        "azure",
        &[("clientId", "c1"), ("tenantId", "t1")],
        &[("certificate", "-----BEGIN CERTIFICATE-9fXw-----")],
    ));
    runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    let env = cloud_env(&dir, "plan");
    let path = env
        .iter()
        .find_map(|l| l.strip_prefix("ARM_CLIENT_CERTIFICATE_PATH="))
        .unwrap_or_else(|| panic!("the certificate file is not named: {env:?}"));
    assert!(!Path::new(path).exists(), "{path} was left behind");
    assert!(!Path::new(path).starts_with(dir.join("work")), "{path}");
    assert!(!env.iter().any(|l| l.contains("9fXw")), "{env:?}");
}
