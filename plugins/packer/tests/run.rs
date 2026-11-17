//! The running half, against a stand-in for `packer`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, graph_from_file, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, Data, Emitter, RunContext, Runner, Value};
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
            "echo \"$@\" >> \"{dir}/calls\"\necho \"$PKR_VAR_region $PKR_VAR_retries $PACKER_NO_COLOR $CHECKPOINT_DISABLE\" > \"{dir}/env\"\ncase \"$1\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    PackerRunner {
        program: program.to_str().unwrap().to_owned(),
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
    assert_eq!(calls(&dir).len(), 2, "nothing is built after the failure");

    let dir = scratch(TMP, "failed-build");
    let cases = "init) ;;\nvalidate) ;;\nbuild) echo 'Build was halted.' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "packer build failed");
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
