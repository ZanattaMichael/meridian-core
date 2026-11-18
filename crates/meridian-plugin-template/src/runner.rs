//! The half that hands the artifact to the real tool. It sits outside the
//! compile chain: nothing here is reached by a compile.

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{settings, Setting};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Log, Operations, Plan, RunContext, RunError, Runner,
};

use crate::MANIFEST_PATH;

/// Names the program to run, when it is not `filesync` on `PATH`. Every
/// first-party plugin has a variable like it, and the tests use it to put a
/// stand-in in the tool's place.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_TEMPLATE";

/// What an operator can configure about this runner. Each setting is read
/// through `meridian_sdk::settings`, so the environment, the project's
/// `meridian.yaml` and the default are honoured in that order, and
/// `meridian plugins --settings` can list them.
///
/// TODO(plugin): declare every setting the runner reads, and nothing it does
/// not. An argument-list setting is read with `settings::args`, which splits
/// it like a shell command line.
pub fn settings() -> Vec<Setting> {
    vec![Setting::new(
        "program",
        PROGRAM_VARIABLE,
        "filesync",
        "The `filesync` program to run, when it is not on `PATH`.",
    )]
}

/// Runs a compiled manifest with `filesync`.
///
/// A plan is `filesync plan manifest.tsv` and an apply is
/// `filesync apply manifest.tsv`, both run in the work directory.
pub struct TemplateRunner {
    pub program: String,
}

impl TemplateRunner {
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        TemplateRunner {
            program: settings::text(setting("program")),
        }
    }

    /// Writes the artifact where the tool will read it and runs one command.
    fn run(
        &self,
        command: &str,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        // The host keeps the work directory between runs. `write_artifact`
        // removes what the previous artifact wrote and this one does not, and
        // leaves the tool's own state alone.
        write_artifact(&context.work_dir, artifact)?;
        // TODO(plugin): pass a secret with `secret_arg`, never `arg`, so it
        // cannot appear in a log line or in an error.
        let finished = Tool::new(&self.program)
            .arg(command)
            .arg(MANIFEST_PATH)
            .dir(&context.work_dir)
            .run_ok(&format!("filesync {command} failed"), log)?;
        changes(&finished.stdout)
    }
}

/// Reads the tool's report: one `create`, `update` or `delete` and a path on
/// each line.
///
/// TODO(plugin): a tool that can print JSON is far safer to read than one
/// that prints prose. Ask it for JSON and use `Tool::capture_stdout`.
fn changes(report: &str) -> Result<Vec<Change>, RunError> {
    let mut changes = Vec::new();
    for line in report.lines().filter(|l| !l.trim().is_empty()) {
        let (action, path) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        let action = match action {
            "create" => ChangeAction::Create,
            "update" => ChangeAction::Update,
            "delete" => ChangeAction::Delete,
            // A line this plugin cannot read is a failure, not something to
            // skip: skipping it would report a change as no change.
            _ => {
                return Err(RunError::new(format!(
                    "filesync printed a line this plugin does not understand: {line:?}"
                )))
            }
        };
        changes.push(Change {
            resource: path.to_owned(),
            action,
            summary: String::new(),
        });
    }
    Ok(changes)
}

impl Runner for TemplateRunner {
    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    /// TODO(plugin): declare only the operations you implement. The host
    /// never calls the others.
    fn operations(&self) -> Operations {
        Operations {
            plan: true,
            apply: true,
            outputs: false,
        }
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        Ok(Plan {
            changes: self.run("plan", context, artifact, log)?,
            // TODO(plugin): set this when the tool cannot say what would
            // change, so an empty list is never read as "in sync".
            unknown: false,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        Ok(Applied {
            changes: self.run("apply", context, artifact, log)?,
            outputs: Vec::new(),
        })
    }
}
