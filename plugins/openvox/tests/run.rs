//! The running half, against stand-ins for OpenVox's `puppet` and OpenBolt's
//! `bolt`, so it runs on a machine with neither installed. `real.rs` does the
//! same against the real tool.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner};
use meridian_target_openvox::{find_program, OpenVoxRunner, Transport, Verify};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// What the real tools answer when asked for the `implementation` fact.
const OPENVOX: &str = r#"echo '{"implementation":"openvox"}'"#;
const PUPPET: &str = r#"echo '{"implementation":""}'"#;

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

/// A runner over stand-ins that record every invocation, one line each. The
/// `puppet` stand-in answers the fact query with `facts` and anything else
/// with `apply`.
fn runner(dir: &Path, facts: &str, apply: &str, bolt: &str) -> OpenVoxRunner {
    let record = |name: &str, body: &str| {
        stand_in(
            dir,
            name,
            &format!("echo \"$@\" >> \"{}/{name}.args\"\n{body}", dir.display()),
        )
        .to_str()
        .unwrap()
        .to_owned()
    };
    OpenVoxRunner::new(
        record(
            "puppet",
            &format!("if [ \"$1\" = facts ]; then\n{facts}\nexit $?\nfi\n{apply}"),
        ),
        record("bolt", bolt),
    )
}

fn invocations(dir: &Path, name: &str) -> Vec<String> {
    fs::read_to_string(dir.join(format!("{name}.args")))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

const FACT_QUERY: &str = "facts show implementation --render-as json";

const NOOP: &str = "cat <<'OUT'
Notice: Compiled catalog for web01 in environment production in 0.05 seconds
Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: current_value 'absent', should be 'present' (noop)
Notice: /Stage[main]/Meridian_web/File[app_conf]/content: current_value '{sha256}aa', should be '{sha256}bb' (noop)
Notice: /Stage[main]/Meridian_web/Service[app_svc]: Would have triggered 'refresh' from 1 event
Notice: Class[Meridian_web]: Would have triggered 'refresh' from 2 events
Notice: Applied catalog in 0.03 seconds
OUT";

#[test]
fn on_the_node_itself_a_plan_is_a_noop_run_of_openvox() {
    let dir = scratch(TMP, "local-plan");
    let log = KeptLog::default();
    let plan = runner(&dir, OPENVOX, NOOP, "exit 9")
        .plan(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        invocations(&dir, "puppet"),
        [
            FACT_QUERY,
            "apply --detailed-exitcodes --color=false --noop apply.pp"
        ]
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
    // The fact query is the plugin's own question; none of it reaches the
    // operator, and OpenVox being OpenVox is not worth a line.
    assert_eq!(log.text().len(), 6);
    assert!(invocations(&dir, "bolt").is_empty());
}

#[test]
fn the_tool_is_asked_what_it_is_once_however_many_runs_follow() {
    let dir = scratch(TMP, "asked-once");
    let runner = runner(&dir, OPENVOX, "exit 0", "exit 9");
    let context = context(&dir, "localhost");
    runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    runner
        .apply(&context, &compiled(), &KeptLog::default())
        .unwrap();
    runner
        .plan(&context, &compiled(), &KeptLog::default())
        .unwrap();
    let ran = invocations(&dir, "puppet");
    assert_eq!(ran.iter().filter(|l| *l == FACT_QUERY).count(), 1);
    assert_eq!(ran.len(), 4);
}

#[test]
fn a_puppet_that_is_not_openvox_is_run_with_a_warning_by_default() {
    let dir = scratch(TMP, "warned");
    let log = KeptLog::default();
    let runner = runner(&dir, PUPPET, "exit 0", "exit 9");
    assert_eq!(runner.verify, Verify::Warn);
    let applied = runner
        .apply(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap();
    assert!(applied.changes.is_empty());
    let lines = log.text();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("Warning: "), "{lines:?}");
    assert!(
        lines[0].contains("is not OpenVox: it has no implementation fact"),
        "{lines:?}"
    );
    assert!(
        lines[0].contains("MERIDIAN_OPENVOX_VERIFY=strict"),
        "{lines:?}"
    );
    assert_eq!(invocations(&dir, "puppet").len(), 2);

    // And every run says so again: a warning seen once in a plan would be
    // easy to have forgotten by the apply.
    let again = KeptLog::default();
    runner
        .plan(&context(&dir, "localhost"), &compiled(), &again)
        .unwrap();
    assert_eq!(again.text().len(), 1);
}

#[test]
fn strict_verification_refuses_before_anything_is_applied() {
    let dir = scratch(TMP, "strict");
    let log = KeptLog::default();
    let err = runner(&dir, PUPPET, "exit 0", "exit 9")
        .with_verify(Verify::Strict)
        .apply(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap_err();
    assert!(err.msg.contains("is not OpenVox"), "{err}");
    assert!(err.msg.contains("MERIDIAN_OPENVOX"), "{err}");
    assert!(err.changes.is_empty());
    assert_eq!(invocations(&dir, "puppet"), [FACT_QUERY]);
    assert!(!dir.join("work").exists(), "nothing was written");
    assert!(log.text().is_empty());
}

#[test]
fn another_implementation_is_named_in_the_refusal() {
    let dir = scratch(TMP, "named");
    let err = runner(
        &dir,
        r#"echo '{"implementation":"puppet"}'"#,
        "exit 0",
        "exit 9",
    )
    .with_verify(Verify::Strict)
    .plan(
        &context(&dir, "localhost"),
        &compiled(),
        &KeptLog::default(),
    )
    .unwrap_err();
    assert!(
        err.msg
            .contains("is not OpenVox: its implementation fact reads \"puppet\""),
        "{err}"
    );
}

#[test]
fn a_tool_that_cannot_answer_the_question_is_not_openvox() {
    // A release with no `facts show`, or one that prints something else.
    for (name, facts) in [
        (
            "unanswered-status",
            "echo 'Error: no such action' >&2\nexit 1",
        ),
        ("unanswered-text", "echo 'implementation => openvox'"),
        (
            "unanswered-failed",
            "echo '{\"implementation\":\"openvox\"}'\nexit 1",
        ),
    ] {
        let dir = scratch(TMP, name);
        let err = runner(&dir, facts, "exit 0", "exit 9")
            .with_verify(Verify::Strict)
            .plan(
                &context(&dir, "localhost"),
                &compiled(),
                &KeptLog::default(),
            )
            .unwrap_err();
        assert!(err.msg.contains("is not OpenVox"), "{name}: {err}");
    }
}

#[test]
fn verification_can_be_turned_off_and_then_nothing_is_asked() {
    let dir = scratch(TMP, "unverified");
    let log = KeptLog::default();
    runner(&dir, "exit 7", "exit 0", "exit 9")
        .with_verify(Verify::Off)
        .apply(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        invocations(&dir, "puppet"),
        ["apply --detailed-exitcodes --color=false apply.pp"]
    );
    assert!(log.text().is_empty());
}

#[test]
fn a_missing_tool_is_reported_as_missing_not_as_the_wrong_product() {
    let dir = scratch(TMP, "missing");
    let missing = dir.join("no-such-puppet");
    let err = OpenVoxRunner::new(missing.to_str().unwrap(), "bolt")
        .with_verify(Verify::Strict)
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(err.msg.contains("it is not installed"), "{err}");
    assert!(!err.msg.contains("is not OpenVox"), "{err}");
}

#[test]
fn exit_status_two_means_changed_and_six_means_a_resource_failed() {
    let dir = scratch(TMP, "local-apply");
    let body = "echo \"Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: created\"";
    let applied = runner(&dir, OPENVOX, &format!("{body}\nexit 2"), "exit 9")
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
    assert!(applied.outputs.is_empty());

    let err = runner(
        &dir,
        OPENVOX,
        &format!("{body}\necho 'Error: Could not start Service[app_svc]' >&2\nexit 6"),
        "exit 9",
    )
    .apply(
        &context(&dir, "localhost"),
        &compiled(),
        &KeptLog::default(),
    )
    .unwrap_err();
    assert_eq!(err.exit_code, Some(6));
    assert_eq!(err.changes.len(), 1, "what was done before the failure");
    assert!(err.output.contains("Could not start Service[app_svc]"));
}

const BOLT_REPORT: &str = r#"cat <<'OUT'
{"items":[{"target":"web01","action":"apply","object":null,"status":"success","value":{"report":{"status":"changed","resource_statuses":{"Package[app_pkg]":{"changed":true,"events":[{"property":"ensure","message":"created","status":"success"}]},"Service[app_svc]":{"changed":false,"events":[]}}}}}],"target_count":1,"elapsed_time":3}
OUT"#;

#[test]
fn another_host_is_reached_with_openbolt_and_the_local_tool_is_not_asked() {
    let dir = scratch(TMP, "bolt-apply");
    let log = KeptLog::default();
    let applied = runner(&dir, "exit 7", "exit 9", BOLT_REPORT)
        .with_verify(Verify::Strict)
        .apply(&context(&dir, "web01"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        invocations(&dir, "bolt"),
        ["apply apply.pp --targets web01 --format json"]
    );
    // The node's agent applies the catalog, so what `puppet` is on this
    // machine has no bearing and is never run.
    assert!(invocations(&dir, "puppet").is_empty());
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(log.text(), ["create app_pkg: ensure: created"]);
}

#[test]
fn a_report_that_cannot_be_read_is_not_taken_for_in_sync() {
    let dir = scratch(TMP, "bolt-unreadable");
    let plan = runner(&dir, OPENVOX, "exit 9", "echo 'Finished on web01'")
        .plan(&context(&dir, "web01"), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.unknown);
    assert!(!plan.in_sync());
    assert_eq!(
        invocations(&dir, "bolt"),
        ["apply apply.pp --targets web01 --format json --noop"]
    );
}

#[test]
fn the_transport_and_extra_arguments_can_be_set() {
    let dir = scratch(TMP, "forced");
    let mut forced = runner(&dir, OPENVOX, "exit 0", "echo '{\"items\":[]}'");
    forced.tool.transport = Some(Transport::Bolt);
    forced.tool.extra_args = vec!["--run-as".into(), "root".into()];
    forced
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        invocations(&dir, "bolt"),
        ["apply apply.pp --targets localhost --format json --noop --run-as root"]
    );
    assert!(invocations(&dir, "puppet").is_empty());

    let dir = scratch(TMP, "forced-local");
    let mut forced = runner(&dir, OPENVOX, "exit 0", "exit 9");
    forced.tool.transport = Some(Transport::Local);
    forced.tool.extra_args = vec!["--environment".into(), "staging".into()];
    forced
        .apply(&context(&dir, "web01"), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        invocations(&dir, "puppet"),
        [
            FACT_QUERY,
            "apply --detailed-exitcodes --color=false --environment staging apply.pp"
        ]
    );
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, OPENVOX, "exit 9", "exit 9")
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

#[test]
fn the_tool_is_looked_for_on_the_path_and_then_where_the_packages_put_it() {
    let dir = scratch(TMP, "found");
    let on_path = dir.join("bin");
    let packaged = dir.join("opt/puppetlabs/bin");
    let empty = dir.join("empty");
    fs::create_dir_all(&empty).unwrap();
    stand_in(&on_path, "puppet", "exit 0");
    let installed = stand_in(&packaged, "puppet", "exit 0");
    let bolt = stand_in(&packaged, "bolt", "exit 0");
    // Not executable, so not a program.
    fs::write(empty.join("puppet"), "").unwrap();

    let path = std::env::join_paths([&empty, &on_path]).unwrap();
    let fallbacks = [empty.as_path(), packaged.as_path()];
    // On the PATH: the bare name, so the operator's PATH decides which.
    assert_eq!(find_program("puppet", Some(&path), &fallbacks), "puppet");
    // Not on the PATH: the packaged one, by its full path.
    assert_eq!(
        find_program("bolt", Some(&path), &fallbacks),
        bolt.to_str().unwrap()
    );
    assert_eq!(
        find_program("puppet", Some(OsStr::new("")), &fallbacks),
        installed.to_str().unwrap()
    );
    // Nowhere: the bare name, and running it says it is not installed.
    assert_eq!(find_program("facter", Some(&path), &fallbacks), "facter");
}
