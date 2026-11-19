//! `meridian lab`: runs a Lab document's cells, lists the last run, and
//! sweeps what expired runs left behind.

use crate::*;

use meridian_test::{CellResult, FileResult, LabFile, Runner, Status, SweepOutcome, TestFile};

use super::testing::{case_json, print_case};

pub(crate) fn lab(command: LabCommand) -> Result<(), Failure> {
    match command {
        LabCommand::Run {
            file,
            filter,
            junit,
            plugins,
            state_dir,
            keep,
        } => run(&file, &filter, junit.as_deref(), &plugins, &state_dir, keep),
        LabCommand::List { file, state_dir } => list(&file, &state_dir),
        LabCommand::Sweep {
            older_than,
            plugins,
            state_dir,
        } => sweep(older_than, &plugins, &state_dir),
    }
}

/// The lab and every Test file it names, read before anything runs.
fn read(file: &Path, state_dir: &Path) -> Result<(LabFile, Vec<TestFile>), Failure> {
    let lab = meridian_test::load_lab(file).map_err(Failure::Message)?;
    let mut tests = Vec::new();
    for path in in_modules(&lab.environments, state_dir)? {
        tests.extend(meridian_test::load(&path).map_err(Failure::Message)?);
    }
    Ok((lab, tests))
}

fn run(
    file: &Path,
    filter: &[String],
    junit: Option<&Path>,
    plugins: &PluginDirs,
    state_dir: &Path,
    keep: bool,
) -> Result<(), Failure> {
    let filters = filter
        .iter()
        .map(|text| {
            text.split_once('=')
                .map(|(a, v)| (a.trim().to_owned(), v.trim().to_owned()))
                .ok_or_else(|| {
                    Failure::Message(format!(
                        "--filter {text}: expected AXIS=VALUE, as in --filter target=terraform"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (lab, tests) = read(file, state_dir)?;
    let cells = meridian_test::cells(&lab, &tests, &filters).map_err(Failure::Message)?;
    if cells.is_empty() {
        return Err(Failure::Message(
            "no cells: the Test files hold no lifecycle case, or every combination is excluded \
             or filtered out"
                .to_owned(),
        ));
    }
    let registry = registry(plugins)?;
    let resolve = |paths: &[PathBuf]| -> Result<Vec<PathBuf>, String> {
        in_modules(paths, state_dir).map_err(|e| match e {
            Failure::Message(text) => text,
            _ => "a module could not be resolved".to_owned(),
        })
    };
    let runner = Runner {
        registry: &registry,
        state_dir: state_dir.to_path_buf(),
        resolve: &resolve,
        keep,
    };
    let run = meridian_test::new_lab_run_id(&lab);
    eprintln!(
        "lab {}: run {run}, {} at a time",
        lab.name,
        count(cells.len(), "cell"),
    );
    let results = meridian_test::run_lab(&lab, &tests, &cells, &runner, &run);
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    let passed = results
        .iter()
        .filter(|r| r.result.status == Status::Passed)
        .count();
    let failed = results
        .iter()
        .filter(|r| r.result.status == Status::Failed)
        .count();
    let errors = results
        .iter()
        .filter(|r| r.result.status == Status::Error)
        .count();
    let ok = failed == 0 && errors == 0;
    remember(state_dir, &lab, &run, &results);
    let mut junit_problem = None;
    if let Some(path) = junit {
        let suites: Vec<FileResult> = results
            .iter()
            .map(|r| FileResult {
                path: lab.path.clone(),
                name: r.cell.name.clone(),
                cases: vec![r.result.clone()],
            })
            .collect();
        match std::fs::write(path, meridian_test::junit(&suites)) {
            Ok(()) => eprintln!("report: wrote junit to {}", path.display()),
            Err(e) => junit_problem = Some(format!("cannot write {}: {e}", path.display())),
        }
    }
    if json_output() {
        emit(
            ok && junit_problem.is_none(),
            body([
                ("lab", lab.name.clone().into()),
                ("run", run.clone().into()),
                (
                    "cells",
                    results.iter().map(cell_json).collect::<Vec<_>>().into(),
                ),
                (
                    "summary",
                    serde_json::json!({
                        "cells": results.len(),
                        "passed": passed,
                        "failed": failed,
                        "errors": errors,
                    }),
                ),
            ]),
        );
    } else {
        outln!("{} ({}, run {run})", lab.path.display(), lab.name);
        for r in &results {
            print_case(&r.cell.name, &r.result);
        }
        let mut line = format!("{}: {passed} passed", count(results.len(), "cell"));
        if failed > 0 {
            line.push_str(&format!(", {failed} failed"));
        }
        if errors > 0 {
            line.push_str(&format!(", {errors} could not run"));
        }
        outln!("{line}");
    }
    if let Some(problem) = junit_problem {
        return Err(Failure::Message(problem));
    }
    if ok {
        Ok(())
    } else {
        Err(Failure::Reported)
    }
}

fn cell_json(r: &CellResult) -> serde_json::Value {
    let mut json = case_json(&r.result);
    if let Some(map) = json.as_object_mut() {
        map.insert("cell".to_owned(), r.cell.number.into());
        map.insert("name".to_owned(), r.cell.name.clone().into());
        map.insert(
            "values".to_owned(),
            serde_json::to_value(&r.cell.values).unwrap_or_default(),
        );
    }
    json
}

/// Keeps how each cell of this run ended, for `meridian lab list`. A
/// problem writing it is a warning: the run itself is reported.
fn remember(state_dir: &Path, lab: &LabFile, run: &str, results: &[CellResult]) {
    let dir = state_dir.join("lab").join(&lab.name);
    let record = serde_json::json!({
        "run": run,
        "cells": results.iter().map(|r| serde_json::json!({
            "name": r.cell.name,
            "status": r.result.status.as_str(),
            "runId": r.result.run_id,
        })).collect::<Vec<_>>(),
    });
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::write(
            dir.join(meridian_test::LAST_RUN_FILE),
            format!("{record:#}\n"),
        )
    });
    if let Err(e) = written {
        eprintln!(
            "warning: cannot keep this run's results in {}: {e}",
            dir.display()
        );
    }
}

fn list(file: &Path, state_dir: &Path) -> Result<(), Failure> {
    let (lab, tests) = read(file, state_dir)?;
    let cells = meridian_test::cells(&lab, &tests, &[]).map_err(Failure::Message)?;
    let last: Option<serde_json::Value> = std::fs::read(
        state_dir
            .join("lab")
            .join(&lab.name)
            .join(meridian_test::LAST_RUN_FILE),
    )
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let run = last
        .as_ref()
        .and_then(|l| l["run"].as_str())
        .map(str::to_owned);
    let status_of = |name: &str| -> String {
        last.as_ref()
            .and_then(|l| l["cells"].as_array())
            .and_then(|cells| cells.iter().find(|c| c["name"] == name))
            .and_then(|c| c["status"].as_str())
            .unwrap_or("not-run")
            .to_owned()
    };
    if json_output() {
        emit(
            true,
            body([
                ("lab", lab.name.clone().into()),
                ("lastRun", run.into()),
                (
                    "cells",
                    cells
                        .iter()
                        .map(|c| {
                            serde_json::json!({
                                "cell": c.number,
                                "name": c.name,
                                "values": c.values,
                                "last": status_of(&c.name),
                            })
                        })
                        .collect::<Vec<_>>()
                        .into(),
                ),
            ]),
        );
    } else {
        match &run {
            Some(run) => outln!("{} ({}, last run {run})", lab.path.display(), lab.name),
            None => outln!("{} ({}, never run)", lab.path.display(), lab.name),
        }
        for c in &cells {
            outln!("  {:<8}  {}", status_of(&c.name), c.name);
        }
        outln!("{}", count(cells.len(), "cell"));
    }
    Ok(())
}

fn sweep(
    older_than: Option<Duration>,
    plugins: &PluginDirs,
    state_dir: &Path,
) -> Result<(), Failure> {
    let registry = registry(plugins)?;
    let swept = meridian_test::sweep(state_dir, &registry, older_than);
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    let destroyed = swept
        .iter()
        .filter(|s| s.outcome == SweepOutcome::Destroyed)
        .count();
    let failed = swept
        .iter()
        .filter(|s| matches!(s.outcome, SweepOutcome::Failed(_)))
        .count();
    let left = swept.len() - destroyed - failed;
    if json_output() {
        emit(
            failed == 0,
            body([
                (
                    "environments",
                    swept
                        .iter()
                        .map(|s| {
                            let (outcome, error) = match &s.outcome {
                                SweepOutcome::Destroyed => ("destroyed", None),
                                SweepOutcome::NotDue => ("not-due", None),
                                SweepOutcome::Failed(why) => ("failed", Some(why.clone())),
                            };
                            serde_json::json!({
                                "dir": s.dir.display().to_string(),
                                "name": s.name,
                                "runId": s.run_id,
                                "outcome": outcome,
                                "error": error,
                            })
                        })
                        .collect::<Vec<_>>()
                        .into(),
                ),
                (
                    "summary",
                    serde_json::json!({
                        "destroyed": destroyed,
                        "failed": failed,
                        "notDue": left,
                    }),
                ),
            ]),
        );
    } else {
        for s in &swept {
            let what = if s.name.is_empty() {
                s.dir.display().to_string()
            } else {
                format!("{} ({}, {})", s.name, s.run_id, s.dir.display())
            };
            match &s.outcome {
                SweepOutcome::Destroyed => outln!("destroyed  {what}"),
                SweepOutcome::NotDue => outln!("not due    {what}"),
                SweepOutcome::Failed(why) => {
                    outln!("FAILED     {what}");
                    outln!("           - {why}");
                }
            }
        }
        outln!(
            "{}: {destroyed} destroyed, {failed} failed, {left} not due",
            count(swept.len(), "environment")
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(Failure::Reported)
    }
}
