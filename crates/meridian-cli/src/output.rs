//! Machine-readable output: `--output json`.
//!
//! In `json` every command prints exactly one JSON object on standard output,
//! at its end, and the report for people moves to standard error, as it does
//! with `--events json`. Every object carries `command`, the subcommand as
//! it is typed, and `ok`, whether the command did what was asked; the rest
//! is the command's own and is described, object by object, in
//! `docs/reference/cli-json.md` and by the schemas under `schemas/output/`.
//!
//! The objects are built here from the same values the text printer reads,
//! so the two can never disagree about what happened. A sensitive output is
//! masked exactly as the text is, and nothing here reads a document, an
//! artifact's content or a tool's output, which is what keeps a secret out.

use super::*;

use serde_json::{json, Map, Value as Json};

/// How a command's result is printed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Text for a person
    #[default]
    Text,
    /// One JSON object, described in docs/reference/cli-json.md
    Json,
}

static FORMAT: OnceLock<OutputFormat> = OnceLock::new();
static COMMAND: OnceLock<String> = OnceLock::new();

/// Settles, once, how the command prints and which command it is.
pub(crate) fn settle_output(format: OutputFormat, command: &str) {
    let _ = FORMAT.set(format);
    let _ = COMMAND.set(command.to_owned());
}

/// Whether the result is to be one JSON object.
pub(crate) fn json_output() -> bool {
    FORMAT.get().copied().unwrap_or_default() == OutputFormat::Json
}

/// The subcommand as it is typed, such as `plan` or `module verify`.
pub(crate) fn command_name() -> &'static str {
    COMMAND.get().map_or("", String::as_str)
}

/// Prints the command's one object: `command` and `ok` first, then `body`.
pub(crate) fn emit(ok: bool, body: Map<String, Json>) {
    let mut object = Map::new();
    object.insert("command".to_owned(), Json::from(command_name()));
    object.insert("ok".to_owned(), Json::from(ok));
    object.extend(body);
    let text = serde_json::to_string_pretty(&Json::Object(object))
        .expect("a JSON object made here renders");
    outln!("{text}");
}

/// Prints the object for a command that failed before it had a result.
pub(crate) fn emit_error(message: &str) {
    emit(false, body([("error", json!({ "message": message }))]));
}

/// A body from a few entries, in the order given.
pub(crate) fn body<const N: usize>(entries: [(&str, Json); N]) -> Map<String, Json> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// A text that is `null` when empty: the records keep "none" as "".
fn text_or_null(text: &str) -> Json {
    if text.is_empty() {
        Json::Null
    } else {
        Json::from(text)
    }
}

/// A document value as JSON. Its canonical form is JSON already.
pub(crate) fn value(value: &meridian_sdk::Value) -> Json {
    serde_json::from_str(&value.canonical()).unwrap_or_else(|_| Json::from(value.canonical()))
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// A problem `validate` found, with where it is.
pub(crate) fn document_error(error: &meridian_ir::Error) -> Json {
    json!({
        "file": error.pos.file,
        "line": error.pos.line,
        "column": error.pos.column,
        "path": error.path,
        "message": error.msg,
    })
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

fn stage_id(id: &StageId) -> Json {
    json!({ "name": id.name, "kind": id.kind.as_str() })
}

pub(crate) fn change(change: &meridian_sdk::Change) -> Json {
    json!({
        "resource": change.resource,
        "action": change.action.as_str(),
        "summary": change.summary,
    })
}

fn changes(changes: &[meridian_sdk::Change]) -> Json {
    Json::Array(changes.iter().map(change).collect())
}

/// An output as a person may see it: a sensitive value is the mask, and the
/// object says so.
pub(crate) fn output(output: &Output) -> Json {
    json!({
        "name": output.name,
        "value": if output.sensitive { Json::from(MASK) } else { value(&output.value) },
        "sensitive": output.sensitive,
    })
}

fn warning(warning: &meridian_sdk::Warning) -> Json {
    json!({
        "target": warning.target,
        "resource": text_or_null(&warning.resource),
        "message": warning.msg,
    })
}

fn analysis(analysis: &Analysis) -> Json {
    let counts = analysis.counts();
    json!({
        "analyzer": analysis.analyzer,
        "asked": analysis.asked,
        "error": text_or_null(&analysis.error),
        "counts": {
            "critical": counts.critical,
            "high": counts.high,
            "medium": counts.medium,
            "low": counts.low,
            "info": counts.info,
            "accepted": counts.accepted,
        },
        "findings": analysis.findings.iter().map(|finding| json!({
            "rule": finding.rule,
            "severity": finding.severity.as_str(),
            "message": finding.message,
            "file": text_or_null(&finding.file),
            "line": if finding.line == 0 { Json::Null } else { Json::from(finding.line) },
            "resource": text_or_null(&finding.resource),
            "document": text_or_null(&finding.document),
            "documentLine": if finding.document_line == 0 { Json::Null } else { Json::from(finding.document_line) },
            "url": text_or_null(&finding.url),
            "accepted": finding.accepted,
        })).collect::<Vec<_>>(),
        "measures": analysis.measures.iter().map(|measure| json!({
            "name": measure.name,
            "value": measure.value,
            "unit": text_or_null(&measure.unit),
        })).collect::<Vec<_>>(),
    })
}

fn policy(decisions: &Decisions) -> Json {
    json!({
        "allowed": decisions.allowed(),
        "asked": decisions.asked,
        "deny": decisions.deny,
        "warn": decisions.warn,
    })
}

/// Whether a planned stage leaves something for an apply to do. A tool that
/// cannot preview may change things, and a stage waiting on an output that
/// does not exist yet is waiting on an apply, so both count.
pub(crate) fn stage_has_changes(stage: &StageReport) -> bool {
    match &stage.outcome {
        Outcome::Planned(plan) => plan.unknown || !plan.changes.is_empty(),
        Outcome::Deferred { .. } => true,
        _ => false,
    }
}

/// What one stage did. `status` says which of the other fields are there.
pub(crate) fn stage(stage: &StageReport, mode: Mode, out: Option<&Path>) -> Json {
    let mut object = body([
        ("name", Json::from(stage.id.name.as_str())),
        ("kind", Json::from(stage.id.kind.as_str())),
        ("target", Json::from(stage.target.as_str())),
        ("host", text_or_null(&stage.host)),
        ("credential", text_or_null(&stage.credential)),
    ]);
    let status = match &stage.outcome {
        Outcome::Compiled => "compiled",
        Outcome::Planned(_) => "planned",
        Outcome::Applied { .. } => "applied",
        Outcome::Deferred { .. } => "deferred",
        Outcome::Skipped { .. } => "skipped",
        Outcome::Drift(_) => "drift",
        Outcome::LeftOut { .. } => "left-out",
        Outcome::Failed(_) => "failed",
        Outcome::NotRun { .. } => "not-run",
    };
    object.insert("status".to_owned(), Json::from(status));
    object.insert(
        "reason".to_owned(),
        stage.reason.map_or(Json::Null, |r| Json::from(r.as_str())),
    );
    object.insert(
        "artifact".to_owned(),
        match &stage.artifact {
            None => Json::Null,
            Some(artifact) => json!({
                "hash": meridian_orchestrator::artifact_hash(artifact),
                "files": artifact.files.keys().collect::<Vec<_>>(),
            }),
        },
    );
    if let (Mode::Compile, Some(dir), Some(_)) = (mode, out, &stage.artifact) {
        object.insert(
            "writtenTo".to_owned(),
            Json::from(dir.join(&stage.id.name).display().to_string()),
        );
    }
    object.insert(
        "warnings".to_owned(),
        Json::Array(stage.warnings.iter().map(warning).collect()),
    );
    object.insert(
        "removed".to_owned(),
        Json::Array(
            stage
                .removed
                .iter()
                .map(|r| json!({ "subject": r.subject, "condition": r.condition }))
                .collect(),
        ),
    );
    object.insert(
        "policy".to_owned(),
        stage.policy.as_ref().map_or(Json::Null, policy),
    );
    object.insert(
        "analyses".to_owned(),
        Json::Array(stage.analyses.iter().map(analysis).collect()),
    );
    match &stage.outcome {
        Outcome::Compiled => {}
        Outcome::Planned(plan) => {
            object.insert("changes".to_owned(), changes(&plan.changes));
            object.insert("unknown".to_owned(), Json::from(plan.unknown));
        }
        Outcome::Applied {
            changes: made,
            outputs,
        } => {
            object.insert("changes".to_owned(), changes(made));
            object.insert(
                "outputs".to_owned(),
                Json::Array(outputs.iter().map(output).collect()),
            );
        }
        Outcome::Deferred {
            waiting_on,
            output: name,
        } => {
            object.insert("waitingOn".to_owned(), stage_id(waiting_on));
            object.insert("waitingFor".to_owned(), Json::from(name.as_str()));
        }
        Outcome::Skipped { applied_in } => {
            object.insert("appliedIn".to_owned(), Json::from(applied_in.as_str()));
        }
        Outcome::Drift(drift) => {
            let (word, found, last_apply_failed) = match drift {
                Drift::NeverApplied => ("never-applied", None, None),
                Drift::InSync => ("in-sync", None, None),
                Drift::Unknown => ("unknown", None, None),
                Drift::Drifted(found) => ("drifted", Some(found), None),
                Drift::Pending {
                    changes,
                    last_apply_failed,
                } => ("pending", Some(changes), Some(*last_apply_failed)),
            };
            object.insert("drift".to_owned(), Json::from(word));
            object.insert(
                "changes".to_owned(),
                found.map_or_else(|| Json::Array(Vec::new()), |c| changes(c)),
            );
            if let Some(failed) = last_apply_failed {
                object.insert("lastApplyFailed".to_owned(), Json::from(failed));
            }
        }
        Outcome::LeftOut { condition } => {
            object.insert("condition".to_owned(), Json::from(condition.as_str()));
        }
        Outcome::Failed(why) => {
            object.insert("changes".to_owned(), changes(&stage.done_before_failing));
            object.insert("error".to_owned(), Json::from(why.as_str()));
        }
        Outcome::NotRun { because_of } => {
            object.insert("becauseOf".to_owned(), stage_id(because_of));
        }
    }
    object.insert(
        "previousGood".to_owned(),
        text_or_null(&stage.previous_good),
    );
    Json::Object(object)
}

fn rollback(report: &RollbackReport) -> Json {
    let mut object = body([
        ("name", Json::from(report.id.name.as_str())),
        ("kind", Json::from(report.id.kind.as_str())),
        ("target", Json::from(report.target.as_str())),
        ("host", text_or_null(&report.host)),
    ]);
    let status = match &report.outcome {
        RollbackOutcome::RolledBack { .. } => "rolled-back",
        RollbackOutcome::Unchanged => "unchanged",
        RollbackOutcome::Unavailable(_) => "unavailable",
        RollbackOutcome::Failed(_) => "failed",
        RollbackOutcome::NotAttempted { .. } => "not-attempted",
    };
    object.insert("status".to_owned(), Json::from(status));
    match &report.outcome {
        RollbackOutcome::RolledBack { to, changes: made } => {
            object.insert("to".to_owned(), Json::from(to.as_str()));
            object.insert("changes".to_owned(), changes(made));
        }
        RollbackOutcome::Unchanged => {}
        RollbackOutcome::Unavailable(why) | RollbackOutcome::Failed(why) => {
            object.insert("error".to_owned(), Json::from(why.as_str()));
        }
        RollbackOutcome::NotAttempted { because_of } => {
            object.insert("becauseOf".to_owned(), stage_id(because_of));
        }
    }
    Json::Object(object)
}

pub(crate) fn lock(info: &LockInfo) -> Json {
    json!({
        "id": info.id,
        "user": text_or_null(&info.user),
        "host": text_or_null(&info.host),
        "pid": info.pid,
        "command": text_or_null(&info.command),
        "since": info.since,
    })
}

fn refusal(refusal: &Refusal) -> Json {
    let kind = match refusal {
        Refusal::Locked(_) => "locked",
        Refusal::State(_) => "state",
        Refusal::Resume(_) => "resume",
        Refusal::Rollback(_) => "rollback",
        Refusal::Plan(_) => "plan",
    };
    json!({
        "kind": kind,
        "message": refusal.to_string(),
        "lock": match refusal {
            Refusal::Locked(info) => lock(info),
            _ => Json::Null,
        },
    })
}

/// The saved plan a run wrote or carried out, for the run's output.
pub(crate) struct PlanAbout {
    /// The plan file.
    pub(crate) file: PathBuf,
    /// The id of the run that took the plan; empty when this run did.
    pub(crate) run: String,
    /// Whether the plan leaves an apply something to do.
    pub(crate) has_changes: bool,
}

/// The whole run: what `compile`, `plan`, `apply`, `drift` and `rollback`
/// print.
pub(crate) fn run(
    report: &RunReport,
    mode: Mode,
    out: Option<&Path>,
    plan: Option<&PlanAbout>,
) -> Map<String, Json> {
    let failed = report
        .stages
        .iter()
        .filter(|s| matches!(s.outcome, Outcome::Failed(_)))
        .count();
    let drifted = report
        .stages
        .iter()
        .filter(|s| matches!(s.outcome, Outcome::Drift(Drift::Drifted(_))))
        .count();
    let has_changes = report.stages.iter().any(stage_has_changes);
    let changes: usize = report
        .stages
        .iter()
        .map(|s| match &s.outcome {
            Outcome::Planned(plan) => plan.changes.len(),
            Outcome::Applied { changes, .. } => changes.len(),
            Outcome::Drift(Drift::Drifted(changes) | Drift::Pending { changes, .. }) => {
                changes.len()
            }
            Outcome::Failed(_) => s.done_before_failing.len(),
            _ => 0,
        })
        .sum();
    body([
        ("mode", Json::from(mode.as_str())),
        ("run", text_or_null(&report.run_id)),
        (
            "refused",
            report.refused.as_ref().map_or(Json::Null, refusal),
        ),
        (
            "stages",
            Json::Array(report.stages.iter().map(|s| stage(s, mode, out)).collect()),
        ),
        (
            "rollbacks",
            Json::Array(report.rollbacks.iter().map(rollback).collect()),
        ),
        ("warnings", json!(report.warnings)),
        (
            "plan",
            match plan {
                None => Json::Null,
                Some(about) => json!({
                    "file": about.file.display().to_string(),
                    "run": text_or_null(&about.run),
                    "hasChanges": about.has_changes,
                }),
            },
        ),
        (
            "summary",
            json!({
                "stages": report.stages.len(),
                "failed": failed,
                "changes": changes,
                "hasChanges": has_changes,
                "drifted": drifted,
                "succeeded": report.succeeded(),
                "interrupted": report.interrupted(),
            }),
        ),
    ])
}

/// A recorded run, as `status` and `runs` print it.
pub(crate) fn record(run: &RunRecord, holds_lock: bool) -> Json {
    let status = match (run.status, holds_lock) {
        (record::RunStatus::Running, false) => "interrupted",
        (status, _) => status.as_str(),
    };
    json!({
        "id": run.id,
        "mode": run.mode,
        "status": status,
        "command": text_or_null(&run.command),
        "user": text_or_null(&run.user),
        "host": text_or_null(&run.host),
        "selector": text_or_null(&run.selector),
        "started": run.started,
        "finished": text_or_null(&run.finished),
        "resumes": text_or_null(&run.resumes),
        "steps": run.steps.iter().map(|step| json!({
            "name": step.id.name,
            "kind": step.id.kind.as_str(),
            "target": step.target,
            "endpoint": text_or_null(&step.endpoint),
            "status": step.status.as_str(),
            "reason": text_or_null(&step.reason),
            "artifact": text_or_null(&step.artifact),
            "changes": step.changes,
            "previous": text_or_null(&step.previous),
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Plugins, signatures, modules
// ---------------------------------------------------------------------------

/// What checking a signature found, as a state word and its detail.
pub(crate) fn signature(signature: &Signature) -> Json {
    match signature {
        Signature::NotChecked => json!({ "state": "not-checked" }),
        Signature::Verified { key_id } => json!({ "state": "verified", "keyId": key_id }),
        Signature::Unsigned => json!({ "state": "unsigned" }),
        Signature::Invalid(why) => json!({ "state": "invalid", "reason": why }),
    }
}

/// One plugin of the catalogue, with everything `plugins --settings` shows.
pub(crate) fn plugin(name: &str, info: &meridian_plugin_host::Info, can: &[&str]) -> Json {
    json!({
        "name": name,
        "category": info.category.as_str(),
        "can": can,
        "resources": info.resource_types,
        "versions": info.target_versions,
        "imports": info.import_formats.iter().map(|format| json!({
            "name": format.name,
            "description": format.description,
            "patterns": format.patterns,
        })).collect::<Vec<_>>(),
        "sdk": info.sdk_version,
        "protocol": info.protocol_version,
        "binary": text_or_null(&info.path),
        "signature": signature(&info.signature),
        "settings": info.settings.iter().map(|setting| json!({
            "name": setting.name,
            "variable": setting.variable,
            "default": text_or_null(&setting.default),
            "description": setting.description,
        })).collect::<Vec<_>>(),
        "rules": info.rules.iter().map(|rule| json!({
            "name": rule.name,
            "meaning": rule.meaning,
            "hint": text_or_null(&rule.hint),
        })).collect::<Vec<_>>(),
        "debugLevels": info.debug_levels,
    })
}

pub(crate) fn locked_module(module: &meridian_module::Locked) -> Json {
    json!({
        "name": module.name,
        "version": text_or_null(&module.version),
        "source": module.source,
        "resolved": text_or_null(&module.resolved),
        "integrity": text_or_null(&module.integrity),
        "requires": module.requires,
    })
}

// ---------------------------------------------------------------------------
// Compile rules
// ---------------------------------------------------------------------------

pub(crate) fn violation(violation: &meridian_rules::Violation) -> Json {
    json!({
        "rule": violation.rule,
        "check": violation.check,
        "layer": violation.layer,
        "group": violation.group,
        "key": text_or_null(&violation.key),
        "message": violation.text,
    })
}
