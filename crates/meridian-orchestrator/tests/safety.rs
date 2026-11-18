//! What makes a run safe to repeat and to share: the lock, the record, the
//! backends and resume, run through real plugin processes.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use meridian_ir::Bundle;
use meridian_orchestrator::backend::{Backend, ExecBackend, LocalBackend, MemoryBackend};
use meridian_orchestrator::record::{self, RunStatus, StepStatus};
use meridian_orchestrator::{
    event, lock, run, work_dir, Event, LockInfo, Mode, Observer, OnFailure, Options, Outcome,
    Quiet, Refusal, Resume, RunState, StageId, StageKind, StageReport, Stop,
};
use meridian_plugin_host::{Registry, StartOptions};
use meridian_sdk::LogLine;

/// A machine, and a ResourceSet configured on whatever address it gets. The
/// machine's document also holds a secret that its tool reports as a
/// sensitive output.
const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
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
        content: hunter2-very-secret
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

fn workflow(motd: &str) -> Bundle {
    bundle(&WORKFLOW.replace("MOTD", motd))
}

fn bundle(text: &str) -> Bundle {
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

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("safety")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn options(test: &str) -> Options {
    Options::new(scratch(test))
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

/// Keeps every event and every line a tool printed.
#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
    logs: Vec<String>,
    finished: Vec<StageReport>,
}

impl Observer for Recorder {
    fn log(&mut self, id: &StageId, line: &LogLine) {
        self.logs.push(format!("{}: {}", id.name, line.text));
    }
    fn stage_finished(&mut self, report: &StageReport) {
        self.finished.push(report.clone());
    }
    fn event(&mut self, event: &Event) {
        self.events.push(event.clone());
    }
}

fn kinds(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

#[test]
fn an_apply_leaves_a_record_its_events_and_no_lock() {
    let registry = registry();
    let options = options("record");
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &Options {
            command: "apply workflow.yaml".to_owned(),
            ..options.clone()
        },
        &mut recorder,
    );
    assert!(report.succeeded(), "{report:?}");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let backend = options.store();

    // The lock was taken and given back.
    assert!(lock::holder(backend.as_ref()).unwrap().is_none());

    // The summary says what was asked, and how each step ended.
    let record = record::load_run(backend.as_ref(), &report.run_id)
        .unwrap()
        .expect("the run was recorded");
    assert_eq!(record.mode, "apply");
    assert_eq!(record.command, "apply workflow.yaml");
    assert_eq!(record.status, RunStatus::Succeeded);
    assert!(record.started <= record.finished, "{record:?}");
    assert_eq!(record.steps.len(), 2);
    let step = record.step(&web()).unwrap();
    assert_eq!(step.status, StepStatus::Succeeded);
    assert_eq!(step.target, "fake");
    assert_eq!(step.endpoint, "10.0.0.5");
    assert_eq!(step.changes, 1);
    assert!(step.artifact.starts_with("sha256:"), "{step:?}");

    // What was kept is exactly what the observer was told, in order.
    let events = record::load_events(backend.as_ref(), &report.run_id).unwrap();
    assert_eq!(events, recorder.events);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1);
        assert_eq!(event.run, report.run_id);
        assert_eq!(event.instance, event.step);
    }
    let all = kinds(&events);
    assert_eq!(&all[..3], ["lock.acquired", "run.started", "step.started"]);
    assert_eq!(
        &all[all.len() - 3..],
        ["step.succeeded", "run.succeeded", "lock.released"]
    );
    let of_web: Vec<&str> = events
        .iter()
        .filter(|e| e.step == "web")
        .map(|e| e.kind.as_str())
        .collect();
    assert_eq!(of_web[0], "step.started");
    assert_eq!(of_web[1], "resource.changed");
    assert_eq!(*of_web.last().unwrap(), "step.succeeded");
    let changed = events
        .iter()
        .find(|e| e.step == "web" && e.kind == event::RESOURCE_CHANGED)
        .unwrap();
    assert_eq!(changed.detail["resource"].as_str(), Some("files/motd"));
    assert_eq!(changed.detail["action"].as_str(), Some("create"));
    assert_eq!(changed.endpoint, "10.0.0.5");
    assert_eq!(changed.attempt, 1);

    // An output is named, and its value is never kept, sensitive or not.
    let produced: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == event::OUTPUT_PRODUCED && e.step == "network")
        .collect();
    assert!(produced
        .iter()
        .any(|e| e.detail["name"].as_str() == Some("secret_key")
            && e.detail["sensitive"] == meridian_sdk::Value::Bool(true)));
    assert!(produced.iter().all(|e| !e.detail.contains_key("value")));

    // Nothing kept holds a secret, in the record or in the run-state.
    for key in backend.list("runs/").unwrap() {
        let text = String::from_utf8(backend.get(&key).unwrap().unwrap()).unwrap();
        assert!(!text.contains("hunter2"), "{key} holds a secret");
        assert!(!text.contains("s3cret"), "{key} holds a secret");
    }

    // The run-state says which run applied each stage.
    let state = RunState::read(backend.as_ref()).unwrap();
    let applied = state.get(&web()).unwrap();
    assert!(applied.applied);
    assert_eq!(applied.run, report.run_id);
    assert_eq!(applied.last_good, applied.artifact_hash);
}

#[test]
fn compiling_takes_no_lock_and_leaves_no_record() {
    let registry = registry();
    let options = options("compile");
    let backend = options.store();
    // Another run holds the state. Compiling contacts nothing, so it is not
    // in that run's way.
    lock::acquire(
        backend.as_ref(),
        &LockInfo::for_this_process("someone-else", "apply"),
        Duration::ZERO,
    )
    .unwrap();
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Compile,
        &options,
        &mut recorder,
    );
    assert!(report.succeeded(), "{report:?}");
    assert!(backend.list("runs/").unwrap().is_empty());
    assert!(backend.get("state.json").unwrap().is_none());
    // It still says what happened to whoever is listening.
    assert_eq!(
        kinds(&recorder.events),
        [
            "run.started",
            "step.started",
            "step.succeeded",
            "step.started",
            "step.deferred",
            "run.succeeded"
        ]
    );
    let deferred = &recorder.events[4];
    assert_eq!(deferred.detail["waitingOn"].as_str(), Some("network"));
    assert_eq!(deferred.detail["output"].as_str(), Some("address"));
}

#[test]
fn a_second_run_is_refused_while_another_holds_the_state() {
    let registry = registry();
    let options = options("locked");
    let backend = options.store();
    let other = LockInfo {
        id: "2026-11-17T09-14-03Z-7f3a".to_owned(),
        user: "mike".to_owned(),
        host: "build-07".to_owned(),
        pid: 4242,
        command: "apply workflow.yaml".to_owned(),
        since: "2026-11-17T09:14:03.000Z".to_owned(),
    };
    lock::acquire(backend.as_ref(), &other, Duration::ZERO).unwrap();

    for mode in [Mode::Plan, Mode::Apply] {
        let mut recorder = Recorder::default();
        let report = run(
            &workflow("welcome"),
            &registry,
            mode,
            &options,
            &mut recorder,
        );
        assert!(!report.succeeded());
        assert_eq!(
            report.refused,
            Some(Refusal::Locked(Box::new(other.clone())))
        );
        // Nothing was attempted, said or recorded.
        assert!(report.stages.is_empty());
        assert!(recorder.events.is_empty());
        assert!(recorder.logs.is_empty());
        assert!(backend.list("runs/").unwrap().is_empty());
        assert!(!work_dir(&options.state_dir, &network(), "fake").exists());
    }
    let said = Refusal::Locked(Box::new(other.clone())).to_string();
    assert!(said.contains("mike@build-07, process 4242"), "{said}");
    assert!(
        said.contains("meridian unlock 2026-11-17T09-14-03Z-7f3a"),
        "{said}"
    );

    // The other run's lock is still its own.
    assert_eq!(lock::holder(backend.as_ref()).unwrap(), Some(other.clone()));

    // Once it is removed, by the run or by hand, the next run goes ahead.
    lock::force_unlock(backend.as_ref(), &other.id).unwrap();
    assert!(run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet
    )
    .succeeded());
}

#[test]
fn a_run_waits_for_the_lock_when_given_a_timeout() {
    let registry = registry();
    let options = Options {
        lock_timeout: Duration::from_secs(20),
        ..options("lock-wait")
    };
    let backend = options.store();
    lock::acquire(
        backend.as_ref(),
        &LockInfo::for_this_process("first", "apply"),
        Duration::ZERO,
    )
    .unwrap();
    let releasing = backend.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        lock::release(releasing.as_ref(), "first").unwrap();
    });
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    releaser.join().unwrap();
    assert!(report.succeeded(), "{report:?}");
}

#[test]
fn a_failed_run_records_the_failure_as_a_fixed_word_and_releases_the_lock() {
    let registry = registry();
    let options = options("failure");
    let backend = options.store();
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("FAIL welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut recorder,
    );
    assert!(!report.succeeded());
    assert!(lock::holder(backend.as_ref()).unwrap().is_none());

    let record = record::load_run(backend.as_ref(), &report.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Failed);
    assert_eq!(
        record.step(&network()).unwrap().status,
        StepStatus::Succeeded
    );
    let failed = record.step(&web()).unwrap();
    assert_eq!(failed.status, StepStatus::Failed);
    assert_eq!(failed.reason, "tool-failed");

    // The terminal is told what the tool said. The record is not: a tool
    // can quote a value in its error.
    let Outcome::Failed(why) = &report.stages[1].outcome else {
        panic!("{:?}", report.stages[1].outcome);
    };
    assert!(why.contains("disk on fire"), "{why}");
    let kept = String::from_utf8(
        backend
            .get(&record::events_key(&report.run_id))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(!kept.contains("disk on fire"), "{kept}");
    assert!(!kept.contains("could not write"), "{kept}");
    let event = recorder
        .events
        .iter()
        .find(|e| e.kind == event::STEP_FAILED)
        .unwrap();
    assert_eq!(event.step, "web");
    assert_eq!(event.detail["reason"].as_str(), Some("tool-failed"));
    assert_eq!(*kinds(&recorder.events).last().unwrap(), "lock.released");
    assert!(kinds(&recorder.events).contains(&"run.failed"));

    // The run-state remembers the failure, and that nothing good came
    // before it.
    let state = RunState::read(backend.as_ref()).unwrap();
    let web_state = state.get(&web()).unwrap();
    assert!(!web_state.applied);
    assert_eq!(web_state.last_good, "");
}

#[test]
fn a_stage_not_run_is_recorded_as_skipped_because_of_the_one_that_failed() {
    let registry = registry();
    let options = options("not-run");
    let broken = WORKFLOW.replace("content: 10.0.0.5", "content: FAIL 10.0.0.5");
    let mut recorder = Recorder::default();
    let report = run(
        &bundle(&broken),
        &registry,
        Mode::Apply,
        &options,
        &mut recorder,
    );
    assert!(!report.succeeded());
    let skipped = recorder
        .events
        .iter()
        .find(|e| e.kind == event::STEP_SKIPPED)
        .unwrap();
    assert_eq!(skipped.step, "web");
    assert_eq!(
        skipped.detail["reason"].as_str(),
        Some("earlier-step-failed")
    );
    assert_eq!(skipped.detail["becauseOf"].as_str(), Some("network"));
    let record = record::load_run(options.store().as_ref(), &report.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.step(&web()).unwrap().status, StepStatus::NotRun);
}

/// Applies a workflow whose second stage fails, and returns the options and
/// the failed run's id.
#[test]
fn a_stage_past_the_time_limit_is_recorded_as_timed_out_and_the_lock_is_released() {
    let registry = registry();
    let options = Options {
        stop: Stop::after(Duration::from_millis(300)),
        ..options("timed-out")
    };
    let backend = options.store();
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("HANG welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut recorder,
    );
    assert!(!report.succeeded());
    assert!(!report.interrupted());
    assert!(lock::holder(backend.as_ref()).unwrap().is_none());

    let record = record::load_run(backend.as_ref(), &report.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Failed);
    let timed_out = record.step(&web()).unwrap();
    assert_eq!(timed_out.status, StepStatus::Failed);
    assert_eq!(timed_out.reason, "timed-out");
    let Outcome::Failed(why) = &report.stages[1].outcome else {
        panic!("{:?}", report.stages[1].outcome);
    };
    assert!(why.starts_with("timed out after 300ms"), "{why}");
    let event = recorder
        .events
        .iter()
        .find(|e| e.kind == event::STEP_FAILED)
        .unwrap();
    assert_eq!(event.detail["reason"].as_str(), Some("timed-out"));
    assert_eq!(*kinds(&recorder.events).last().unwrap(), "lock.released");

    // Fixed, the run is resumed like any failed one: the stage that timed
    // out is applied, the one before it left alone.
    let resumed = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut Quiet,
    );
    assert!(resumed.succeeded(), "{:?}", resumed.stages);
    assert!(matches!(resumed.stages[0].outcome, Outcome::Skipped { .. }));
    assert!(matches!(resumed.stages[1].outcome, Outcome::Applied { .. }));
}

#[test]
fn an_interrupted_apply_is_recorded_as_such_and_is_not_rolled_back_by_itself() {
    let registry = registry();
    let flag = Arc::new(AtomicBool::new(false));
    let options = Options {
        stop: Stop::when(Arc::clone(&flag)),
        on_failure: OnFailure::Rollback,
        ..options("interrupted")
    };
    let backend = options.store();
    // The interrupt arrives while the second stage's tool is waiting.
    let asked = Arc::clone(&flag);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        asked.store(true, Ordering::SeqCst);
    });
    let report = run(
        &workflow("HANG welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(report.interrupted());
    assert!(lock::holder(backend.as_ref()).unwrap().is_none());
    let record = record::load_run(backend.as_ref(), &report.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, RunStatus::Failed);
    assert_eq!(
        record.step(&network()).unwrap().status,
        StepStatus::Succeeded
    );
    let interrupted = record.step(&web()).unwrap();
    assert_eq!(interrupted.status, StepStatus::Failed);
    assert_eq!(interrupted.reason, "interrupted");
    let Outcome::Failed(why) = &report.stages[1].outcome else {
        panic!("{:?}", report.stages[1].outcome);
    };
    assert!(why.starts_with("interrupted: the run was stopped"), "{why}");

    // Nothing was put back: the operator asked for the run to stop, not
    // for more tools to run. They are told how to roll back by hand.
    assert!(report.rollbacks.is_empty(), "{:?}", report.rollbacks);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("nothing was rolled back")),
        "{:?}",
        report.warnings
    );
    assert_eq!(motd_in(&options), "", "the interrupted stage wrote nothing");
}

/// A stage that has not started when the interrupt arrives is interrupted
/// without its tool ever being asked.
#[test]
fn an_interrupt_before_a_stage_starts_does_not_start_its_tool() {
    let registry = registry();
    let flag = Arc::new(AtomicBool::new(true));
    let options = Options {
        stop: Stop::when(flag),
        ..options("interrupted-early")
    };
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(report.interrupted());
    assert_eq!(report.stages[0].reason, "interrupted");
    let Outcome::Failed(why) = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert_eq!(
        why,
        "interrupted: the run was stopped before the tool was started"
    );
    assert!(matches!(report.stages[1].outcome, Outcome::NotRun { .. }));
    assert!(!work_dir(&options.state_dir, &network(), "fake")
        .join("current")
        .exists());
}

/// What the web stage's tool has written, or nothing.
fn motd_in(options: &Options) -> String {
    fs::read_to_string(
        work_dir(&options.state_dir, &web(), "fake")
            .join("current")
            .join("motd"),
    )
    .unwrap_or_default()
}

fn failed_apply(registry: &Registry, test: &str) -> (Options, String) {
    let options = options(test);
    let report = run(
        &workflow("FAIL welcome"),
        registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(!report.succeeded());
    assert!(matches!(report.stages[0].outcome, Outcome::Applied { .. }));
    assert!(matches!(report.stages[1].outcome, Outcome::Failed(_)));
    (options, report.run_id)
}

fn resuming(options: &Options, resume: Resume) -> Options {
    Options {
        resume: Some(resume),
        ..options.clone()
    }
}

#[test]
fn a_resumed_apply_skips_what_was_applied_and_finishes_the_rest() {
    let registry = registry();
    let (options, first) = failed_apply(&registry, "resume");

    // The document that failed is fixed; the one that worked is untouched.
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut recorder,
    );
    assert!(report.succeeded(), "{report:?}");

    // The first stage was not handed to its tool again.
    assert_eq!(
        report.stages[0].outcome,
        Outcome::Skipped {
            applied_in: first.clone()
        }
    );
    assert!(
        !recorder.logs.iter().any(|l| l.starts_with("network:")),
        "the tool ran again: {:?}",
        recorder.logs
    );

    // Its output still reached the stage that needs it, read back from the
    // tool that owns it.
    let web_report = &report.stages[1];
    assert_eq!(web_report.host, "10.0.0.5");
    assert!(matches!(&web_report.outcome, Outcome::Applied { changes, .. } if changes.len() == 1));
    let written = work_dir(&options.state_dir, &web(), "fake").join("current/files/motd");
    assert_eq!(fs::read_to_string(written).unwrap(), "welcome");

    // The record says what was continued and what was skipped.
    let backend = options.store();
    let record = record::load_run(backend.as_ref(), &report.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.resumes, first);
    assert_eq!(record.step(&network()).unwrap().status, StepStatus::Skipped);
    let skipped = recorder
        .events
        .iter()
        .find(|e| e.kind == event::STEP_SKIPPED)
        .unwrap();
    assert_eq!(skipped.detail["reason"].as_str(), Some("already-applied"));
    assert_eq!(skipped.detail["appliedIn"].as_str(), Some(first.as_str()));
    assert_eq!(
        recorder.events[1].detail["resumes"].as_str(),
        Some(first.as_str())
    );

    // The run-state still credits the run that really applied the stage.
    let state = RunState::read(backend.as_ref()).unwrap();
    assert_eq!(state.get(&network()).unwrap().run, first);
    assert_eq!(state.get(&web()).unwrap().run, report.run_id);

    // It succeeded, so there is nothing left to resume.
    let again = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut Quiet,
    );
    let Some(Refusal::Resume(why)) = &again.refused else {
        panic!("{again:?}");
    };
    assert!(
        why.contains(&format!("run {} succeeded", report.run_id)),
        "{why}"
    );
    assert!(again.stages.is_empty());
}

#[test]
fn a_resumed_apply_applies_a_document_that_changed_since() {
    let registry = registry();
    let (options, _) = failed_apply(&registry, "resume-changed");
    let changed = WORKFLOW
        .replace("MOTD", "welcome")
        .replace("hunter2-very-secret", "a-different-key");
    let mut recorder = Recorder::default();
    let report = run(
        &bundle(&changed),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut recorder,
    );
    assert!(report.succeeded(), "{report:?}");
    let Outcome::Applied { changes, .. } = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(recorder.logs.iter().any(|l| l.starts_with("network:")));
}

#[test]
fn a_resumed_apply_applies_again_when_the_tool_cannot_report_its_outputs() {
    let registry = registry();
    let (options, _) = failed_apply(&registry, "resume-no-outputs");
    // The tool has lost its own state, so it can no longer say what the
    // address is. Guessing is not an option, and applying again is safe.
    fs::remove_file(work_dir(&options.state_dir, &network(), "fake").join("state.json")).unwrap();
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut Quiet,
    );
    assert!(report.succeeded(), "{report:?}");
    assert!(matches!(report.stages[0].outcome, Outcome::Applied { .. }));
    assert_eq!(report.stages[1].host, "10.0.0.5");
}

#[test]
fn a_run_can_be_resumed_by_id_more_than_once_and_after_being_interrupted() {
    let registry = registry();
    let (options, first) = failed_apply(&registry, "resume-by-id");
    let backend = options.store();

    // Resuming, and failing again.
    let second = run(
        &workflow("FAIL again"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Run(first.clone())),
        &mut Quiet,
    );
    assert!(!second.succeeded());
    assert!(matches!(second.stages[0].outcome, Outcome::Skipped { .. }));

    // A run that died says `running` for ever. It can be resumed like one
    // that failed: what it finished is still finished.
    let mut died = record::load_run(backend.as_ref(), &second.run_id)
        .unwrap()
        .unwrap();
    died.status = RunStatus::Running;
    died.finished = String::new();
    backend
        .put(&record::run_key(&died.id), died.to_json().as_bytes())
        .unwrap();

    // The last apply is the second one, where the first stage was skipped.
    // Skipped counts as done.
    let third = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut Quiet,
    );
    assert!(third.succeeded(), "{third:?}");
    assert_eq!(
        third.stages[0].outcome,
        Outcome::Skipped {
            applied_in: first.clone()
        }
    );
    let record = record::load_run(backend.as_ref(), &third.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.resumes, second.run_id);
}

#[test]
fn what_cannot_be_resumed_is_refused_with_the_reason() {
    let registry = registry();
    let fresh = options("resume-nothing");
    let refusal = |options: &Options, mode: Mode, resume: Resume| {
        let report = run(
            &workflow("welcome"),
            &registry,
            mode,
            &resuming(options, resume),
            &mut Quiet,
        );
        assert!(report.stages.is_empty(), "{report:?}");
        assert!(lock::holder(options.store().as_ref()).unwrap().is_none());
        match report.refused {
            Some(Refusal::Resume(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    };

    assert_eq!(
        refusal(&fresh, Mode::Apply, Resume::Last),
        "there is no apply to resume: none has been recorded in this state"
    );
    assert_eq!(
        refusal(
            &fresh,
            Mode::Apply,
            Resume::Run("2020-01-01T00-00-00Z-ffff".to_owned())
        ),
        "there is no run 2020-01-01T00-00-00Z-ffff in this state"
    );
    assert_eq!(
        refusal(&fresh, Mode::Apply, Resume::Run("../../etc".to_owned())),
        "\"../../etc\" is not a run id"
    );
    assert_eq!(
        refusal(&fresh, Mode::Plan, Resume::Last),
        "only an apply can be resumed"
    );

    // A plan is a run too, and it is not one that can be continued.
    let planned = run(
        &workflow("welcome"),
        &registry,
        Mode::Plan,
        &fresh,
        &mut Quiet,
    );
    assert!(planned.succeeded());
    let why = refusal(&fresh, Mode::Apply, Resume::Run(planned.run_id.clone()));
    assert_eq!(
        why,
        format!(
            "run {} was a plan, and only an apply can be resumed",
            planned.run_id
        )
    );
    // And `Last` looks past it, for an apply.
    assert_eq!(
        refusal(&fresh, Mode::Apply, Resume::Last),
        "there is no apply to resume: none has been recorded in this state"
    );
}

#[test]
fn only_the_newest_records_are_kept() {
    let registry = registry();
    let options = Options {
        keep_runs: 2,
        ..options("keep-runs")
    };
    let mut ids = Vec::new();
    for _ in 0..4 {
        let report = run(
            &workflow("welcome"),
            &registry,
            Mode::Plan,
            &options,
            &mut Quiet,
        );
        ids.push(report.run_id);
        // Ids sort by the millisecond a run started in.
        std::thread::sleep(Duration::from_millis(3));
    }
    let kept = record::list_runs(options.store().as_ref()).unwrap();
    assert_eq!(kept, ids[2..]);
}

/// A store whose record cannot be written, though the lock and the
/// run-state can.
#[derive(Debug, Default)]
struct NoRecord {
    inner: MemoryBackend,
    asked: AtomicBool,
}

impl Backend for NoRecord {
    fn describe(&self) -> String {
        "a store that is full".to_owned()
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.inner.get(key)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        if key.starts_with("runs/") {
            self.asked.store(true, Ordering::SeqCst);
            return Err("no space left".to_owned());
        }
        self.inner.put(key, bytes)
    }
    fn create(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        self.inner.create(key, bytes)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.inner.delete(key)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        self.inner.list(prefix)
    }
}

#[test]
fn a_record_that_cannot_be_written_is_a_warning_and_not_a_failed_run() {
    let registry = registry();
    let backend = Arc::new(NoRecord::default());
    let options = Options {
        backend: Some(backend.clone()),
        ..options("no-record")
    };
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(report.succeeded(), "{report:?}");
    assert!(backend.asked.load(Ordering::SeqCst));
    assert_eq!(
        report.warnings,
        [format!(
            "the record of run {} could not be written: no space left",
            report.run_id
        )]
    );
    assert!(lock::holder(backend.as_ref()).unwrap().is_none());
    // The tools still did their work, and the run-state says so.
    assert!(
        RunState::read(backend.as_ref())
            .unwrap()
            .get(&web())
            .unwrap()
            .applied
    );
}

/// A store that cannot be reached at all.
#[derive(Debug)]
struct Unreachable;

impl Backend for Unreachable {
    fn describe(&self) -> String {
        "a store that is down".to_owned()
    }
    fn get(&self, _: &str) -> Result<Option<Vec<u8>>, String> {
        Err("connection refused".to_owned())
    }
    fn put(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("connection refused".to_owned())
    }
    fn create(&self, _: &str, _: &[u8]) -> Result<bool, String> {
        Err("connection refused".to_owned())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Err("connection refused".to_owned())
    }
    fn list(&self, _: &str) -> Result<Vec<String>, String> {
        Err("connection refused".to_owned())
    }
}

#[test]
fn a_store_that_cannot_be_reached_stops_the_run_before_anything_is_applied() {
    let registry = registry();
    let options = Options {
        backend: Some(Arc::new(Unreachable)),
        ..options("unreachable")
    };
    let mut recorder = Recorder::default();
    let report = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut recorder,
    );
    assert!(!report.succeeded());
    assert_eq!(
        report.refused,
        Some(Refusal::State(
            "taking the state lock: connection refused".to_owned()
        ))
    );
    assert!(report.stages.is_empty());
    assert!(recorder.logs.is_empty());
    assert!(!work_dir(&options.state_dir, &network(), "fake").exists());
}

/// The reference backend shipped in `examples/backends`, as Meridian would
/// find it in a plugin directory.
fn dir_backend(test: &str) -> (ExecBackend, PathBuf) {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/backends");
    let store = scratch(test).join("shared");
    let settings: BTreeMap<String, String> =
        [("path".to_owned(), store.display().to_string())].into();
    let backend = ExecBackend::find("dir", &[plugins], settings).expect("the backend is found");
    (backend, store)
}

#[test]
fn a_backend_program_keeps_the_same_contract_as_the_built_in_one() {
    let (exec, _) = dir_backend("exec-contract");
    let local = LocalBackend::new(scratch("exec-contract-local"));
    for backend in [&exec as &dyn Backend, &local] {
        assert_eq!(backend.get("state.json").unwrap(), None);
        backend.put("state.json", b"one").unwrap();
        backend.put("state.json", b"two\nlines\n").unwrap();
        assert_eq!(backend.get("state.json").unwrap().unwrap(), b"two\nlines\n");

        assert!(backend.create("lock", b"first").unwrap());
        assert!(!backend.create("lock", b"second").unwrap());
        assert_eq!(backend.get("lock").unwrap().unwrap(), b"first");
        backend.delete("lock").unwrap();
        backend.delete("lock").unwrap();
        assert!(backend.create("lock", b"third").unwrap());

        backend.put("runs/b/run.json", b"{}").unwrap();
        backend.put("runs/a/run.json", b"{}").unwrap();
        backend.put("runs/a/events.jsonl", b"").unwrap();
        assert_eq!(
            backend.list("runs/").unwrap(),
            ["runs/a/events.jsonl", "runs/a/run.json", "runs/b/run.json"],
            "{}",
            backend.describe()
        );
        assert_eq!(backend.list("runs/b/").unwrap(), ["runs/b/run.json"]);
        assert!(backend.list("nothing/").unwrap().is_empty());
        assert!(backend.get("../escape").is_err());
    }
}

#[test]
fn a_backend_program_that_fails_says_why() {
    // With no `path` setting the reference backend refuses to guess one.
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/backends");
    let backend = ExecBackend::find("dir", &[plugins], BTreeMap::new()).unwrap();
    let err = backend.get("state.json").unwrap_err();
    assert_eq!(
        err,
        "state backend \"dir\": get state.json: exit status 2: the dir backend needs \
         --backend-config path=<directory>"
    );
}

#[test]
fn a_whole_run_and_its_resume_work_through_a_backend_program() {
    let registry = registry();
    let (backend, store) = dir_backend("exec-run");
    let backend: Arc<dyn Backend> = Arc::new(backend);
    let options = Options {
        backend: Some(backend.clone()),
        ..options("exec-run-work")
    };

    let failed = run(
        &workflow("FAIL welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(!failed.succeeded());
    assert!(failed.warnings.is_empty(), "{:?}", failed.warnings);

    // The record is in the shared store, and only the tools' working
    // directories are under the state directory.
    assert!(store.join("state.json").is_file());
    assert!(store
        .join("runs")
        .join(&failed.run_id)
        .join("run.json")
        .is_file());
    assert!(!store.join("lock").exists());
    assert!(!options.state_dir.join("state.json").exists());
    assert!(!options.state_dir.join("runs").exists());
    assert!(work_dir(&options.state_dir, &network(), "fake").is_dir());

    // While another machine holds the shared lock, this one is refused.
    let other = LockInfo::for_this_process("another-machine", "apply");
    lock::acquire(backend.as_ref(), &other, Duration::ZERO).unwrap();
    let refused = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(matches!(refused.refused, Some(Refusal::Locked(_))));
    lock::force_unlock(backend.as_ref(), "another-machine").unwrap();

    let resumed = run(
        &workflow("welcome"),
        &registry,
        Mode::Apply,
        &resuming(&options, Resume::Last),
        &mut Quiet,
    );
    assert!(resumed.succeeded(), "{resumed:?}");
    assert_eq!(
        resumed.stages[0].outcome,
        Outcome::Skipped {
            applied_in: failed.run_id.clone()
        }
    );
    assert_eq!(
        record::list_runs(backend.as_ref()).unwrap(),
        [failed.run_id, resumed.run_id]
    );
}
