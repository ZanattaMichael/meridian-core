//! Asking a policy before a change is made, run through a real plugin
//! process and a real engine program.
//!
//! The engine here is a shell script that speaks the protocol and stands in
//! for a policy language: each line of its policy file is `verb|needle|
//! message`, and a `deny` or `warn` line whose needle is in the input
//! answers with its message. It also keeps every input it was given beside
//! the policy file, which a real engine must never do, so that the tests
//! can read what an engine is told.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use meridian_ir::Bundle;
use meridian_orchestrator::record;
use meridian_orchestrator::{
    event, run, work_dir, Event, Mode, Observer, OnFailure, Options, Outcome, Policy, Quiet,
    Reason, RunReport, RunState, StageId, StageKind,
};
use meridian_plugin_host::{Registry, StartOptions};
use meridian_sdk::{decode_value, Value};

const ENGINE: &str = r#"#!/bin/sh
input=$(cat)
printf '%s\n' "$input" >> "$(dirname "$1")/seen.jsonl"
deny=""
warn=""
while IFS='|' read -r verb needle message; do
  case "$verb" in
    exit) echo "the engine broke" >&2; exit "$needle" ;;
    garbage) echo "certainly"; exit 0 ;;
    deny|warn)
      if printf '%s' "$input" | grep -qF -- "$needle"; then
        if [ "$verb" = deny ]; then
          deny="$deny${deny:+,}\"$message\""
        else
          warn="$warn${warn:+,}\"$message\""
        fi
      fi ;;
  esac
done < "$1"
printf '{"deny":[%s],"warn":[%s]}\n' "$deny" "$warn"
"#;

/// A machine with a key, and a ResourceSet configured on its address.
const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
  labels: {env: prod}
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
    - id: key
      type: file
      params:
        content: KEY
  outputs:
    - name: address
      value: "${vm.content}"
    - name: secret_key
      value: "${key.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: infrastructure
    ref: network
    output: address
  resources:
    - id: motd
      type: file
      params:
        content: "MOTD"
"#;

fn workflow(key: &str, motd: &str) -> Bundle {
    let text = WORKFLOW.replace("KEY", key).replace("MOTD", motd);
    let bundle = Bundle::new(meridian_ir::parse("workflow.yaml", text.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let dir = meridian_target_fake::plugin_dir();
    registry
        .discover(dir.to_str().unwrap(), &StartOptions::default())
        .expect("the fake plugin loads");
    registry
}

struct Case {
    dir: PathBuf,
    options: Options,
}

/// A scratch directory holding the engine, a policy with these rules, and
/// the state.
fn case(test: &str, rules: &str) -> Case {
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("policy")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::create_dir_all(dir.join("policy")).unwrap();
    let engine = dir.join("bin/meridian-policy-lines");
    fs::write(&engine, ENGINE).unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
    let rules_file = dir.join("policy/rules");
    fs::write(&rules_file, rules).unwrap();
    let policy = Policy::find("lines", &[dir.join("bin")], vec![rules_file]).unwrap();
    let options = Options {
        policy: Some(policy),
        ..Options::new(dir.join("state"))
    };
    Case { dir, options }
}

impl Case {
    fn rules(&self, rules: &str) {
        fs::write(self.dir.join("policy/rules"), rules).unwrap();
    }

    /// Every input the engine has been given, in order.
    fn seen(&self) -> Vec<Value> {
        let Ok(text) = fs::read_to_string(self.dir.join("policy/seen.jsonl")) else {
            return Vec::new();
        };
        text.lines()
            .map(|line| decode_value(line.as_bytes()).unwrap())
            .collect()
    }

    fn forget(&self) {
        let _ = fs::remove_file(self.dir.join("policy/seen.jsonl"));
    }

    fn file(&self, id: &StageId, file: &str) -> Option<String> {
        fs::read_to_string(
            work_dir(&self.options.state_dir, id, "fake")
                .join("current/files")
                .join(file),
        )
        .ok()
    }
}

fn network() -> StageId {
    StageId {
        kind: StageKind::Infrastructure,
        name: "network".to_owned(),
    }
}

fn web() -> StageId {
    StageId {
        kind: StageKind::ResourceSet,
        name: "web".to_owned(),
    }
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |value, key| {
        value
            .as_map()
            .and_then(|map| map.get(*key))
            .unwrap_or_else(|| panic!("no {key:?} in {}", value.canonical()))
    })
}

fn text<'a>(value: &'a Value, path: &[&str]) -> &'a str {
    at(value, path).as_str().unwrap()
}

#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
}

impl Observer for Recorder {
    fn event(&mut self, event: &Event) {
        self.events.push(event.clone());
    }
}

fn failed_with(report: &RunReport, index: usize) -> &str {
    match &report.stages[index].outcome {
        Outcome::Failed(why) => why,
        other => panic!("stage {index} is {other:?}"),
    }
}

#[test]
fn a_denied_change_stops_the_apply_before_anything_is_applied() {
    let case = case(
        "denied",
        "deny|\"content\":\"forbidden\"|that key is not allowed\n",
    );
    let registry = registry();
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("forbidden", "hello"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut recorder,
    );

    assert!(!report.succeeded());
    assert_eq!(report.stages.len(), 2);
    assert_eq!(report.stages[0].reason, Some(Reason::PolicyDenied));
    assert_eq!(
        failed_with(&report, 0),
        "the policy does not allow this change:\n  - that key is not allowed"
    );
    let decisions = report.stages[0].policy.as_ref().unwrap();
    assert!(decisions.asked);
    assert_eq!(decisions.deny, ["that key is not allowed"]);
    assert_eq!(
        report.stages[1].outcome,
        Outcome::NotRun {
            because_of: network()
        }
    );

    // Nothing reached the tool, and the run-state knows of no apply.
    assert_eq!(case.file(&network(), "vm"), None);
    let state = RunState::read(case.options.store().as_ref()).unwrap();
    assert!(state.get(&network()).is_none());

    // The record says a policy denied it, and how many times, and never
    // what the policy said.
    let kinds: Vec<&str> = recorder.events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            event::LOCK_ACQUIRED,
            event::RUN_STARTED,
            event::STEP_STARTED,
            event::POLICY_DENIED,
            event::STEP_FAILED,
            event::STEP_SKIPPED,
            event::RUN_FAILED,
            event::LOCK_RELEASED,
        ]
    );
    let denied = &recorder.events[3];
    assert_eq!(denied.step, "network");
    assert_eq!(denied.detail["denials"], Value::Int(1));
    assert_eq!(denied.detail["warnings"], Value::Int(0));
    assert_eq!(denied.detail["asked"], Value::Bool(true));
    assert_eq!(
        recorder.events[4].detail["reason"],
        Value::from(Reason::PolicyDenied.as_str())
    );
    let store = case.options.store();
    let kept = String::from_utf8(
        store
            .get(&record::events_key(&report.run_id))
            .unwrap()
            .unwrap(),
    )
    .unwrap()
        + &String::from_utf8(
            store
                .get(&record::run_key(&report.run_id))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    assert!(!kept.contains("not allowed"), "{kept}");
    assert!(!kept.contains("forbidden"), "{kept}");
}

#[test]
fn a_stage_that_waits_on_an_output_is_asked_at_its_turn() {
    // On a first apply the ResourceSet's host does not exist yet, so the
    // first pass cannot ask about it.
    let case = case(
        "at-its-turn",
        "deny|\"content\":\"forbidden\"|not that motd\n",
    );
    let registry = registry();
    let report = run(
        &workflow("key-one", "forbidden"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );

    assert!(matches!(report.stages[0].outcome, Outcome::Applied { .. }));
    assert!(report.stages[0].policy.as_ref().unwrap().allowed());
    assert_eq!(report.stages[1].reason, Some(Reason::PolicyDenied));
    assert_eq!(case.file(&network(), "key").as_deref(), Some("key-one"));
    assert_eq!(case.file(&web(), "motd"), None);

    // Asked once about each: the network before anything was applied, the
    // ResourceSet once its host was known.
    let seen = case.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(text(&seen[0], &["stage", "name"]), "network");
    assert_eq!(text(&seen[1], &["stage", "name"]), "web");
    assert_eq!(text(&seen[1], &["stage", "host"]), "10.0.0.5");
}

#[test]
fn a_later_stage_that_is_denied_stops_an_earlier_one_from_being_applied() {
    let case = case("later-denied", "");
    let registry = registry();
    let first = run(
        &workflow("key-one", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert!(first.succeeded(), "{first:?}");
    case.forget();

    // Now the machine exists, so both stages can be asked about up front.
    case.rules("deny|\"content\":\"forbidden\"|not that motd\n");
    let second = run(
        &workflow("key-two", "forbidden"),
        &registry,
        Mode::Apply,
        &Options {
            on_failure: OnFailure::Rollback,
            ..case.options.clone()
        },
        &mut Quiet,
    );
    assert!(!second.succeeded());
    assert_eq!(
        second.stages[0].outcome,
        Outcome::NotRun { because_of: web() }
    );
    assert_eq!(second.stages[1].reason, Some(Reason::PolicyDenied));
    // The key was not changed, and there was nothing to put back.
    assert_eq!(case.file(&network(), "key").as_deref(), Some("key-one"));
    assert_eq!(case.file(&web(), "motd").as_deref(), Some("one"));
    assert!(second.rollbacks.is_empty());
    assert_eq!(case.seen().len(), 2);
}

#[test]
fn an_allowed_change_is_applied_and_a_warning_is_passed_on() {
    let case = case(
        "warned",
        "warn|\"name\":\"network\"|the network is changing\n",
    );
    let registry = registry();
    let first = run(
        &workflow("key-one", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert!(first.succeeded(), "{first:?}");
    case.forget();

    let mut recorder = Recorder::default();
    let second = run(
        &workflow("key-two", "two"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut recorder,
    );
    assert!(second.succeeded(), "{second:?}");
    assert!(matches!(second.stages[0].outcome, Outcome::Applied { .. }));
    let decisions = second.stages[0].policy.as_ref().unwrap();
    assert!(decisions.allowed());
    assert_eq!(decisions.warn, ["the network is changing"]);
    assert!(second.stages[1].policy.as_ref().unwrap().warn.is_empty());
    assert_eq!(case.file(&network(), "key").as_deref(), Some("key-two"));

    // Each stage was asked about once, in the first pass, and not again
    // when its turn came.
    assert_eq!(case.seen().len(), 2);
    let passed: Vec<&Event> = recorder
        .events
        .iter()
        .filter(|e| e.kind == event::POLICY_PASSED)
        .collect();
    assert_eq!(passed.len(), 2);
    assert_eq!(passed[0].step, "network");
    assert_eq!(passed[0].detail["warnings"], Value::Int(1));
    assert_eq!(passed[1].detail["warnings"], Value::Int(0));
}

#[test]
fn the_engine_is_told_the_stage_its_resources_its_artifact_and_its_plan() {
    let case = case("input", "");
    let registry = registry();
    let first = run(
        &workflow("key-one", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert!(first.succeeded(), "{first:?}");

    let seen = case.seen();
    let network = &seen[0];
    assert_eq!(text(network, &["run", "mode"]), "apply");
    assert_eq!(text(network, &["run", "id"]), first.run_id);
    assert_eq!(text(network, &["stage", "name"]), "network");
    assert_eq!(text(network, &["stage", "kind"]), "Infrastructure");
    assert_eq!(text(network, &["stage", "target"]), "fake");
    assert_eq!(text(network, &["stage", "host"]), "");
    assert_eq!(text(network, &["stage", "labels", "env"]), "prod");
    assert!(!text(network, &["stage", "category"]).is_empty());
    let resources = at(network, &["resources"]).as_list().unwrap();
    assert_eq!(resources.len(), 2);
    assert_eq!(text(&resources[1], &["id"]), "key");
    assert_eq!(text(&resources[1], &["type"]), "file");
    assert_eq!(text(&resources[1], &["params", "content"]), "key-one");
    assert!(!at(network, &["artifact", "files"])
        .as_map()
        .unwrap()
        .is_empty());
    let changes = at(network, &["plan", "changes"]).as_list().unwrap();
    assert!(!changes.is_empty());
    assert_eq!(text(&changes[0], &["action"]), "create");
    assert!(!text(&changes[0], &["resource"]).is_empty());
    assert_eq!(at(network, &["plan", "unknown"]), &Value::Bool(false));

    let web = &seen[1];
    assert_eq!(text(web, &["stage", "kind"]), "ResourceSet");
    assert!(at(web, &["stage", "labels"]).as_map().unwrap().is_empty());
}

#[test]
fn an_engine_that_cannot_be_asked_is_a_denial() {
    let registry = registry();
    for (name, rules, says) in [
        (
            "exits",
            "exit|3|\n",
            "exited with status 3: the engine broke",
        ),
        ("garbage", "garbage||\n", "is not JSON"),
    ] {
        let case = case(&format!("closed-{name}"), rules);
        let report = run(
            &workflow("key-one", "one"),
            &registry,
            Mode::Apply,
            &case.options,
            &mut Quiet,
        );
        assert!(!report.succeeded(), "{name}");
        assert_eq!(
            report.stages[0].reason,
            Some(Reason::PolicyDenied),
            "{name}"
        );
        let why = failed_with(&report, 0);
        assert!(
            why.contains("could not be asked, and a policy that was not asked has not allowed"),
            "{why}"
        );
        assert!(why.contains(says), "{why}");
        assert!(!report.stages[0].policy.as_ref().unwrap().asked);
        assert_eq!(case.file(&network(), "vm"), None);
    }

    // And one that is not there at all.
    let case = case("closed-missing", "");
    fs::remove_file(case.dir.join("bin/meridian-policy-lines")).unwrap();
    let report = run(
        &workflow("key-one", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert_eq!(report.stages[0].reason, Some(Reason::PolicyDenied));
    assert!(failed_with(&report, 0).contains("starting"));
    assert_eq!(case.file(&network(), "vm"), None);
}

#[test]
fn a_plan_shows_every_denial_and_fails() {
    let case = case("plan", "");
    let registry = registry();
    let applied = run(
        &workflow("key-one", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert!(applied.succeeded(), "{applied:?}");
    case.forget();

    case.rules(
        "deny|\"name\":\"network\"|no changes to the network\n\
         deny|\"name\":\"web\"|no changes to web\n",
    );
    let plan = run(
        &workflow("key-two", "two"),
        &registry,
        Mode::Plan,
        &case.options,
        &mut Quiet,
    );
    assert!(!plan.succeeded());
    // The plan went on past the first denial.
    assert!(failed_with(&plan, 0).contains("no changes to the network"));
    assert!(failed_with(&plan, 1).contains("no changes to web"));
    let seen = case.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(text(&seen[0], &["run", "mode"]), "plan");
    // Nothing was changed by asking.
    assert_eq!(case.file(&network(), "key").as_deref(), Some("key-one"));

    // With nothing denied the plan is the plan, with the decision beside it.
    case.rules("");
    let plan = run(
        &workflow("key-two", "two"),
        &registry,
        Mode::Plan,
        &case.options,
        &mut Quiet,
    );
    assert!(plan.succeeded(), "{plan:?}");
    assert!(matches!(plan.stages[0].outcome, Outcome::Planned(_)));
    assert!(plan.stages[0].policy.as_ref().unwrap().allowed());
}

#[test]
fn a_message_that_quotes_a_sensitive_output_is_masked() {
    let case = case("masked", "");
    let registry = registry();
    let first = run(
        &workflow("hunter2-very-secret", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert!(first.succeeded(), "{first:?}");

    case.rules("deny|\"name\":\"web\"|web may not use hunter2-very-secret\n");
    let second = run(
        &workflow("hunter2-very-secret", "two"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    let why = failed_with(&second, 1);
    assert!(why.contains("web may not use (sensitive)"), "{why}");
    assert!(!format!("{second:?}").contains("web may not use hunter2"));
}

#[test]
fn a_drift_check_and_a_compile_ask_no_policy() {
    let case = case("not-asked", "");
    let registry = registry();
    let applied = run(
        &workflow("key-one", "one"),
        &registry,
        Mode::Apply,
        &case.options,
        &mut Quiet,
    );
    assert!(applied.succeeded(), "{applied:?}");
    case.forget();
    case.rules("deny|\"name\"|everything is denied\n");

    for mode in [Mode::Drift, Mode::Compile] {
        let report = run(
            &workflow("key-one", "one"),
            &registry,
            mode,
            &case.options,
            &mut Quiet,
        );
        assert!(report.succeeded(), "{mode:?}: {report:?}");
        assert!(report.stages.iter().all(|stage| stage.policy.is_none()));
    }
    assert!(case.seen().is_empty());
}

/// A document that reads a secret, and one its own `when` leaves out.
const SECRETS: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: fake
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: flag
      from: exec
      command: [sh, -c, "echo fetched >> COUNT; printf on"]
  conditions:
    flagged: secret.flag == 'on'
  resources:
    - id: motd
      type: file
      when: cond.flagged
      params:
        content: "welcome"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: spare
spec:
  target: fake
  when: "1 == 2"
  targetHost:
    from: inline
    host: web02
  resources:
    - id: motd
      type: file
      params:
        content: "spare"
"#;

#[test]
fn a_secret_is_fetched_once_at_its_stages_turn_and_not_by_the_first_pass() {
    let case = case("secrets", "warn|never-matches|nothing\n");
    let count = case.dir.join("count");
    let yaml = SECRETS.replace("COUNT", count.to_str().unwrap());
    let bundle = Bundle::new(meridian_ir::parse("workflow.yaml", yaml.as_bytes()).unwrap());
    bundle.validate().unwrap();

    let mut recorder = Recorder::default();
    let report = run(
        &bundle,
        &registry(),
        Mode::Apply,
        &case.options,
        &mut recorder,
    );
    assert!(report.succeeded(), "{report:?}");

    // The first pass asked nothing of the provider; the stage's own turn
    // asked once. A second fetch would prompt a person twice, or find an
    // environment variable already taken.
    assert_eq!(fs::read_to_string(&count).unwrap(), "fetched\n");

    // The stage was still put to the policy, at its turn, before its apply.
    let site = StageId {
        kind: StageKind::ResourceSet,
        name: "site".to_owned(),
    };
    assert!(matches!(report.stages[0].outcome, Outcome::Applied { .. }));
    assert!(report.stages[0].policy.as_ref().unwrap().allowed());
    assert_eq!(case.file(&site, "motd").as_deref(), Some("welcome"));
    let seen = case.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(text(&seen[0], &["stage", "name"]), "site");

    // The document its own `when` leaves out is not asked about, and is
    // recorded as skipped without the condition's text.
    assert!(matches!(
        report.stages[1].outcome,
        Outcome::LeftOut { ref condition } if condition == "1 == 2"
    ));
    assert!(report.stages[1].policy.is_none());
    let skipped: Vec<&Event> = recorder
        .events
        .iter()
        .filter(|e| e.kind == event::STEP_SKIPPED)
        .collect();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].step, "spare");
    assert_eq!(skipped[0].detail["reason"], Value::from("when-false"));
    assert!(!skipped[0]
        .detail
        .values()
        .any(|v| v.canonical().contains("1 == 2")));
}
