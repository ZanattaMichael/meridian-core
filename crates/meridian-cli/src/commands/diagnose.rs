//! `meridian diagnose`: what broke in a run, where, and what to run next.
//! It reads the record, the events and the failures this machine kept; it
//! runs no tool, loads no target plugin and takes no lock.

use crate::*;

use meridian_orchestrator::diagnose::{self, Diagnosis, Entry, Filter, ResourceFailure};
use meridian_orchestrator::logs;
use serde_json::{json, Value as Json};

/// How many failures of one machine are shown before the rest are counted.
const SHOWN: usize = 5;

/// What `meridian diagnose` was asked for.
pub(crate) struct DiagnoseQuery<'a> {
    pub(crate) run: Option<&'a str>,
    pub(crate) filter: Filter<'a>,
}

pub(crate) fn diagnose(state: &State, query: &DiagnoseQuery<'_>) -> Result<(), Failure> {
    let backend = backend(state)?;
    let backend = backend.as_ref();
    let run = match query.run {
        Some(id) => record::load_run(backend, id)
            .map_err(Failure::Message)?
            .ok_or_else(|| {
                Failure::Message(format!("there is no run {id} in {}", backend.describe()))
            })?,
        None => {
            let mut found = None;
            for id in record::list_runs(backend)
                .map_err(Failure::Message)?
                .iter()
                .rev()
            {
                if let Some(run) = record::load_run(backend, id).map_err(Failure::Message)? {
                    if diagnose::has_failed(&run) {
                        found = Some(run);
                        break;
                    }
                }
            }
            match found {
                Some(run) => run,
                None => {
                    if json_output() {
                        emit(true, body([("run", Json::Null), ("failed", json!([]))]));
                    } else {
                        outln!("no failed run is recorded in {}", backend.describe());
                    }
                    return Ok(());
                }
            }
        }
    };
    let events = record::load_events(backend, &run.id).map_err(Failure::Message)?;
    let kept = logs::read_failures(&state.state_dir, &run.id);
    let found = diagnose::diagnose(&run, &events, &kept, query.filter);
    if json_output() {
        emit(true, diagnosis_json(&found));
    } else {
        print_diagnosis(&found);
    }
    Ok(())
}

fn print_diagnosis(d: &Diagnosis) {
    let run = &d.run;
    let failed = run
        .steps
        .iter()
        .filter(|s| s.status == record::StepStatus::Failed)
        .count();
    let mut head = format!("run {}: {}, {}", run.id, run.mode, run.status.as_str());
    if failed > 0 {
        head.push_str(&format!(" ({failed} of {} steps failed", run.steps.len()));
        if !d.skipped.is_empty() {
            head.push_str(&format!("; {} not run because of it", d.skipped.len()));
        }
        head.push(')');
    }
    outln!("{head}");
    if !run.resumes.is_empty() {
        outln!("resumes {}", run.resumes);
    }
    if d.failed.is_empty() {
        outln!("nothing failed that matches");
    }
    for step in &d.failed {
        for entry in &step.entries {
            outln!("");
            print_entry(&step.step, entry);
        }
    }
    if !d.skipped.is_empty() {
        outln!("");
        let mut by_cause: Vec<(&str, Vec<&str>)> = Vec::new();
        for s in &d.skipped {
            match by_cause.iter_mut().find(|(c, _)| *c == s.because_of) {
                Some((_, names)) => names.push(&s.step.id.name),
                None => by_cause.push((&s.because_of, vec![&s.step.id.name])),
            }
        }
        for (because, names) in by_cause {
            let why = if because.is_empty() {
                "earlier-step-failed".to_owned()
            } else {
                format!("earlier-step-failed: {because}")
            };
            outln!("not run: {} ({why})", names.join(", "));
        }
    }
    if !d.failed.is_empty() && !d.kept_here {
        outln!("");
        outln!(
            "the tools' own words were kept on the machine that ran {}",
            run.id
        );
    }
}

fn print_entry(step: &record::StepRecord, entry: &Entry) {
    let place = match entry.endpoints.as_slice() {
        [] => "-".to_owned(),
        [one] => one.clone(),
        many => format!("{} machines", many.len()),
    };
    outln!(
        "{}  {}  {place}  {}",
        step.id.name,
        step.target,
        entry.cause
    );
    if entry.endpoints.len() > 1 {
        let shown: Vec<&str> = entry
            .endpoints
            .iter()
            .take(SHOWN)
            .map(String::as_str)
            .collect();
        let mut line = shown.join(", ");
        if entry.endpoints.len() > SHOWN {
            line.push_str(&format!(" and {} more", entry.endpoints.len() - SHOWN));
        }
        outln!("  {line}");
    }
    for f in entry.failures.iter().take(SHOWN) {
        let line = [f.resource.as_str(), f.summary.as_str(), f.at.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  ");
        if !line.is_empty() {
            outln!("  {line}");
        }
    }
    if entry.failures.len() > SHOWN {
        outln!("  and {} more", entry.failures.len() - SHOWN);
    }
    for next in &entry.next {
        outln!("  next: {next}");
    }
}

fn text_or_null(text: &str) -> Json {
    if text.is_empty() {
        Json::Null
    } else {
        Json::from(text)
    }
}

fn failure_json(f: &ResourceFailure) -> Json {
    json!({
        "endpoint": text_or_null(&f.endpoint),
        "resource": text_or_null(&f.resource),
        "cause": f.cause,
        "toolCode": text_or_null(&f.tool_code),
        "summary": text_or_null(&f.summary),
        "at": text_or_null(&f.at),
    })
}

fn diagnosis_json(d: &Diagnosis) -> serde_json::Map<String, Json> {
    let run = &d.run;
    let failed: Vec<Json> = d
        .failed
        .iter()
        .map(|s| {
            json!({
                "name": s.step.id.name,
                "kind": s.step.id.kind.as_str(),
                "target": s.step.target,
                "endpoint": text_or_null(&s.step.endpoint),
                "reason": text_or_null(&s.step.reason),
                "entries": s.entries.iter().map(|e| json!({
                    "cause": e.cause,
                    "endpoints": e.endpoints,
                    "failures": e.failures.iter().map(failure_json).collect::<Vec<_>>(),
                    "next": e.next,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let skipped: Vec<Json> = d
        .skipped
        .iter()
        .map(|s| {
            json!({
                "name": s.step.id.name,
                "kind": s.step.id.kind.as_str(),
                "becauseOf": text_or_null(&s.because_of),
            })
        })
        .collect();
    body([
        (
            "run",
            json!({
                "id": run.id,
                "mode": run.mode,
                "status": run.status.as_str(),
                "resumes": text_or_null(&run.resumes),
                "steps": run.steps.len(),
            }),
        ),
        ("failed", failed.into()),
        ("skipped", skipped.into()),
        ("keptHere", d.kept_here.into()),
    ])
}
