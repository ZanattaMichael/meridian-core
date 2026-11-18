//! One stream of events, rendered as each pipeline engine shows it. The
//! expected files under `testdata/pipeline/` are recorded with
//! `UPDATE_GOLDEN=1`; a change to them is a change to what a pull request
//! shows, and is read as such.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use meridian_orchestrator::event::from_jsonl;
use meridian_report::engine::{
    problems, problems_summary, run_summary, Engine, Level, Problem, Summary,
};
use meridian_report::Report;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/pipeline")
}

fn golden(name: &str, actual: &str) {
    let path = testdata().join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; record it with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(actual, expected, "{} differs from what the code renders; run with UPDATE_GOLDEN=1 to record it again, and read the diff", path.display());
}

/// The report of the recorded run, and where its documents are.
fn report() -> Report {
    let text = fs::read_to_string(testdata().join("events.jsonl")).unwrap();
    let events = from_jsonl(&text);
    assert_eq!(events.len(), 13);
    Report::from_events(&events)
}

fn positions(step: &str) -> Option<(String, usize)> {
    match step {
        "network" => Some(("site.yaml".to_owned(), 1)),
        "web" => Some(("site.yaml".to_owned(), 24)),
        _ => None,
    }
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn lines(engine: Engine, problems: &[Problem]) -> String {
    let mut out = String::new();
    for problem in problems {
        if let Some(line) = engine.annotation(problem) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn a_failed_apply_is_one_problem_per_failed_step_at_its_document() {
    let found = problems(&report(), &positions);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].level, Level::Error);
    assert_eq!(found[0].title, "ResourceSet web failed");
    assert_eq!((found[0].file.as_str(), found[0].line), ("site.yaml", 24));
    // The reason is the record's fixed word; the tool's own words are never
    // in an event, so they cannot be here.
    assert!(
        found[0].message.contains("tool-failed"),
        "{}",
        found[0].message
    );
}

#[test]
fn github_annotations_and_step_summary() {
    let report = report();
    golden(
        "github-annotations.txt",
        &lines(Engine::GitHubActions, &problems(&report, &positions)),
    );
    let summary = run_summary(&report, ".meridian (local)");
    match Engine::GitHubActions.summary(&summary, &env(&[("GITHUB_STEP_SUMMARY", "s.md")])) {
        Some(Summary::AppendToFile { content, .. }) => golden("github-summary.md", &content),
        other => panic!("{other:?}"),
    }
}

#[test]
fn azure_logissue_lines_and_summary() {
    let report = report();
    golden(
        "azure-annotations.txt",
        &lines(Engine::AzurePipelines, &problems(&report, &positions)),
    );
    let summary = run_summary(&report, ".meridian (local)");
    match Engine::AzurePipelines.summary(&summary, &env(&[("AGENT_TEMPDIRECTORY", "/agent/_temp")]))
    {
        Some(Summary::FileThenLine {
            content,
            line,
            path,
        }) => {
            assert_eq!(path, PathBuf::from("/agent/_temp/meridian-summary.md"));
            assert_eq!(
                line,
                "##vso[task.uploadsummary]/agent/_temp/meridian-summary.md"
            );
            golden("azure-summary.md", &content);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn buildkite_annotation_markdown() {
    let report = report();
    assert_eq!(lines(Engine::Buildkite, &problems(&report, &positions)), "");
    let summary = run_summary(&report, ".meridian (local)");
    match Engine::Buildkite.summary(&summary, &env(&[])) {
        Some(Summary::Agent {
            program,
            args,
            stdin,
        }) => {
            assert_eq!(program, "buildkite-agent");
            assert_eq!(
                args,
                [
                    "annotate",
                    "--context",
                    "meridian",
                    "--style",
                    "error",
                    "--append"
                ]
            );
            golden("buildkite-annotation.md", &stdin);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refused_validate_is_its_problems_and_nothing_else() {
    let problems = [
        Problem {
            level: Level::Error,
            title: "validate".to_owned(),
            message: "spec.resources[0]: unknown field \"colour\"".to_owned(),
            file: "site.yaml".to_owned(),
            line: 34,
            column: 7,
        },
        Problem::plain(
            Level::Error,
            "plan",
            "no document's labels match the selector tier=none",
        ),
    ];
    golden(
        "validate-github-annotations.txt",
        &lines(Engine::GitHubActions, &problems),
    );
    golden(
        "validate-summary.md",
        &problems_summary("validate", &problems),
    );
}

#[test]
fn no_value_of_the_run_is_in_what_an_engine_is_shown() {
    // The stream names a sensitive output; its value was never in an event,
    // and the summary says so rather than showing anything.
    let report = report();
    let summary = run_summary(&report, ".meridian (local)");
    assert!(summary.contains("db_password"));
    assert!(summary.contains("(sensitive)"));
    assert!(summary.contains("Values are never in a report"));
}
