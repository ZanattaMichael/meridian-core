//! `meridian test`: runs Test documents and reports each case.

use crate::*;

use meridian_test::{CaseResult, FileResult, Runner, Status};

pub(crate) fn test(
    files: &[PathBuf],
    case: Option<&str>,
    junit: Option<&Path>,
    plugins: &PluginDirs,
    state_dir: &Path,
    keep: bool,
) -> Result<(), Failure> {
    let files = if files.is_empty() {
        let found = meridian_test::discover(Path::new("."));
        if found.is_empty() {
            return Err(Failure::Message(format!(
                "no test files: name them, or put *{} files under tests/",
                meridian_test::TEST_FILE_SUFFIX
            )));
        }
        found
    } else {
        files.to_vec()
    };
    // Every test file is read before anything runs, so a mistake in the
    // last one is reported before the first one's cases take their time.
    let mut tests = Vec::new();
    for file in &files {
        tests.extend(meridian_test::load(file).map_err(Failure::Message)?);
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
    let mut results: Vec<FileResult> = Vec::new();
    for test in &tests {
        let result = meridian_test::run_file(test, &runner, case);
        if !json_output() {
            print_file(&result);
        }
        results.push(result);
    }
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    let cases: Vec<&CaseResult> = results.iter().flat_map(|f| &f.cases).collect();
    let passed = cases.iter().filter(|c| c.status == Status::Passed).count();
    let failed = cases.iter().filter(|c| c.status == Status::Failed).count();
    let errors = cases.iter().filter(|c| c.status == Status::Error).count();
    let ok = failed == 0 && errors == 0 && !cases.is_empty();
    let mut junit_problem = None;
    if let Some(path) = junit {
        match std::fs::write(path, meridian_test::junit(&results)) {
            Ok(()) => eprintln!("report: wrote junit to {}", path.display()),
            Err(e) => junit_problem = Some(format!("cannot write {}: {e}", path.display())),
        }
    }
    if json_output() {
        emit(
            ok && junit_problem.is_none(),
            body([
                (
                    "files",
                    results.iter().map(file_json).collect::<Vec<_>>().into(),
                ),
                (
                    "summary",
                    serde_json::json!({
                        "files": results.len(),
                        "cases": cases.len(),
                        "passed": passed,
                        "failed": failed,
                        "errors": errors,
                    }),
                ),
            ]),
        );
    } else {
        let mut line = format!("{}: {passed} passed", count(cases.len(), "case"));
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
    if cases.is_empty() {
        return Err(Failure::Message(match case {
            Some(text) => format!("no case's name holds {text:?}"),
            None => "no cases".to_owned(),
        }));
    }
    if ok {
        Ok(())
    } else {
        Err(Failure::Reported)
    }
}

fn print_file(result: &FileResult) {
    outln!("{} ({})", result.path.display(), result.name);
    for case in &result.cases {
        print_case(&case.name, case);
    }
}

/// One case, or one lab cell, under its name: the status word, the level
/// and the documents, then a lifecycle case's phases, then each failure.
pub(crate) fn print_case(name: &str, case: &CaseResult) {
    let word = match case.status {
        Status::Passed => "ok  ",
        Status::Failed => "FAIL",
        Status::Error => "ERR ",
        Status::Skipped => "skip",
    };
    outln!(
        "  {word}  {name} ({}, {})",
        case.run.as_str(),
        count(case.stages.len(), "document")
    );
    for phase in &case.phases {
        let word = match phase.status {
            Status::Passed => "ok",
            Status::Skipped => "skipped",
            _ => "FAIL",
        };
        let why = match phase.status {
            Status::Skipped => format!(": {}", phase.failures.join("; ")),
            _ => String::new(),
        };
        outln!("        {}: {word}{why}", phase.name);
    }
    for failure in &case.failures {
        let mut lines = failure.lines();
        if let Some(first) = lines.next() {
            outln!("        - {first}");
        }
        for more in lines {
            outln!("          {more}");
        }
    }
}

fn file_json(result: &FileResult) -> serde_json::Value {
    serde_json::json!({
        "file": result.path.display().to_string(),
        "name": result.name,
        "ok": result.passed(),
        "cases": result.cases.iter().map(case_json).collect::<Vec<_>>(),
    })
}

/// One case as JSON, as `meridian test` and `meridian lab run` give it.
pub(crate) fn case_json(c: &CaseResult) -> serde_json::Value {
    serde_json::json!({
        "name": c.name,
        "run": c.run.as_str(),
        "status": c.status.as_str(),
        "runId": c.run_id,
        "failures": c.failures,
        "documents": c.stages.iter().map(|s| serde_json::json!({
            "name": s.name,
            "target": s.target,
            "outcome": s.outcome,
        })).collect::<Vec<_>>(),
        "phases": c.phases.iter().map(|p| serde_json::json!({
            "name": p.name,
            "status": p.status.as_str(),
            "failures": p.failures,
            "millis": p.millis as u64,
        })).collect::<Vec<_>>(),
        "millis": c.millis as u64,
    })
}
