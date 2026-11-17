//! The running half, against stand-ins for `puppet` and `bolt`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner};
use meridian_target_puppet::{PuppetRunner, Transport};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("manifests/init.pp", "class meridian_web {\n}\n"),
        ("site.pp", "node 'web01' {\n  include meridian_web\n}\n"),
    ])
}

fn context(dir: &Path, host: &str) -> RunContext {
    RunContext {
        document: "web".into(),
        host: host.into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn runner(dir: &Path, puppet: &str, bolt: &str) -> PuppetRunner {
    let record = |name: &str, body: &str| {
        stand_in(
            dir,
            name,
            &format!("echo \"$@\" > \"{}/{name}.args\"\n{body}", dir.display()),
        )
        .to_str()
        .unwrap()
        .to_owned()
    };
    PuppetRunner {
        puppet: record("puppet", puppet),
        bolt: record("bolt", bolt),
        transport: None,
        extra_args: Vec::new(),
    }
}

const NOOP: &str = "cat <<'OUT'
Notice: Compiled catalog for web01 in environment production in 0.05 seconds
Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: current_value 'absent', should be 'present' (noop)
Notice: /Stage[main]/Meridian_web/File[app_conf]/content: current_value '{sha256}aa', should be '{sha256}bb' (noop)
Notice: /Stage[main]/Meridian_web/Service[app_svc]: Would have triggered 'refresh' from 1 event
Notice: Class[Meridian_web]: Would have triggered 'refresh' from 2 events
Notice: Applied catalog in 0.03 seconds
OUT";

#[test]
fn on_the_node_itself_a_plan_is_puppet_apply_noop() {
    let dir = scratch(TMP, "local-plan");
    let log = KeptLog::default();
    let plan = runner(&dir, NOOP, "exit 9")
        .plan(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap().trim(),
        "apply --detailed-exitcodes --color=false --noop apply.pp"
    );
    assert_eq!(
        fs::read_to_string(dir.join("work/apply.pp")).unwrap(),
        "class meridian_web {\n}\n\ninclude meridian_web\n"
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            "create app_pkg: ensure: current_value 'absent', should be 'present' (noop)",
            "update app_conf: content: current_value '{sha256}aa', should be '{sha256}bb' (noop)",
            "update app_svc: Would have triggered 'refresh' from 1 event",
        ]
    );
    assert!(!plan.unknown);
    assert_eq!(log.text().len(), 6);
}

#[test]
fn exit_status_two_means_changed_and_four_means_failed() {
    let dir = scratch(TMP, "local-apply");
    let body = "echo \"Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: created\"";
    let applied = runner(&dir, &format!("{body}\nexit 2"), "exit 9")
        .apply(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        applied.changes[0].to_string(),
        "create app_pkg: ensure: created"
    );
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap().trim(),
        "apply --detailed-exitcodes --color=false apply.pp"
    );

    let err = runner(
        &dir,
        &format!("{body}\necho 'Error: Could not start Service[app_svc]' >&2\nexit 6"),
        "exit 9",
    )
    .apply(
        &context(&dir, "localhost"),
        &compiled(),
        &KeptLog::default(),
    )
    .unwrap_err();
    assert_eq!(err.msg, "puppet apply reported a failed resource");
    assert_eq!(err.exit_code, Some(6));
    assert_eq!(err.changes.len(), 1);
    assert!(err.output.contains("Could not start Service[app_svc]"));
}

const BOLT_REPORT: &str = r#"cat <<'OUT'
{"items":[{"target":"web01","action":"apply","object":null,"status":"success","value":{"report":{"status":"changed","resource_statuses":{"Package[app_pkg]":{"changed":true,"events":[{"property":"ensure","message":"created","status":"success"}]},"Service[app_svc]":{"changed":false,"events":[]}}}}}],"target_count":1,"elapsed_time":3}
OUT"#;

#[test]
fn another_host_is_reached_with_bolt() {
    let dir = scratch(TMP, "bolt-apply");
    let log = KeptLog::default();
    let applied = runner(&dir, "exit 9", BOLT_REPORT)
        .apply(&context(&dir, "web01"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("bolt.args")).unwrap().trim(),
        "apply apply.pp --targets web01 --format json"
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(
        applied.changes[0].to_string(),
        "create app_pkg: ensure: created"
    );
    assert_eq!(log.text(), ["create app_pkg: ensure: created"]);
}

#[test]
fn a_bolt_report_that_cannot_be_read_is_not_taken_for_in_sync() {
    let dir = scratch(TMP, "bolt-unreadable");
    let plan = runner(&dir, "exit 9", "echo 'Finished on web01'")
        .plan(&context(&dir, "web01"), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.unknown);
    assert!(!plan.in_sync());
    assert_eq!(
        fs::read_to_string(dir.join("bolt.args")).unwrap().trim(),
        "apply apply.pp --targets web01 --format json --noop"
    );
}

#[test]
fn the_transport_can_be_forced_and_a_bolt_failure_keeps_its_output() {
    let dir = scratch(TMP, "bolt-failed");
    let mut forced = runner(
        &dir,
        "exit 9",
        "echo '{\"items\":[]}'\necho 'Failed to connect to web01' >&2\nexit 2",
    );
    forced.transport = Some(Transport::Bolt);
    let err = forced
        .apply(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "bolt apply failed");
    assert!(err.output.contains("Failed to connect to web01"));
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, "exit 9", "exit 9")
        .plan(
            &context(&dir, "localhost"),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(err
        .msg
        .starts_with("the artifact has no manifests/init.pp and site.pp"));
}
