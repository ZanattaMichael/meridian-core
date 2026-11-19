//! The running half, against a stand-in for `terraform`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    artifact, cloud_credential, cloud_env, scratch, secrets, stand_in, KeptLog,
};
use meridian_sdk::{Artifact, RunContext, Runner, SecretRef, Value};
use meridian_target_terraform::TerraformRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[("main.tf.json", "{}\n")])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-vm".into(),
        work_dir: dir.join("work"),
        inputs: [("region".to_owned(), Value::String("us-east-1".into()))].into(),
        ..RunContext::default()
    }
}

/// A stand-in that appends each invocation to `calls` and answers by
/// subcommand.
fn runner(dir: &Path, cases: &str) -> TerraformRunner {
    let program = stand_in(
        dir,
        "terraform",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\necho \"$TF_VAR_region\" > \"{dir}/region\"\ncase \"$1\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    TerraformRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

const HAPPY: &str = r#"init) echo 'Terraform has been successfully initialized!' ;;
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
    let dir = scratch(TMP, "plan");
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
    // The JSON is data: only what Terraform says to a person is relayed.
    assert_eq!(
        log.text(),
        [
            "Terraform has been successfully initialized!",
            "Plan: 1 to add, 0 to change, 0 to destroy."
        ]
    );
    assert!(dir.join("work/main.tf.json").exists());
    assert_eq!(
        fs::read_to_string(dir.join("region")).unwrap().trim(),
        "us-east-1"
    );
}

#[test]
fn an_apply_applies_the_plan_it_just_showed_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
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
fn outputs_are_read_without_planning_anything() {
    let dir = scratch(TMP, "outputs");
    let err = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );

    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/main.tf.json"), "{}").unwrap();
    let outputs = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap();
    assert_eq!(calls(&dir), ["output -json -no-color"]);
    assert_eq!(outputs.len(), 2);
}

#[test]
fn a_failed_step_says_which_and_stops_there() {
    let dir = scratch(TMP, "failed");
    let cases =
        "init) echo ok ;;\nplan) echo 'Error: No valid credential sources found' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "terraform plan failed");
    assert_eq!(err.exit_code, Some(1));
    assert!(err.output.contains("No valid credential sources found"));
    assert!(err
        .command
        .ends_with("plan -input=false -no-color -json -out=meridian.tfplan"));
    assert_eq!(calls(&dir).len(), 2, "nothing runs after the failure");
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, HAPPY)
        .plan(
            &context(&dir),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no main.tf.json; it was not compiled by this plugin"
    );
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
    let dir = scratch(TMP, "secrets");
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
    let dir = scratch(TMP, "no-secrets");
    let runner = runner(&dir, &keeping_secrets(&dir));
    runner
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(!calls(&dir).iter().any(|c| c.contains("-var-file")));
    assert!(dir.join("work").join("meridian.tfplan").exists());
}

/// A stand-in that answers like `keeping_secrets` and records the cloud
/// variables each call sees.
fn cloud_runner(dir: &Path) -> TerraformRunner {
    let program = stand_in(
        dir,
        "terraform",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\n{record}case \"$1\" in\n{cases}\nesac",
            dir = dir.display(),
            record = meridian_plugin_testkit::record_cloud_env(dir, "$1"),
            cases = keeping_secrets(dir)
        ),
    );
    TerraformRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

/// The runner's work and the stand-in's records hold nothing of `secret`.
fn nothing_kept(dir: &Path, log: &KeptLog, secret: &str) {
    assert!(
        !calls(dir).iter().any(|c| c.contains(secret)),
        "{:?}",
        calls(dir)
    );
    assert!(
        !log.text().iter().any(|l| l.contains(secret)),
        "{:?}",
        log.text()
    );
    meridian_plugin_testkit::nothing_under_holds(&dir.join("work"), secret);
}

#[test]
fn an_aws_credential_reaches_every_call_in_its_environment_and_nowhere_else() {
    let dir = scratch(TMP, "aws-credential");
    let runner = cloud_runner(&dir);
    let mut context = context(&dir);
    context.credential = Some(cloud_credential(
        "aws",
        &[("accessKeyId", "AKIA1"), ("region", "eu-west-1")],
        &[
            ("secretAccessKey", "s3cr3t-9fXw"),
            ("sessionToken", "t0k3n-9fXw"),
        ],
    ));
    let log = KeptLog::default();
    runner.plan(&context, &compiled(), &log).unwrap();
    // init, plan and show: each is a process of its own, and the providers
    // are loaded by each, so each is told.
    for call in ["init", "plan", "show"] {
        assert_eq!(
            cloud_env(&dir, call),
            [
                "AWS_ACCESS_KEY_ID=AKIA1",
                "AWS_DEFAULT_REGION=eu-west-1",
                "AWS_REGION=eu-west-1",
                "AWS_SECRET_ACCESS_KEY=s3cr3t-9fXw",
                "AWS_SESSION_TOKEN=t0k3n-9fXw",
            ],
            "{call}"
        );
    }
    // The stand-in's own record is the one place the value is written.
    for call in ["init", "plan", "show"] {
        fs::remove_file(dir.join(format!("cloud-env.{call}"))).unwrap();
    }
    nothing_kept(&dir, &log, "s3cr3t-9fXw");
    nothing_kept(&dir, &log, "t0k3n-9fXw");
    // No variables file was passed: a credential is not a secret in a param.
    assert!(!calls(&dir).iter().any(|c| c.contains("-var-file")));

    runner.apply(&context, &compiled(), &log).unwrap();
    assert!(cloud_env(&dir, "apply").contains(&"AWS_SECRET_ACCESS_KEY=s3cr3t-9fXw".to_owned()));
    assert!(cloud_env(&dir, "output").contains(&"AWS_ACCESS_KEY_ID=AKIA1".to_owned()));
}

#[test]
fn a_gcp_key_is_a_private_file_the_environment_names_and_it_is_removed_after_the_run() {
    let dir = scratch(TMP, "gcp-credential");
    let runner = cloud_runner(&dir);
    let mut context = context(&dir);
    let key = "{\"type\": \"service_account\", \"private_key\": \"-----BEGIN-9fXw\"}";
    context.credential = Some(cloud_credential(
        "gcp",
        &[("project", "shop-prod")],
        &[("serviceAccountKey", key)],
    ));
    let log = KeptLog::default();
    runner.plan(&context, &compiled(), &log).unwrap();
    let env = cloud_env(&dir, "plan");
    assert!(
        env.contains(&"GOOGLE_PROJECT=shop-prod".to_owned()),
        "{env:?}"
    );
    assert!(
        env.contains(&"CLOUDSDK_CORE_PROJECT=shop-prod".to_owned()),
        "{env:?}"
    );
    let path = env
        .iter()
        .find_map(|l| l.strip_prefix("GOOGLE_APPLICATION_CREDENTIALS="))
        .expect("the key file is named");
    // The tool read the key from the file while it ran, the file was not in
    // the work directory, and it is gone now.
    assert_eq!(fs::read_to_string(dir.join("gcp-key-seen")).unwrap(), key);
    assert!(!Path::new(path).starts_with(dir.join("work")), "{path}");
    assert!(!Path::new(path).exists(), "{path} was left behind");
    assert!(!env.iter().any(|l| l.contains("9fXw")), "{env:?}");
    for call in ["init", "plan", "show"] {
        fs::remove_file(dir.join(format!("cloud-env.{call}"))).unwrap();
    }
    fs::remove_file(dir.join("gcp-key-seen")).unwrap();
    nothing_kept(&dir, &log, "9fXw");
}

#[test]
fn a_credential_the_providers_cannot_take_stops_the_run_before_the_tool_starts() {
    let dir = scratch(TMP, "ssh-credential");
    let runner = cloud_runner(&dir);
    let mut context = context(&dir);
    context.credential = Some(meridian_plugin_testkit::credential(
        "ssh",
        "deploy",
        &[("privateKey", "-----KEY-9fXw-----")],
    ));
    let err = runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the credential \"deploy\": terraform reaches a cloud, as an aws, azure or gcp \
         credential, not ssh"
    );
    assert!(!dir.join("calls").exists(), "the tool was started");
}

#[test]
fn a_plan_asked_to_save_hands_back_the_plan_file_and_an_apply_handed_it_applies_that() {
    let dir = scratch(TMP, "saved-plan");
    let runner = runner(&dir, &keeping_secrets(&dir));
    let mut context = context(&dir);
    context.save_plan = true;
    let plan = runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    let saved = plan.saved.expect("the plan file is handed back");
    assert_eq!(saved.data, b"PLANDATA\n");

    // The apply is handed the plan back: no `plan`, the file put where
    // the tool saved it, shown and applied.
    let _ = fs::remove_file(dir.join("calls"));
    let _ = fs::remove_file(dir.join("work/meridian.tfplan"));
    let mut context = context.clone();
    context.save_plan = false;
    context.saved_plan = Some(saved);
    let log = KeptLog::default();
    runner.apply(&context, &compiled(), &log).unwrap();
    assert_eq!(
        calls(&dir),
        [
            "init -input=false -no-color",
            "show -json meridian.tfplan",
            "apply -input=false -no-color -json meridian.tfplan",
            "output -json -no-color",
        ]
    );
    assert!(log
        .text()
        .contains(&"applying the plan saved by `meridian plan --out`".to_owned()));
}

#[test]
fn a_plan_that_handed_over_a_secret_saves_nothing() {
    let dir = scratch(TMP, "saved-plan-secret");
    let runner = runner(&dir, &keeping_secrets(&dir));
    let mut context = context(&dir);
    context.save_plan = true;
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2")]);
    let plan = runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.saved.is_none());
    assert!(!dir.join("work").join("meridian.tfplan").exists());
}

#[test]
fn a_debug_level_sets_tf_log_on_every_call_and_level_0_sets_nothing() {
    for (level, expected) in [
        (0, ""),
        (1, "INFO"),
        (2, "DEBUG"),
        (3, "TRACE"),
        (7, "TRACE"),
    ] {
        let dir = scratch(TMP, &format!("debug-{level}"));
        let program = stand_in(
            &dir,
            "terraform",
            &format!(
                "echo \"$1 ${{TF_LOG:-}}\" >> \"{dir}/logs\"\ncase \"$1\" in\n{HAPPY}\nesac",
                dir = dir.display()
            ),
        );
        let runner = TerraformRunner {
            program: program.to_str().unwrap().to_owned(),
            extra_args: Vec::new(),
        };
        let context = RunContext {
            debug_level: level,
            ..context(&dir)
        };
        runner
            .plan(&context, &compiled(), &KeptLog::default())
            .unwrap();
        let seen = fs::read_to_string(dir.join("logs")).unwrap();
        for line in seen.lines() {
            let (_, value) = line.split_once(' ').unwrap();
            assert_eq!(value, expected, "level {level}: {seen}");
        }
        assert_eq!(seen.lines().count(), 3, "{seen}");
    }
}

#[test]
fn a_failed_apply_names_the_resource_and_its_cause_from_the_json_diagnostic() {
    let dir = scratch(TMP, "diagnostic");
    let cases = r#"init) echo ok ;;
plan) echo '{"@message":"Plan: 1 to add","type":"change_summary"}' ;;
show) echo '{"resource_changes":[{"address":"aws_instance.web_vm","mode":"managed","type":"aws_instance","name":"web_vm","change":{"actions":["create"]}}]}' ;;
apply) echo '{"@level":"error","@message":"Error: creating EC2 Instance: InvalidAMIID.NotFound","diagnostic":{"severity":"error","summary":"creating EC2 Instance: InvalidAMIID.NotFound","detail":"The image id does not exist","address":"aws_instance.web_vm"},"type":"diagnostic"}'; exit 1 ;;"#;
    let log = KeptLog::default();
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &log)
        .unwrap_err();
    assert_eq!(err.msg, "terraform apply failed");
    assert_eq!(err.failures.len(), 1);
    assert_eq!(err.failures[0].resource, "web_vm");
    assert_eq!(err.failures[0].cause.as_str(), "resource-failed");
    assert_eq!(err.failures[0].detail, "The image id does not exist");
    // A person reads each line's message, not the JSON.
    assert!(log.text().contains(
        &"Error: creating EC2 Instance: InvalidAMIID.NotFound: The image id does not exist"
            .to_owned()
    ));
}

const DESTROYING: &str = r#"init) echo 'initialized' ;;
plan) echo '{"@message":"Plan: 0 to add, 0 to change, 1 to destroy.","type":"change_summary"}' ;;
show) echo '{"resource_changes":[{"address":"aws_instance.web_vm","mode":"managed","type":"aws_instance","name":"web_vm","change":{"actions":["delete"]}}]}' ;;
apply) echo '{"@message":"Destroy complete! Resources: 1 destroyed.","type":"apply_complete"}' ;;"#;

#[test]
fn a_destroy_plans_with_destroy_and_applies_that_plan() {
    let dir = scratch(TMP, "destroy");
    let runner = runner(&dir, DESTROYING);
    assert!(runner.destroys());
    let removed = runner
        .destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "init -input=false -no-color",
            "plan -input=false -no-color -json -out=meridian.tfplan -destroy",
            "show -json meridian.tfplan",
            "apply -input=false -no-color -json meridian.tfplan",
        ]
    );
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].to_string(), "delete web_vm: aws_instance");
}

#[test]
fn a_destroy_preview_plans_with_destroy_and_applies_nothing() {
    let dir = scratch(TMP, "destroy-preview");
    let plan = runner(&dir, DESTROYING)
        .plan_destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "init -input=false -no-color",
            "plan -input=false -no-color -json -out=meridian.tfplan -destroy",
            "show -json meridian.tfplan",
        ]
    );
    assert_eq!(plan.changes[0].to_string(), "delete web_vm: aws_instance");
    assert!(plan.saved.is_none());
}

const STATE: &str = r#"{"values":{"root_module":{"resources":[{"address":"aws_instance.web","mode":"managed","type":"aws_instance","name":"web","values":{"id":"i-0abc","instance_type":"t3.micro"},"sensitive_values":{}}]}}}"#;

fn inspecting(dir: &Path, plan_exit: u8) -> TerraformRunner {
    runner(
        dir,
        &format!(
            "init) echo 'Terraform has been successfully initialized!' ;;\nshow) echo '{STATE}' ;;\nplan) exit {plan_exit} ;;"
        ),
    )
}

#[test]
fn inspect_reads_the_state_and_asks_a_plan_targeted_at_the_resource() {
    let dir = scratch(TMP, "inspect");
    let context = context(&dir);
    fs::create_dir_all(&context.work_dir).unwrap();
    fs::write(context.work_dir.join("main.tf.json"), "{}\n").unwrap();
    let read = inspecting(&dir, 2)
        .inspect(&context, &compiled(), "web", "")
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "init -input=false -no-color",
            "show -json -no-color",
            "plan -input=false -no-color -lock=false -target=aws_instance.web -detailed-exitcode",
        ]
    );
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(false));
    assert_eq!(
        read.actual.get("instance_type"),
        Some(&Value::from("t3.micro"))
    );
    assert_eq!(
        read.command,
        "terraform show -json; terraform plan -target=aws_instance.web -detailed-exitcode"
    );
}

#[test]
fn inspect_before_any_apply_runs_nothing_and_finds_nothing() {
    let dir = scratch(TMP, "inspect-never");
    let read = inspecting(&dir, 0)
        .inspect(&context(&dir), &compiled(), "web", "")
        .unwrap();
    assert!(!dir.join("calls").exists());
    assert!(!read.found);
    assert_eq!(read.in_desired_state, Some(false));
}

#[test]
fn inspect_names_a_failed_plan() {
    let dir = scratch(TMP, "inspect-fails");
    let context = context(&dir);
    fs::create_dir_all(&context.work_dir).unwrap();
    fs::write(context.work_dir.join("main.tf.json"), "{}\n").unwrap();
    let err = inspecting(&dir, 1)
        .inspect(&context, &compiled(), "web", "")
        .unwrap_err();
    assert!(err.msg.contains("terraform plan failed"), "{}", err.msg);
}
