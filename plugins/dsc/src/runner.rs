//! The half that runs a compiled configuration through DscPipelineRunner. It
//! sits outside the compile chain: nothing here is reached by a compile.

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, Operations, Plan, RunContext,
    RunError, Runner, Value,
};
use meridian_sdk::{settings, Setting};

use crate::DATUM_PATH;

/// Names the PowerShell program, when it is not `pwsh` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_PWSH";
/// The DscPipelineRunner module to import: a name PowerShell can find, or
/// the path of its manifest.
pub const MODULE_VARIABLE: &str = "MERIDIAN_DSC_MODULE";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "pwsh",
            "The PowerShell program, when it is not `pwsh` on `PATH`.",
        ),
        Setting::new(
            "module",
            MODULE_VARIABLE,
            "Dsc.PipelineRunner",
            "The DscPipelineRunner module to import: a name PowerShell can find, or the path of its manifest.",
        ),
    ]
}

/// The line of PowerShell's output that carries the run's result.
const RESULT_MARK: &str = "MERIDIAN-RESULT ";

/// Runs a compiled configuration with `Invoke-DscRunner`.
///
/// A plan is the runner's `Test` mode: every resource is tested and none is
/// set. An apply is a `Test` to learn what has drifted, then `Set`.
pub struct DscRunner {
    pub program: String,
    pub module: String,
}

/// One resource's result in a run.
struct Outcome {
    resource: String,
    r#type: String,
    /// `OK`, `FAIL` or `SKIP`.
    status: String,
    message: String,
}

impl DscRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        DscRunner {
            program: settings::text(setting("program")),
            module: settings::text(setting("module")),
        }
    }

    fn prepare(&self, context: &RunContext, artifact: &Artifact) -> Result<(), RunError> {
        if !artifact.files.contains_key(DATUM_PATH) {
            return Err(RunError::new(format!(
                "the artifact has no {DATUM_PATH}; it was not compiled by this plugin"
            )));
        }
        write_artifact(&context.work_dir, artifact)
    }

    /// One `Invoke-DscRunner` over the work directory.
    fn invoke(
        &self,
        context: &RunContext,
        mode: &str,
        log: &dyn Log,
    ) -> Result<Vec<Outcome>, RunError> {
        // The runner compiles in a second runspace with its own current
        // directory, so the configuration is named by its full path.
        let directory = std::path::absolute(&context.work_dir)
            .unwrap_or_else(|_| context.work_dir.clone())
            .display()
            .to_string();
        let script = format!(
            "$ErrorActionPreference = 'Stop'; Import-Module {module}; \
             $run = Invoke-DscRunner -ConfigurationSourcePath {directory} -Mode {mode}; \
             '{RESULT_MARK}' + ($run | ConvertTo-Json -Depth 8 -Compress)",
            module = quote(&self.module),
            directory = quote(&directory),
        );
        let finished = Tool::new(&self.program)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .dir(&context.work_dir)
            .run_ok("DscPipelineRunner could not run the configuration", log)?;
        let Some(json) = finished
            .stdout
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix(RESULT_MARK.trim_end()))
        else {
            return Err(
                finished.into_error("DscPipelineRunner finished without reporting a result")
            );
        };
        read_result(json.trim())
    }
}

/// A PowerShell single-quoted string, in which only the quote is special.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A value PowerShell wrote as one object when a list had one element.
fn each(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::List(items)) => items.iter().collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(one) => vec![one],
    }
}

/// Reads the summary `Invoke-DscRunner` returns.
fn read_result(json: &str) -> Result<Vec<Outcome>, RunError> {
    let unreadable = |why: String| {
        RunError::new(format!(
            "DscPipelineRunner's result could not be read: {why}"
        ))
    };
    let value = decode_value(json.as_bytes()).map_err(unreadable)?;
    let summary = value
        .as_map()
        .ok_or_else(|| unreadable("it is not an object".to_owned()))?;
    let mut outcomes = Vec::new();
    for configuration in each(summary.get("Configurations")) {
        let Some(configuration) = configuration.as_map() else {
            continue;
        };
        let text = |map: &std::collections::BTreeMap<String, Value>, key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        // The run itself broke, as opposed to a resource failing in it.
        if text(configuration, "Status") == "AbortedByException" {
            return Err(RunError::new(format!(
                "DscPipelineRunner stopped: {}",
                text(configuration, "ErrorMessage")
            )));
        }
        for result in each(configuration.get("Results")) {
            let Some(result) = result.as_map() else {
                continue;
            };
            outcomes.push(Outcome {
                resource: text(result, "InstanceName"),
                r#type: text(result, "ResourceType"),
                status: text(result, "Status"),
                message: text(result, "ErrorMessage"),
            });
        }
    }
    Ok(outcomes)
}

/// What a `Test` run says would change: in that mode the runner records a
/// resource that is not in its desired state as `FAIL`.
fn drift(outcomes: &[Outcome]) -> Vec<Change> {
    outcomes
        .iter()
        .filter(|o| o.status == "FAIL")
        .map(|o| Change {
            resource: o.resource.clone(),
            action: ChangeAction::Update,
            summary: if o.message.is_empty() {
                o.r#type.clone()
            } else {
                format!("{}: {}", o.r#type, o.message)
            },
        })
        .collect()
}

impl Runner for DscRunner {
    fn settings(&self) -> Vec<Setting> {
        settings()
    }

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
        self.prepare(context, artifact)?;
        log.out("testing every resource against its desired state");
        let changes = drift(&self.invoke(context, "Test", log)?);
        for change in &changes {
            log.out(&change.to_string());
        }
        Ok(Plan {
            changes,
            unknown: false,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        self.prepare(context, artifact)?;
        // The runner's result does not say which resources a Set changed, so
        // the ones that had drifted are found first.
        log.out("testing every resource against its desired state");
        let drifted = drift(&self.invoke(context, "Test", log)?);
        log.out("setting the resources that are not in their desired state");
        let outcomes = self.invoke(context, "Set", log)?;

        let failed: Vec<&Outcome> = outcomes.iter().filter(|o| o.status == "FAIL").collect();
        let changes: Vec<Change> = drifted
            .into_iter()
            .filter(|c| !failed.iter().any(|o| o.resource == c.resource))
            .collect();
        if !failed.is_empty() {
            let mut error = RunError::new(format!(
                "DscPipelineRunner could not set {}",
                failed
                    .iter()
                    .map(|o| format!("{:?}", o.resource))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            error.output = failed
                .iter()
                .map(|o| format!("{}/{}: {}", o.r#type, o.resource, o.message))
                .collect::<Vec<_>>()
                .join("\n");
            error.changes = changes;
            return Err(error);
        }
        Ok(Applied {
            changes,
            outputs: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_read_whether_powershell_wrote_lists_or_single_objects() {
        let many = r#"{"Status":"Failed","Configurations":[{"Status":"Completed","Results":[
            {"ResourceType":"PSDscResources/Service","InstanceName":"spooler","Status":"OK","ErrorMessage":null},
            {"ResourceType":"PSDscResources/Registry","InstanceName":"key","Status":"FAIL","ErrorMessage":"not in the desired state"}]}]}"#;
        let outcomes = read_result(many).unwrap();
        assert_eq!(outcomes.len(), 2);
        let shown: Vec<String> = drift(&outcomes).iter().map(|c| c.to_string()).collect();
        assert_eq!(
            shown,
            ["update key: PSDscResources/Registry: not in the desired state"]
        );

        let one = r#"{"Configurations":{"Status":"Completed","Results":{"ResourceType":"a/b","InstanceName":"x","Status":"SKIP","ErrorMessage":""}}}"#;
        assert_eq!(read_result(one).unwrap().len(), 1);
    }

    #[test]
    fn a_run_that_broke_is_an_error() {
        let aborted = r#"{"Configurations":[{"Status":"AbortedByException","ErrorMessage":"dsc is not installed","Results":[]}]}"#;
        let err = read_result(aborted).err().unwrap();
        assert_eq!(err.msg, "DscPipelineRunner stopped: dsc is not installed");
    }

    #[test]
    fn a_path_with_a_quote_stays_one_powershell_string() {
        assert_eq!(quote("C:\\it's here"), "'C:\\it''s here'");
    }
}
