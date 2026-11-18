//! Running inside a pipeline engine: GitHub Actions, Azure Pipelines,
//! GitLab CI, Buildkite, Jenkins, Argo Workflows or Tekton.
//!
//! When the environment says the command runs in one, and standard output
//! is not a terminal, every problem the command prints is also written in
//! the engine's own annotation syntax, where it has one, so that a refused
//! document shows on the pull request at its line; and the run's report is
//! handed to the engine as the job's summary, where it shows one. What is
//! written is decided in `meridian_report::engine`, from a document's
//! diagnostics and from the report folded from the run's events, and never
//! from a second reading of anything; this module only detects the engine
//! and does the writing. Output is never coloured, in an engine or out of
//! it, and nothing is masked, because no value is ever printed.
//!
//! `--annotations off` (`MERIDIAN_ANNOTATIONS=off`) turns all of it off;
//! `on` keeps it on when standard output is a terminal, which Buildkite's
//! agent gives a job by default.

use super::*;

use std::io::IsTerminal;

use meridian_report::engine::{problems_summary, Summary};
pub(crate) use meridian_report::engine::{Engine, Level, Problem};

/// Whether to write for the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum AnnotationMode {
    /// When a pipeline engine is detected and standard output is not a
    /// terminal
    #[default]
    Auto,
    /// When a pipeline engine is detected, terminal or not
    On,
    /// Never
    Off,
}

static ENGINE: OnceLock<Option<Engine>> = OnceLock::new();

/// Settles, once, which engine the command writes for: none, usually.
pub(crate) fn settle_pipeline(mode: AnnotationMode) {
    let env = meridian_secret::ambient_environment();
    let engine = choose(mode, Engine::detect(&env), std::io::stdout().is_terminal());
    let _ = ENGINE.set(engine);
}

/// Which engine `mode` chooses, given what the environment says and whether
/// a person is at the keyboard.
pub(crate) fn choose(
    mode: AnnotationMode,
    detected: Option<Engine>,
    terminal: bool,
) -> Option<Engine> {
    match mode {
        AnnotationMode::Off => None,
        AnnotationMode::On => detected,
        AnnotationMode::Auto if terminal => None,
        AnnotationMode::Auto => detected,
    }
}

/// The engine the command writes for, if any.
pub(crate) fn engine() -> Option<Engine> {
    ENGINE.get().copied().flatten()
}

/// Writes each problem as the engine's annotation, on standard error, which
/// the engines read their commands from as they read standard output, and
/// which keeps standard output the command's result.
pub(crate) fn annotate(problems: &[Problem]) {
    let Some(engine) = engine() else {
        return;
    };
    for problem in problems {
        if let Some(line) = engine.annotation(problem) {
            eprintln!("{line}");
        }
    }
}

/// Hands a Markdown summary to the engine's job page, where it has one.
pub(crate) fn summarise(markdown: &str) {
    let Some(engine) = engine() else {
        return;
    };
    let env = meridian_secret::ambient_environment();
    let Some(summary) = engine.summary(markdown, &env) else {
        return;
    };
    if let Err(why) = deliver(summary) {
        eprintln!("warning: the job summary was not written: {why}");
    }
}

fn deliver(summary: Summary) -> Result<(), String> {
    match summary {
        Summary::AppendToFile { path, content } => {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            file.write_all(content.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|e| format!("{}: {e}", path.display()))
        }
        Summary::FileThenLine {
            path,
            content,
            line,
        } => {
            std::fs::write(&path, content).map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("{line}");
            Ok(())
        }
        Summary::Agent {
            program,
            args,
            stdin,
        } => {
            // Only if the agent is there: a job that runs Meridian in a
            // container without it gets its log and nothing more.
            if !on_path(&program) {
                return Ok(());
            }
            let mut child = std::process::Command::new(&program)
                .args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("{program}: {e}"))?;
            if let Some(mut pipe) = child.stdin.take() {
                let _ = pipe.write_all(stdin.as_bytes());
            }
            let status = child.wait().map_err(|e| format!("{program}: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("{program} exited with {status}"))
            }
        }
    }
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file() || (cfg!(windows) && dir.join(format!("{program}.exe")).is_file())
    })
}

/// A command that failed before it had a run: its problems are annotated,
/// and they are the whole of its summary.
pub(crate) fn report_problems(problems: &[Problem]) {
    if engine().is_none() || problems.is_empty() {
        return;
    }
    annotate(problems);
    summarise(&problems_summary(command_name(), problems));
}

/// One problem that is nowhere in particular: a refusal, a missing file.
pub(crate) fn report_refusal(message: &str) {
    report_problems(&[Problem::plain(Level::Error, command_name(), message)]);
}

/// A document's diagnostics, each at its line.
pub(crate) fn document_problems(errors: &meridian_ir::ErrorList) -> Vec<Problem> {
    errors
        .iter()
        .map(|error| Problem {
            level: Level::Error,
            title: command_name().to_owned(),
            message: if error.path.is_empty() {
                error.msg.clone()
            } else {
                format!("{}: {}", error.path, error.msg)
            },
            file: error.pos.file.clone(),
            line: error.pos.line,
            column: error.pos.column,
        })
        .collect()
}

/// Where each document of the bundle starts, for a step's problem to point
/// at.
pub(crate) fn positions(bundle: &Bundle) -> impl Fn(&str) -> Option<(String, usize)> + '_ {
    move |step: &str| {
        bundle
            .documents
            .iter()
            .find(|doc| doc.name() == step)
            .map(|doc| (doc.position().file.clone(), doc.position().line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_writes_for_an_engine_only_when_no_person_is_watching() {
        let github = Some(Engine::GitHubActions);
        assert_eq!(choose(AnnotationMode::Auto, github, false), github);
        assert_eq!(choose(AnnotationMode::Auto, github, true), None);
        assert_eq!(choose(AnnotationMode::Auto, None, false), None);
        assert_eq!(choose(AnnotationMode::On, github, true), github);
        assert_eq!(choose(AnnotationMode::On, None, false), None);
        assert_eq!(choose(AnnotationMode::Off, github, false), None);
    }
}
