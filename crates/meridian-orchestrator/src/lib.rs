//! Runs a workflow.
//!
//! A workflow is a set of documents. Each document is a stage: it is compiled
//! for its target, and then, depending on what was asked, previewed or
//! applied by that target's plugin. Stages run in an order that lets a later
//! one use what an earlier one produced: every Infrastructure document first,
//! then every ResourceSet, each group in the order it was written.
//!
//! Before a document reaches its plugin the orchestrator settles everything
//! that is the core's to settle: whether the document's labels select it for
//! this run, whether its kind is one the target's category takes, whether the
//! target compiles for the version the document asks for, and which resources
//! their `when` conditions leave in.
//!
//! The orchestrator knows nothing about any tool. It routes on what a plugin
//! declares, hands it an artifact and a directory, and reads back changes and
//! outputs. Everything tool-shaped happens behind the plugin boundary.
//!
//! A run stops at the first stage that fails. Stages after it are reported as
//! not run, never attempted against infrastructure in an unknown state.
//!
//! A run that reaches a tool is made safe to repeat and to share:
//!
//! - it holds the state's [lock](lock), so two runs cannot collide;
//! - every change of state is an [event](event), and the events and a
//!   summary are kept as the run's [record](record) in a [backend];
//! - an apply that failed can be [resumed](Resume), skipping the stages that
//!   were already applied from the same artifact;
//! - an apply that failed can instead put back what it changed
//!   ([`OnFailure::Rollback`]), and [`Mode::Rollback`] does the same for the
//!   last apply on request;
//! - a [policy](policy) is asked whether each change may be made, before
//!   the first one is;
//! - [`Mode::Drift`] asks whether the world still matches what was applied.
//!
//! None of it holds a secret value: see `docs/reference/state.md`.

pub mod analyze;
pub mod backend;
pub mod event;
pub mod history;
mod journal;
pub mod lock;
mod options;
pub mod plan;
pub mod policy;
mod reason;
pub mod record;
mod report;
mod rollback;
mod run;
mod secrets;
mod select;
mod stages;
mod state;
pub mod time;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use meridian_ast::ResourceGraph;
use meridian_ir::{
    Bundle, Document, Infrastructure, Removed, Resolution, ResourceSet, Secrets,
    TARGET_HOST_FROM_INFRASTRUCTURE,
};
pub use meridian_plugin_host::{describe_duration, Stop};
use meridian_plugin_host::{name_secrets, to_sdk, EmitError, Registry, RunFailure, StopCause};
use meridian_sdk::{
    Artifact, Category, Change, Data, LogLine, Output, Plan, RunContext, SecretRef, Value, Warning,
};
use meridian_secret::Masker;

pub use analyze::{
    Accepted, Analysis, Analyzer, Baseline, Counts, FailOn, Finding, Measure, Severity,
};
pub use backend::{Backend, ExecBackend, LocalBackend, MemoryBackend};
pub use event::Event;
pub use lock::{LockError, LockInfo};
pub use options::{mask, Observer, OnFailure, Options, Quiet, Resume, DEFAULT_KEEP_RUNS, MASK};
pub use plan::{file_hash, Asked, PlanFile, PlannedStage, PlannedStatus, PLAN_FILE_VERSION};
pub use policy::{Decisions, Policy};
pub use reason::{Reason, UnknownReason};
pub use record::{RunRecord, RunStatus, StepRecord, StepStatus};
pub use report::{Drift, Mode, Outcome, Refusal, RunReport, StageId, StageKind, StageReport};
pub use rollback::{RollbackOutcome, RollbackReport};
pub use run::work_dir;
pub use secrets::{check_declared, names_declared, places_declared, Declared};
pub use select::Selector;
pub use stages::{kind_fits, selected, version_fits};
pub use state::{artifact_hash, stage_key, RunState, StageRecord, STATE_KEY};

use journal::*;
use run::*;
use stages::*;

/// Runs every document in `bundle` through its target.
///
/// The bundle must already be valid: [`Bundle::validate`] is the caller's
/// job, because its errors are about the documents and belong in front of
/// the author before anything starts.
pub fn run(
    bundle: &Bundle,
    registry: &Registry,
    mode: Mode,
    options: &Options,
    observer: &mut dyn Observer,
) -> RunReport {
    run_selected(bundle, registry, mode, options, &Selector::all(), observer)
}

/// [`run`], for only the documents whose labels `selector` matches.
///
/// A document the selector leaves out is not a stage, but it stays loaded: a
/// selected ResourceSet can still name it as the source of its target host,
/// and finds that host from what the document's last apply left behind.
pub fn run_selected(
    bundle: &Bundle,
    registry: &Registry,
    mode: Mode,
    options: &Options,
    selector: &Selector,
    observer: &mut dyn Observer,
) -> RunReport {
    let run_id = time::run_id();
    let backend = options.store();
    // Compiling contacts nothing and changes nothing, so it takes no lock
    // and leaves no record.
    let persist = mode != Mode::Compile;
    let mut report = RunReport {
        run_id: run_id.clone(),
        ..RunReport::default()
    };

    let command = if options.command.is_empty() {
        mode.as_str().to_owned()
    } else {
        options.command.clone()
    };
    let holder = LockInfo::for_this_process(&run_id, &command);
    if persist {
        if let Err(e) = lock::acquire(backend.as_ref(), &holder, options.lock_timeout) {
            report.refused = Some(match e {
                LockError::Held(holder) => Refusal::Locked(holder),
                LockError::Backend(why) => Refusal::State(format!("taking the state lock: {why}")),
            });
            return report;
        }
    }
    // Releases the lock however the run ends, a panic included.
    let mut guard = LockGuard {
        backend: backend.clone(),
        id: run_id.clone(),
        held: persist,
    };

    let state = if persist {
        match RunState::read(backend.as_ref()) {
            Ok(state) => state,
            Err(why) => {
                report.refused = Some(Refusal::State(format!("reading the run-state: {why}")));
                return report;
            }
        }
    } else {
        RunState::default()
    };
    let resume = match (&options.resume, mode) {
        (None, _) => None,
        (Some(_), Mode::Compile | Mode::Plan | Mode::Drift | Mode::Rollback) => {
            report.refused = Some(Refusal::Resume("only an apply can be resumed".to_owned()));
            return report;
        }
        (Some(resume), Mode::Apply) => match Resumed::find(backend.as_ref(), resume) {
            Ok(resumed) => Some(resumed),
            Err(why) => {
                report.refused = Some(Refusal::Resume(why));
                return report;
            }
        },
    };

    if let Some(plan) = &options.from_plan {
        if mode != Mode::Apply {
            report.refused = Some(Refusal::Plan(
                "only an apply carries out a saved plan".to_owned(),
            ));
            return report;
        }
        if resume.is_some() {
            report.refused = Some(Refusal::Plan(
                "a saved plan is applied whole, so it cannot be combined with --resume; apply                  the plan again, or take a new one"
                    .to_owned(),
            ));
            return report;
        }
        // The plan must fit this run before anything runs: the same stages,
        // none of them applied by anyone since the plan was taken.
        let ids: Vec<StageId> = stages(bundle, selector, &options.data)
            .iter()
            .map(Stage::id)
            .collect();
        if let Err(refusal) = plan::check(plan, &ids, &state) {
            report.refused = Some(refusal);
            return report;
        }
    }

    let rolling_back = if mode == Mode::Rollback {
        match rollback::find(backend.as_ref(), &options.rollback_of) {
            Ok(record) => Some(record),
            Err(why) => {
                report.refused = Some(Refusal::Rollback(why));
                return report;
            }
        }
    } else {
        None
    };

    let mut journal = Journal {
        backend: backend.clone(),
        persist,
        fail_on: options.fail_on,
        record: RunRecord {
            id: run_id.clone(),
            mode: mode.as_str().to_owned(),
            command,
            user: holder.user.clone(),
            host: holder.host.clone(),
            selector: selector.to_string(),
            started: time::now(),
            finished: String::new(),
            status: RunStatus::Running,
            resumes: resume.as_ref().map(|r| r.run.clone()).unwrap_or_default(),
            steps: Vec::new(),
        },
        events: Vec::new(),
        warnings: Vec::new(),
    };
    if persist {
        journal.emit(
            observer,
            event::LOCK_ACQUIRED,
            None,
            vec![("id", Value::from(run_id.as_str()))],
        );
    }
    journal.emit(
        observer,
        event::RUN_STARTED,
        None,
        vec![
            ("mode", Value::from(mode.as_str())),
            ("command", Value::from(journal.record.command.as_str())),
            ("resumes", Value::from(journal.record.resumes.as_str())),
            (
                "plan",
                Value::from(
                    options
                        .from_plan
                        .as_ref()
                        .map_or("", |plan| plan.run.as_str()),
                ),
            ),
            (
                "rollsBack",
                Value::from(rolling_back.as_ref().map_or("", |r| r.id.as_str())),
            ),
        ],
    );
    journal.flush();

    let mut run = Run {
        bundle,
        registry,
        mode,
        options,
        outputs: BTreeMap::new(),
        secrets: Vec::new(),
        state,
        backend: backend.clone(),
        run_id: run_id.clone(),
        resume,
        warnings: Vec::new(),
        touched: Vec::new(),
        checked: BTreeMap::new(),
        analyzed: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        preflight: false,
        masker: Masker::new(),
        unlocked: Vec::new(),
        warned_unlocked: false,
    };
    let mut failed: Option<StageId> = None;

    // A rollback runs no stage of its own: it puts back another run's.
    let to_run = if mode == Mode::Rollback {
        Vec::new()
    } else {
        stages(bundle, selector, &options.data)
    };

    // With a policy, an apply asks about every stage it can before it
    // applies the first: a change that is not allowed stops the run while
    // there is still nothing to undo.
    let mut to_run = to_run;
    if mode == Mode::Apply && (options.policy.is_some() || !options.analyzers.is_empty()) {
        run.preflight = true;
        let checks: Vec<StageReport> = to_run
            .iter()
            .map(|stage| run.stage(*stage, observer))
            .collect();
        run.preflight = false;
        let first = checks
            .iter()
            .find(|check| matches!(check.outcome, Outcome::Failed(_)))
            .map(|check| check.id.clone());
        if let Some(first) = first {
            for check in checks {
                let stopped = if matches!(check.outcome, Outcome::Failed(_)) {
                    observer.stage_started(&check.id, &check.target);
                    journal.started(observer, &check.id, &check.target);
                    check
                } else {
                    StageReport {
                        host: String::new(),
                        credential: String::new(),
                        artifact: None,
                        warnings: Vec::new(),
                        outcome: Outcome::NotRun {
                            because_of: first.clone(),
                        },
                        policy: None,
                        analyses: Vec::new(),
                        ..check
                    }
                };
                journal.finished(observer, &stopped);
                observer.stage_finished(&stopped);
                report.stages.push(stopped);
            }
            failed = Some(first);
            to_run = Vec::new();
        }
    }
    for stage in to_run {
        let id = stage.id();
        // The target as the document's interpolations make it, for what
        // is said before the stage has run. The stage itself reads the
        // document again, with its secrets, when its turn comes.
        let target = match meridian_ir::resolve(&stage.document(), &options.data) {
            Ok(Resolution::Kept { document, .. }) => document.target().to_owned(),
            _ => stage.target().to_owned(),
        };
        if let Some(because_of) = &failed {
            let skipped = StageReport {
                id,
                target,
                host: String::new(),
                credential: String::new(),
                artifact: None,
                warnings: Vec::new(),
                removed: Vec::new(),
                outcome: Outcome::NotRun {
                    because_of: because_of.clone(),
                },
                reason: None,
                done_before_failing: Vec::new(),
                previous_good: String::new(),
                policy: None,
                analyses: Vec::new(),
            };
            journal.finished(observer, &skipped);
            observer.stage_finished(&skipped);
            report.stages.push(skipped);
            continue;
        }
        observer.stage_started(&id, &target);
        journal.started(observer, &id, &target);
        let stage_report = run.stage(stage, observer);
        // A plan goes on past a stage its policy denied, so that one plan
        // shows every denial there is; a compile or a plan goes on past a
        // stage an analyzer failed, for the same reason.
        let denied_plan = mode == Mode::Plan && stage_report.reason == Some(Reason::PolicyDenied);
        let found_wanting = matches!(mode, Mode::Compile | Mode::Plan)
            && matches!(
                stage_report.reason,
                Some(Reason::AnalysisFailed | Reason::AnalyzerFailed)
            );
        if matches!(stage_report.outcome, Outcome::Failed(_)) && !denied_plan && !found_wanting {
            failed = Some(stage_report.id.clone());
        }
        journal.finished(observer, &stage_report);
        observer.stage_finished(&stage_report);
        report.stages.push(stage_report);
    }

    if let Some(record) = &rolling_back {
        let items = rollback::items_of(&mut run, record);
        report.rollbacks = rollback::execute(&mut run, &mut journal, observer, items);
    } else if failed.is_some() && mode == Mode::Apply && options.on_failure == OnFailure::Rollback {
        // An operator who stopped the run asked for nothing more to happen;
        // a rollback runs more tools. They can ask for one by name.
        if report.interrupted() {
            run.warnings.push(
                "the run was interrupted, so nothing was rolled back; `meridian rollback` \
                 puts back what it changed"
                    .to_owned(),
            );
        } else {
            let items = std::mem::take(&mut run.touched)
                .into_iter()
                .map(|touched| rollback::Item::Ready(Box::new(touched)))
                .collect();
            report.rollbacks = rollback::execute(&mut run, &mut journal, observer, items);
        }
    }

    let failures = report
        .stages
        .iter()
        .filter(|s| matches!(s.outcome, Outcome::Failed(_)))
        .count();
    journal.record.status = if report.succeeded() {
        RunStatus::Succeeded
    } else {
        RunStatus::Failed
    };
    journal.record.finished = time::now();
    journal.emit(
        observer,
        if report.succeeded() {
            event::RUN_SUCCEEDED
        } else {
            event::RUN_FAILED
        },
        None,
        vec![
            ("steps", Value::Int(report.stages.len() as i64)),
            ("failed", Value::Int(failures as i64)),
            (
                "rolledBack",
                Value::Int(
                    report
                        .rollbacks
                        .iter()
                        .filter(|r| matches!(r.outcome, RollbackOutcome::RolledBack { .. }))
                        .count() as i64,
                ),
            ),
        ],
    );
    if persist {
        journal.emit(
            observer,
            event::LOCK_RELEASED,
            None,
            vec![("id", Value::from(run_id.as_str()))],
        );
    }
    journal.flush();
    if persist {
        if let Err(why) = record::prune(backend.as_ref(), options.keep_runs.max(1)) {
            journal
                .warnings
                .push(format!("removing old run records: {why}"));
        }
        guard.held = false;
        if let Err(why) = lock::release(backend.as_ref(), &run_id) {
            journal.warnings.push(format!(
                "the state lock {run_id} could not be released ({why}); remove it with \
                 `meridian unlock {run_id}`"
            ));
        }
    }
    report.warnings = run.warnings;
    report.warnings.append(&mut journal.warnings);
    report
}
