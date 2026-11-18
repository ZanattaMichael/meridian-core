//! A run as a pipeline engine shows it: annotations at the line of a
//! document, and a job summary.
//!
//! Every engine on the list sets one well-known environment variable in the
//! jobs it runs. When `meridian` finds one, a problem is also written in the
//! engine's own annotation syntax, where it has one, and the run's report is
//! handed to the engine as the job's summary, where it takes one. Nothing
//! here is a second place that formats an error: a [`Problem`] is made from
//! a document's diagnostic or from a step of the [`Report`], which is folded
//! from events, and the summary is the Markdown report as `--report
//! markdown=FILE` writes it. An event never holds a value, so neither does
//! an annotation.
//!
//! This module decides what to write and where. Writing it (appending to a
//! file, printing a line, running the engine's agent) is the command's,
//! which also decides whether to do any of this at all: an engine that is
//! detected while standard output is a terminal is a person at a keyboard
//! inside a job, who wants the plain text.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

use crate::{Report, Step};

/// A pipeline engine `meridian` can tell it is running inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    GitHubActions,
    AzurePipelines,
    GitLabCi,
    Buildkite,
    Jenkins,
    ArgoWorkflows,
    Tekton,
}

impl Engine {
    /// Every engine, in the order they are looked for.
    pub const ALL: [Engine; 7] = [
        Engine::GitHubActions,
        Engine::AzurePipelines,
        Engine::GitLabCi,
        Engine::Buildkite,
        Engine::Jenkins,
        Engine::ArgoWorkflows,
        Engine::Tekton,
    ];

    /// The engine's name as `meridian` prints it.
    pub fn name(self) -> &'static str {
        match self {
            Engine::GitHubActions => "github-actions",
            Engine::AzurePipelines => "azure-pipelines",
            Engine::GitLabCi => "gitlab-ci",
            Engine::Buildkite => "buildkite",
            Engine::Jenkins => "jenkins",
            Engine::ArgoWorkflows => "argo-workflows",
            Engine::Tekton => "tekton",
        }
    }

    /// The environment variable that says a job runs inside the engine. For
    /// Argo Workflows and Tekton it is a prefix: any variable starting with
    /// it.
    pub fn variable(self) -> &'static str {
        match self {
            Engine::GitHubActions => "GITHUB_ACTIONS",
            Engine::AzurePipelines => "TF_BUILD",
            Engine::GitLabCi => "GITLAB_CI",
            Engine::Buildkite => "BUILDKITE",
            Engine::Jenkins => "JENKINS_URL",
            Engine::ArgoWorkflows => "ARGO_",
            Engine::Tekton => "TEKTON_",
        }
    }

    /// The engine the environment says the process runs inside, if any.
    /// `env` is the process's environment, or a stand-in for a test. A
    /// variable that is set to nothing does not count.
    pub fn detect(env: &BTreeMap<String, String>) -> Option<Engine> {
        Engine::ALL.into_iter().find(|engine| {
            let variable = engine.variable();
            if let Some(prefix) = variable.strip_suffix('_') {
                let prefix = format!("{prefix}_");
                env.iter()
                    .any(|(name, value)| name.starts_with(&prefix) && !value.is_empty())
            } else {
                env.get(variable).is_some_and(|value| !value.is_empty())
            }
        })
    }

    /// Whether the engine has a syntax for an annotation on a line of a
    /// file: a problem it shows beside the code, not only in the log.
    pub fn annotates(self) -> bool {
        matches!(
            self,
            Engine::GitHubActions | Engine::AzurePipelines | Engine::Buildkite
        )
    }

    /// Whether the engine shows a summary of the job beside its log.
    pub fn summarises(self) -> bool {
        matches!(
            self,
            Engine::GitHubActions | Engine::AzurePipelines | Engine::Buildkite
        )
    }

    /// A problem in the engine's own annotation syntax, as one line for the
    /// job's log, or `None` where the engine has no such syntax. Buildkite
    /// has none in the log: its annotations go through its agent, as part of
    /// the [`summary`](Engine::summary).
    pub fn annotation(self, problem: &Problem) -> Option<String> {
        match self {
            Engine::GitHubActions => Some(github_annotation(problem)),
            Engine::AzurePipelines => Some(azure_annotation(problem)),
            _ => None,
        }
    }

    /// How a Markdown summary reaches the engine's job page, or `None` where
    /// the engine has no such page. `env` is the process's environment.
    pub fn summary(self, markdown: &str, env: &BTreeMap<String, String>) -> Option<Summary> {
        match self {
            Engine::GitHubActions => {
                let path = env.get("GITHUB_STEP_SUMMARY").filter(|p| !p.is_empty())?;
                Some(Summary::AppendToFile {
                    path: PathBuf::from(path),
                    content: markdown.to_owned(),
                })
            }
            Engine::AzurePipelines => {
                let dir = env
                    .get("AGENT_TEMPDIRECTORY")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir);
                let path = dir.join("meridian-summary.md");
                Some(Summary::FileThenLine {
                    content: markdown.to_owned(),
                    line: format!(
                        "##vso[task.uploadsummary]{}",
                        azure_escape(&path.display().to_string())
                    ),
                    path,
                })
            }
            Engine::Buildkite => Some(Summary::Agent {
                program: "buildkite-agent".to_owned(),
                args: vec![
                    "annotate".to_owned(),
                    "--context".to_owned(),
                    "meridian".to_owned(),
                    "--style".to_owned(),
                    buildkite_style(markdown).to_owned(),
                    "--append".to_owned(),
                ],
                stdin: markdown.to_owned(),
            }),
            _ => None,
        }
    }
}

/// How serious a problem is, in the words the engines share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
        }
    }
}

/// One thing to show the person who opened the job: what, and where in
/// which document, when that is known. The message is the one already
/// printed; it names things and never holds a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub level: Level,
    /// A few words for the annotation's title, such as `validate` or `step
    /// "web" failed`.
    pub title: String,
    pub message: String,
    /// The document the problem is in, as the command was given it; empty
    /// when the problem is not in a document.
    pub file: String,
    /// The line and column, counted from 1; 0 when not known.
    pub line: usize,
    pub column: usize,
}

impl Problem {
    /// A problem that is nowhere in particular: a refusal of the whole run.
    pub fn plain(level: Level, title: &str, message: &str) -> Problem {
        Problem {
            level,
            title: title.to_owned(),
            message: message.to_owned(),
            file: String::new(),
            line: 0,
            column: 0,
        }
    }

    /// The problem as a line of Markdown, for a summary.
    pub fn markdown(&self) -> String {
        let mut line = format!("- **{}**", md(self.title.as_str()));
        if !self.file.is_empty() {
            let _ = write!(line, " `{}", self.file.replace('`', ""));
            if self.line > 0 {
                let _ = write!(line, ":{}", self.line);
                if self.column > 0 {
                    let _ = write!(line, ":{}", self.column);
                }
            }
            line.push('`');
        }
        let _ = write!(line, ": {}", md(&self.message));
        line
    }
}

/// Where a step is in its document, for a problem to point at: the file and
/// the line the document starts on, as the command was given them.
pub type Positions<'a> = dyn Fn(&str) -> Option<(String, usize)> + 'a;

/// Every problem a report holds, one per step that failed, could not be
/// put back, or drifted, in the order the steps ran. Each points at the
/// step's document when `positions` knows where it is.
pub fn problems(report: &Report, positions: &Positions<'_>) -> Vec<Problem> {
    report
        .steps
        .iter()
        .filter(|step| step.is_problem())
        .map(|step| {
            let (level, what) = step_problem(step);
            let (file, line) = positions(&step.name).unwrap_or_default();
            Problem {
                level,
                title: format!("{} {}", step.title(), what),
                message: step_message(step, &report.mode),
                file,
                line,
                column: 0,
            }
        })
        .collect()
}

fn step_problem(step: &Step) -> (Level, &'static str) {
    if step.rollback == "failed" {
        (Level::Error, "was not rolled back")
    } else if step.status == crate::StepState::Failed {
        (Level::Error, "failed")
    } else {
        (Level::Warning, "drifted")
    }
}

/// What went wrong with a step, from the fixed words the record keeps.
fn step_message(step: &Step, mode: &str) -> String {
    let mut message = String::new();
    if !step.target.is_empty() {
        let _ = write!(message, "target {}", step.target);
        if !step.endpoint.is_empty() {
            let _ = write!(message, " on {}", step.endpoint);
        }
        message.push_str(": ");
    }
    if step.drift == "detected" && step.status != crate::StepState::Failed {
        message.push_str("what was applied is no longer what is there");
    } else if step.reason.is_empty() {
        message.push_str("the step failed");
    } else {
        let _ = write!(
            message,
            "{} ({})",
            step.reason,
            crate::model::explain(&step.reason)
        );
    }
    if step.status == crate::StepState::Failed && mode == "apply" {
        message.push_str("; `meridian apply --resume` continues from here once the cause is fixed");
    }
    message
}

/// The summary of a run, as the engine's job page shows it: the Markdown
/// report, then where the record is, so the run can be read back.
pub fn run_summary(report: &Report, record_kept_in: &str) -> String {
    let mut out = crate::render(report, crate::Format::Markdown);
    if !report.run.is_empty() && !record_kept_in.is_empty() {
        let _ = writeln!(
            out,
            "\nThe record is kept in {}: `meridian report {}` reads it back.",
            md(record_kept_in),
            report.run
        );
    }
    out
}

/// The summary of a command that did not get as far as a run: a validate
/// that refused the documents, a plan refused its lock. The problems are
/// the whole story.
pub fn problems_summary(command: &str, problems: &[Problem]) -> String {
    let mut out = format!(
        "## ✗ Meridian {} failed: {}\n\n",
        md(command),
        plural(problems.len(), "problem")
    );
    for problem in problems {
        out.push_str(&problem.markdown());
        out.push('\n');
    }
    out
}

/// Where a summary goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Summary {
    /// Append the content to the file; GitHub's `$GITHUB_STEP_SUMMARY`.
    AppendToFile { path: PathBuf, content: String },
    /// Write the content to the file, then print the line, which names the
    /// file to the engine; Azure's `task.uploadsummary`.
    FileThenLine {
        path: PathBuf,
        content: String,
        line: String,
    },
    /// Run the engine's agent with the content on its standard input, if
    /// the agent is there; Buildkite's `buildkite-agent annotate`.
    Agent {
        program: String,
        args: Vec<String>,
        stdin: String,
    },
}

fn buildkite_style(markdown: &str) -> &'static str {
    if markdown.starts_with("## ✗") {
        "error"
    } else if markdown.starts_with("## ✓") {
        "success"
    } else {
        "info"
    }
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// Escapes text for a line of Markdown prose.
fn md(text: &str) -> String {
    let mut out = String::new();
    for c in crate::render::escape(text).chars() {
        match c {
            '|' | '`' | '*' | '_' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GitHub Actions: `::error file=F,line=L,col=C,title=T::message`
// ---------------------------------------------------------------------------

fn github_annotation(problem: &Problem) -> String {
    let mut properties = Vec::new();
    if !problem.file.is_empty() {
        properties.push(format!("file={}", github_escape_property(&problem.file)));
        if problem.line > 0 {
            properties.push(format!("line={}", problem.line));
            if problem.column > 0 {
                properties.push(format!("col={}", problem.column));
            }
        }
    }
    properties.push(format!(
        "title={}",
        github_escape_property(&format!("Meridian: {}", problem.title))
    ));
    format!(
        "::{} {}::{}",
        problem.level.as_str(),
        properties.join(","),
        github_escape(&problem.message)
    )
}

fn github_escape(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn github_escape_property(text: &str) -> String {
    github_escape(text).replace(':', "%3A").replace(',', "%2C")
}

// ---------------------------------------------------------------------------
// Azure Pipelines: `##vso[task.logissue type=error;sourcepath=F;...]message`
// ---------------------------------------------------------------------------

fn azure_annotation(problem: &Problem) -> String {
    let mut properties = vec![format!("type={}", problem.level.as_str())];
    if !problem.file.is_empty() {
        properties.push(format!("sourcepath={}", azure_escape(&problem.file)));
        if problem.line > 0 {
            properties.push(format!("linenumber={}", problem.line));
            if problem.column > 0 {
                properties.push(format!("columnnumber={}", problem.column));
            }
        }
    }
    format!(
        "##vso[task.logissue {}]{}: {}",
        properties.join(";"),
        azure_escape(&problem.title),
        azure_escape(&problem.message)
    )
}

fn azure_escape(text: &str) -> String {
    text.replace('%', "%AZP25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace(']', "%5D")
        .replace(';', "%3B")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn each_engine_is_told_by_its_own_variable() {
        for engine in Engine::ALL {
            let variable = match engine {
                Engine::ArgoWorkflows => "ARGO_NODE_ID".to_owned(),
                Engine::Tekton => "TEKTON_RESULTS_DIR".to_owned(),
                other => other.variable().to_owned(),
            };
            let set = env(&[(&variable, "true")]);
            assert_eq!(Engine::detect(&set), Some(engine), "{variable}");
            // Set to nothing is not set.
            let empty = env(&[(&variable, "")]);
            assert_eq!(Engine::detect(&empty), None, "{variable}");
        }
    }

    #[test]
    fn no_variable_means_no_engine_and_a_laptop_is_not_one() {
        assert_eq!(Engine::detect(&env(&[])), None);
        assert_eq!(
            Engine::detect(&env(&[("HOME", "/home/me"), ("TERM", "xterm")])),
            None
        );
        // A prefix must be a whole word: ARGOS is not ARGO_.
        assert_eq!(Engine::detect(&env(&[("ARGOS", "1")])), None);
    }

    #[test]
    fn the_first_engine_found_wins_when_two_are_set() {
        let both = env(&[("GITLAB_CI", "true"), ("BUILDKITE", "true")]);
        assert_eq!(Engine::detect(&both), Some(Engine::GitLabCi));
    }

    fn problem() -> Problem {
        Problem {
            level: Level::Error,
            title: "validate".to_owned(),
            message: "spec.resources[0]: unknown field \"colour\", 100% sure\nsecond line"
                .to_owned(),
            file: "site.yaml".to_owned(),
            line: 34,
            column: 7,
        }
    }

    #[test]
    fn github_annotations_name_the_file_and_line_and_escape_the_rest() {
        assert_eq!(
            Engine::GitHubActions.annotation(&problem()).unwrap(),
            "::error file=site.yaml,line=34,col=7,title=Meridian%3A validate::spec.resources[0]: \
             unknown field \"colour\", 100%25 sure%0Asecond line"
        );
        let mut plain = problem();
        plain.file.clear();
        plain.level = Level::Warning;
        assert_eq!(
            Engine::GitHubActions.annotation(&plain).unwrap(),
            "::warning title=Meridian%3A validate::spec.resources[0]: unknown field \"colour\", \
             100%25 sure%0Asecond line"
        );
        let mut no_column = problem();
        no_column.column = 0;
        assert!(Engine::GitHubActions
            .annotation(&no_column)
            .unwrap()
            .starts_with("::error file=site.yaml,line=34,title="));
    }

    #[test]
    fn azure_annotations_use_the_logissue_command() {
        assert_eq!(
            Engine::AzurePipelines.annotation(&problem()).unwrap(),
            "##vso[task.logissue type=error;sourcepath=site.yaml;linenumber=34;columnnumber=7]\
             validate: spec.resources[0%5D: unknown field \"colour\", 100%AZP25 sure%0Asecond line"
        );
        let mut tricky = problem();
        tricky.message = "a; b] c".to_owned();
        assert!(Engine::AzurePipelines
            .annotation(&tricky)
            .unwrap()
            .ends_with("]validate: a%3B b%5D c"));
    }

    #[test]
    fn the_other_engines_have_no_annotation_line() {
        for engine in [
            Engine::GitLabCi,
            Engine::Buildkite,
            Engine::Jenkins,
            Engine::ArgoWorkflows,
            Engine::Tekton,
        ] {
            assert_eq!(engine.annotation(&problem()), None, "{}", engine.name());
            if engine != Engine::Buildkite {
                assert_eq!(engine.summary("## x", &env(&[])), None, "{}", engine.name());
            }
        }
    }

    #[test]
    fn a_summary_goes_where_each_engine_reads_one() {
        let github = env(&[("GITHUB_STEP_SUMMARY", "/tmp/summary.md")]);
        assert_eq!(
            Engine::GitHubActions.summary("## ✓ done", &github),
            Some(Summary::AppendToFile {
                path: PathBuf::from("/tmp/summary.md"),
                content: "## ✓ done".to_owned()
            })
        );
        // Without the file there is nowhere to write.
        assert_eq!(Engine::GitHubActions.summary("## ✓ done", &env(&[])), None);
        let azure = env(&[("AGENT_TEMPDIRECTORY", "/agent/_temp")]);
        assert_eq!(
            Engine::AzurePipelines.summary("## ✓ done", &azure),
            Some(Summary::FileThenLine {
                path: PathBuf::from("/agent/_temp/meridian-summary.md"),
                content: "## ✓ done".to_owned(),
                line: "##vso[task.uploadsummary]/agent/_temp/meridian-summary.md".to_owned(),
            })
        );
        assert_eq!(
            Engine::Buildkite.summary("## ✗ failed", &env(&[])),
            Some(Summary::Agent {
                program: "buildkite-agent".to_owned(),
                args: vec![
                    "annotate".to_owned(),
                    "--context".to_owned(),
                    "meridian".to_owned(),
                    "--style".to_owned(),
                    "error".to_owned(),
                    "--append".to_owned(),
                ],
                stdin: "## ✗ failed".to_owned(),
            })
        );
    }

    #[test]
    fn a_problem_summary_lists_each_problem_at_its_place() {
        let text = problems_summary("validate", &[problem()]);
        assert_eq!(
            text,
            "## ✗ Meridian validate failed: 1 problem\n\n- **validate** `site.yaml:34:7`: \
             spec.resources\\[0\\]: unknown field &quot;colour&quot;, 100% sure second line\n"
        );
    }
}
