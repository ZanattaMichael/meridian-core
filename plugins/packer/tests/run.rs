//! The running half, against a stand-in for `packer`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    artifact, cloud_credential, cloud_env, graph_from_file, nothing_under_holds, scratch, secrets,
    stand_in, KeptLog,
};
use meridian_sdk::{Artifact, Data, Emitter, RunContext, Runner, SecretRef, Value};
use meridian_target_packer::{Packer, PackerRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// What the web-image example compiles to: the real artifact, so the runner
/// is tested on exactly what the compile half hands it.
fn compiled() -> Artifact {
    let graph = graph_from_file("examples/packer/web-image.yaml", "packer");
    Packer.emit(&graph, &Data::new()).unwrap().artifact
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-image".into(),
        work_dir: dir.join("work"),
        inputs: [
            ("region".to_owned(), Value::String("eu-west-1".into())),
            ("retries".to_owned(), Value::Int(3)),
        ]
        .into(),
        ..RunContext::default()
    }
}

/// A stand-in that appends each invocation to `calls`, records the variables
/// it was handed, and answers by subcommand.
fn runner(dir: &Path, cases: &str) -> PackerRunner {
    let program = stand_in(
        dir,
        "packer",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\necho \"$PKR_VAR_region $PKR_VAR_retries $PACKER_NO_COLOR $CHECKPOINT_DISABLE\" > \"{dir}/env\"\n{record}case \"$1\" in\n{cases}\nesac",
            dir = dir.display(),
            record = meridian_plugin_testkit::record_cloud_env(dir, "$1"),
        ),
    );
    PackerRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

/// A build that writes the manifest the way Packer does: in its working
/// directory, appending a run to whatever is already there.
const HAPPY: &str = r#"init) echo 'Installed plugin github.com/hashicorp/amazon v1.3.2' ;;
validate) echo 'The configuration is valid.' ;;
build)
  echo '==> web-image.amazon-ebs.ubuntu: Creating temporary keypair'
  echo 'us-east-1: ami-0abc1234' >&2
  cat > meridian-manifest.json <<'JSON'
{"builds":[
  {"name":"ubuntu","builder_type":"amazon-ebs","build_time":1794000000,"files":null,"artifact_id":"us-east-1:ami-0old","packer_run_uuid":"run-1","custom_data":null},
  {"name":"ubuntu","builder_type":"amazon-ebs","build_time":1794003600,"files":null,"artifact_id":"us-east-1:ami-0abc1234","packer_run_uuid":"run-2","custom_data":null}],
 "last_run_uuid":"run-2"}
JSON
  ;;"#;

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_plan_installs_plugins_validates_and_builds_nothing() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(calls(&dir), ["init .", "validate ."]);
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(
        plan.changes[0].to_string(),
        "create ubuntu: amazon-ebs image"
    );
    assert!(!plan.unknown);
    assert!(!plan.in_sync(), "a build always makes a new image");
    assert_eq!(
        log.text(),
        [
            "Installed plugin github.com/hashicorp/amazon v1.3.2",
            "The configuration is valid."
        ]
    );
    assert!(dir.join("work/build.pkr.json").exists());
    assert!(dir.join("work/meridian-build.json").exists());
    assert!(!dir.join("work/meridian-manifest.json").exists());
}

#[test]
fn inputs_reach_packer_as_variables_and_it_runs_in_the_work_directory() {
    let dir = scratch(TMP, "inputs");
    let cases = format!("{HAPPY}\nvalidate) pwd > \"{}/pwd\" ;;", dir.display());
    // The first matching case wins, so the directory is recorded by init.
    let cases = cases.replacen(
        "init) ",
        &format!("init) pwd > \"{}/pwd\"; ", dir.display()),
        1,
    );
    runner(&dir, &cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("env")).unwrap().trim(),
        "eu-west-1 3 1 1"
    );
    let pwd = fs::read_to_string(dir.join("pwd")).unwrap();
    assert_eq!(
        Path::new(pwd.trim()).canonicalize().unwrap(),
        dir.join("work").canonicalize().unwrap()
    );
}

#[test]
fn an_apply_builds_and_reports_what_the_last_run_produced() {
    let dir = scratch(TMP, "apply");
    let log = KeptLog::default();
    let applied = runner(&dir, HAPPY)
        .apply(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        ["init .", "validate .", "build -color=false ."]
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(
        applied.changes[0].to_string(),
        "create ubuntu: amazon-ebs image"
    );
    let output = |name: &str| {
        applied
            .outputs
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("no output {name}"))
    };
    // The image of this run, not the one the manifest still lists before it.
    assert_eq!(
        output("image_id").value,
        Value::String("ami-0abc1234".into())
    );
    assert_eq!(
        output("image_artifact").value,
        Value::String("us-east-1:ami-0abc1234".into())
    );
    assert!(!output("image_id").sensitive);
    // What Packer prints is relayed as it appears, from both streams.
    let relayed = log.text();
    assert!(
        relayed.contains(&"==> web-image.amazon-ebs.ubuntu: Creating temporary keypair".to_owned())
    );
    assert!(relayed.contains(&"us-east-1: ami-0abc1234".to_owned()));
}

#[test]
fn outputs_are_read_back_without_building_anything() {
    let dir = scratch(TMP, "outputs");
    let nothing = "nothing has been built for this document yet, so it has no outputs";
    let err = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap_err();
    assert_eq!(err.msg, nothing);

    // Planned but never built: the template is there and the manifest is not.
    runner(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    let err = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap_err();
    assert_eq!(err.msg, nothing);

    runner(&dir, HAPPY)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    fs::remove_file(dir.join("calls")).unwrap();
    let outputs = runner(&dir, HAPPY).outputs(&context(&dir)).unwrap();
    assert!(calls(&dir).is_empty(), "reading outputs runs nothing");
    assert_eq!(outputs.len(), 2);
    assert!(outputs
        .iter()
        .any(|o| o.name == "image_id" && o.value == Value::String("ami-0abc1234".into())));
}

#[test]
fn a_failed_step_says_which_and_stops_there() {
    let dir = scratch(TMP, "failed-validate");
    let cases = "init) echo ok ;;\nvalidate) echo 'Error: Unsupported argument' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "packer validate failed");
    assert_eq!(err.exit_code, Some(1));
    assert!(err.output.contains("Unsupported argument"));
    assert!(err.command.ends_with("validate ."), "{}", err.command);
    assert_eq!(err.failures[0].cause.as_str(), "invalid-config");
    assert_eq!(err.failures[0].summary, "Unsupported argument");
    assert_eq!(calls(&dir).len(), 2, "nothing is built after the failure");

    let dir = scratch(TMP, "failed-build");
    let cases = "init) ;;\nvalidate) ;;\nbuild) echo \"==> Some builds didn't complete successfully and had errors:\"; echo '--> amazon-ebs.ubuntu: Script exited with non-zero exit status: 7. Allowed exit codes are: [0]'; exit 1 ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "packer build failed");
    let f = &err.failures[0];
    assert_eq!(
        (f.resource.as_str(), f.cause.as_str(), f.tool_code.as_str()),
        ("ubuntu", "resource-failed", "rc=7")
    );
    assert!(err.changes.is_empty(), "a failed build made no image");
}

#[test]
fn a_build_that_leaves_no_manifest_is_an_error_and_not_an_empty_success() {
    let dir = scratch(TMP, "no-manifest");
    let cases = "init) ;;\nvalidate) ;;\nbuild) echo built ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with("packer did not leave the manifest"),
        "{}",
        err.msg
    );
}

#[test]
fn an_output_of_a_source_the_run_did_not_build_is_an_error() {
    let dir = scratch(TMP, "skipped-source");
    let cases = r#"init) ;;
validate) ;;
build) echo '{"builds":[{"name":"other","artifact_id":"x","packer_run_uuid":"r"}],"last_run_uuid":"r"}' > meridian-manifest.json ;;"#;
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "output \"image_artifact\": the last build did not report anything for the source \"ubuntu\""
    );
}

#[test]
fn a_missing_packer_says_so() {
    let dir = scratch(TMP, "missing");
    let err = PackerRunner {
        program: dir.join("no-such-packer").to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
    .plan(&context(&dir), &compiled(), &KeptLog::default())
    .unwrap_err();
    assert!(err.msg.contains("it is not installed"), "{}", err.msg);
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    for files in [
        &[("main.tf.json", "{}")][..],
        // Half of this plugin's artifact is not this plugin's artifact.
        &[("build.pkr.json", "{}")][..],
    ] {
        let err = runner(&dir, HAPPY)
            .plan(&context(&dir), &artifact(files), &KeptLog::default())
            .unwrap_err();
        assert_eq!(
            err.msg,
            "the artifact has no build.pkr.json and meridian-build.json; it was not compiled by \
             this plugin"
        );
    }
    assert!(calls(&dir).is_empty());
}

// ---- secrets ------------------------------------------------------------

/// A stand-in that also keeps a copy of the variables file it is given and
/// records its mode and the environment, so the test can see what reached
/// Packer and where it was.
fn runner_keeping_vars(dir: &Path, cases: &str) -> PackerRunner {
    let program = stand_in(
        dir,
        "packer",
        &format!(
            "for a in \"$@\"; do case \"$a\" in -var-file=*) f=\"${{a#-var-file=}}\"; echo \"$f\" > \"{dir}/secrets-path\"; cp \"$f\" \"{dir}/secrets.json\"; stat -c %a \"$f\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$f\" > \"{dir}/mode\";; esac; done\nenv > \"{dir}/environment\"\necho \"$@\" >> \"{dir}/calls\"\ncase \"$1\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    PackerRunner {
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
fn secrets_reach_packer_as_a_private_variables_file_and_nowhere_else() {
    let dir = scratch(TMP, "secrets");
    let log = KeptLog::default();
    runner_keeping_vars(&dir, HAPPY)
        .apply(&with_secret(&dir), &compiled(), &log)
        .unwrap();
    // validate and build read the file; init has no use for it.
    let calls = calls(&dir);
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0], "init .");
    assert!(
        calls[1].starts_with("validate -var-file=") && calls[1].ends_with(" ."),
        "{}",
        calls[1]
    );
    assert!(
        calls[2].starts_with("build -color=false -var-file=") && calls[2].ends_with(" ."),
        "{}",
        calls[2]
    );
    // The file held the value under the variable's name, as JSON.
    assert_eq!(
        fs::read_to_string(dir.join("secrets.json")).unwrap(),
        "{\"meridian_secret_db_password\":\"hunter2\\\"\\\\\"}\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    // Nothing else held it: not the command line, not the environment, not
    // the log, not the work directory, and the private file is gone.
    for call in &calls {
        assert!(!call.contains("hunter2"), "{call}");
    }
    assert!(!fs::read_to_string(dir.join("environment"))
        .unwrap()
        .contains("hunter2"));
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
    for entry in fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert!(
                !fs::read_to_string(&path).unwrap().contains("hunter2"),
                "{}",
                path.display()
            );
        }
    }
    let path = fs::read_to_string(dir.join("secrets-path")).unwrap();
    assert!(!Path::new(path.trim()).exists(), "{path} was left behind");
    assert!(!Path::new(path.trim()).parent().unwrap().exists());
}

#[test]
fn a_run_without_secrets_names_no_variables_file() {
    let dir = scratch(TMP, "no-secrets");
    runner_keeping_vars(&dir, HAPPY)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(calls(&dir), ["init .", "validate ."]);
    assert!(!dir.join("secrets.json").exists());
}

#[test]
fn a_gcp_key_reaches_packer_as_a_private_file_and_an_aws_login_in_its_environment() {
    let dir = scratch(TMP, "gcp-credential");
    let mut context = context(&dir);
    let key = "{\"type\": \"service_account\", \"private_key\": \"-----BEGIN-9fXw\"}";
    context.credential = Some(cloud_credential(
        "gcp",
        &[("project", "shop-prod")],
        &[("serviceAccountKey", key)],
    ));
    let log = KeptLog::default();
    runner(&dir, HAPPY)
        .plan(&context, &compiled(), &log)
        .unwrap();
    for call in ["init", "validate"] {
        let env = cloud_env(&dir, call);
        assert!(
            env.contains(&"GOOGLE_PROJECT=shop-prod".to_owned()),
            "{call}: {env:?}"
        );
        let path = env
            .iter()
            .find_map(|l| l.strip_prefix("GOOGLE_APPLICATION_CREDENTIALS="))
            .unwrap_or_else(|| panic!("{call}: the key file is not named: {env:?}"));
        assert!(!Path::new(path).exists(), "{path} was left behind");
        assert!(!env.iter().any(|l| l.contains("9fXw")), "{env:?}");
        fs::remove_file(dir.join(format!("cloud-env.{call}"))).unwrap();
    }
    assert_eq!(fs::read_to_string(dir.join("gcp-key-seen")).unwrap(), key);
    fs::remove_file(dir.join("gcp-key-seen")).unwrap();
    nothing_under_holds(&dir, "9fXw");

    let dir = scratch(TMP, "aws-credential");
    let mut with_aws = self::context(&dir);
    with_aws.credential = Some(cloud_credential(
        "aws",
        &[("accessKeyId", "AKIA1"), ("region", "eu-west-1")],
        &[("secretAccessKey", "s3cr3t-9fXw")],
    ));
    runner(&dir, HAPPY)
        .apply(&with_aws, &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        cloud_env(&dir, "build"),
        [
            "AWS_ACCESS_KEY_ID=AKIA1",
            "AWS_DEFAULT_REGION=eu-west-1",
            "AWS_REGION=eu-west-1",
            "AWS_SECRET_ACCESS_KEY=s3cr3t-9fXw",
        ]
    );
    assert!(!calls(&dir).iter().any(|c| c.contains("9fXw")));
}

#[test]
fn an_azure_credential_is_refused_because_the_plugin_reads_its_login_from_the_template() {
    let dir = scratch(TMP, "azure-credential");
    let mut context = context(&dir);
    context.credential = Some(cloud_credential(
        "azure",
        &[("clientId", "c1"), ("tenantId", "t1")],
        &[("clientSecret", "sp-9fXw")],
    ));
    let err = runner(&dir, HAPPY)
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the credential \"prod\": packer reaches a cloud, as an aws or gcp credential, not azure"
    );
    assert!(!dir.join("calls").exists(), "the tool was started");
}
