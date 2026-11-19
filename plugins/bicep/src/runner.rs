//! The half that deploys a Bicep file with the `az` command line. It sits
//! outside the compile chain: nothing here is reached by a compile.

use std::collections::BTreeMap;

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog, Operations, Output, Plan,
    RunContext, RunError, Runner, SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::{DEPLOYMENT_PATH, TEMPLATE_PATH};

/// What `--tool-debug` adds to the tool, level 1 first. Flags of Meridian's own `az` calls; Azure Resource Manager has no log to raise.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["--verbose"]),
    meridian_sdk::debug::DebugLevel::args(&["--debug"]),
];

/// Names the program to run, when it is not `az` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_AZ";
/// Extra arguments for every call, split like a shell command line.
pub const ARGS_VARIABLE: &str = "MERIDIAN_AZ_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "az",
            "The Azure CLI program to run, when it is not `az` on `PATH`. Shared with the arm target.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every `az` call, split like a shell command line: `--only-show-errors`. Shared with the arm target.",
        ),
    ]
}

/// Deploys a compiled Bicep file to a resource group.
///
/// A plan is `az deployment group what-if`, Azure's own preview. An apply is
/// `az deployment group create`, preceded by the same preview so the changes
/// can be reported.
pub struct BicepRunner {
    pub program: String,
    /// Arguments added to every call, after Meridian's own.
    pub extra_args: Vec<String>,
}

/// What `deployment.json` says.
struct Deployment {
    name: String,
    resource_group: String,
    subscription: String,
    /// `type/name`, lower-cased, to the id the author wrote.
    resources: BTreeMap<String, String>,
}

impl Deployment {
    fn read(text: &str) -> Result<Deployment, RunError> {
        let bad = || RunError::new(format!("{DEPLOYMENT_PATH} is not one this plugin compiled"));
        let value = decode_value(text.as_bytes()).map_err(|_| bad())?;
        let map = value.as_map().ok_or_else(bad)?;
        let string = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let deployment = Deployment {
            name: string("name"),
            resource_group: string("resourceGroup"),
            subscription: string("subscription"),
            resources: map
                .get("resources")
                .and_then(Value::as_map)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
        };
        if deployment.name.is_empty() || deployment.resource_group.is_empty() {
            return Err(bad());
        }
        Ok(deployment)
    }

    /// The document's id for an Azure resource id, or the readable end of the
    /// resource id when the document did not name it plainly.
    fn resource(&self, resource_id: &str) -> String {
        let tail = resource_id
            .split_once("/providers/")
            .map_or(resource_id, |(_, tail)| tail);
        self.resources
            .get(&tail.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| tail.to_owned())
    }
}

impl BicepRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        BicepRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
        }
    }

    /// One `az deployment group` call.
    fn az(&self, context: &RunContext, deployment: &Deployment, operation: &str) -> Tool {
        let mut tool = Tool::new(&self.program)
            .args(["deployment", "group", operation])
            .args(["--resource-group", &deployment.resource_group])
            .args(["--name", &deployment.name])
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
            .dir(&context.work_dir)
            .capture_stdout();
        if !deployment.subscription.is_empty() {
            tool = tool.args(["--subscription", &deployment.subscription]);
        }
        tool.args(self.extra_args.iter().cloned())
    }

    /// The call given the template and, when the run has secrets, the
    /// parameters file that holds their values: named on the command line by
    /// its path and nothing more.
    fn with_template(tool: Tool, secrets: Option<&SecretFile>) -> Tool {
        let tool = tool.args(["--template-file", TEMPLATE_PATH]);
        match secrets {
            Some(file) => tool.args(["--parameters", &format!("@{}", file.path().display())]),
            None => tool,
        }
    }

    fn preview(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        secrets: Option<&SecretFile>,
        log: &dyn Log,
    ) -> Result<(Deployment, Plan), RunError> {
        let (Some(_), Some(settings)) = (
            artifact.files.get(TEMPLATE_PATH),
            artifact.files.get(DEPLOYMENT_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {TEMPLATE_PATH} and {DEPLOYMENT_PATH}; it was not compiled by this plugin"
            )));
        };
        let deployment = Deployment::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;

        log.out(&format!(
            "resource group {}: asking Azure what would change",
            deployment.resource_group
        ));
        let finished = Self::with_template(self.az(context, &deployment, "what-if"), secrets)
            .args(["--no-pretty-print", "--output", "json"])
            .run_ok("az could not preview the deployment", log)?;
        let plan = read_what_if(&finished.stdout, &deployment)?;
        for change in &plan.changes {
            log.out(&change.to_string());
        }
        Ok((deployment, plan))
    }
}

/// The run's secrets as a Resource Manager parameters file in a private
/// file, held in memory where the system allows and removed when dropped;
/// nothing when the run has none. Azure keeps a secure parameter's value out
/// of the deployment's record.
fn secrets_file(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
    if context.secrets.is_empty() {
        return Ok(None);
    }
    context
        .secrets
        .json_file_shaped(
            "{\"$schema\":\"https://schema.management.azure.com/schemas/2019-04-01/deploymentParameters.json#\",\"contentVersion\":\"1.0.0.0\",\"parameters\":{",
            ",",
            "}}\n",
            "parameters.json",
            |name| {
                (
                    format!(
                        "{}:{{\"value\":\"",
                        Value::String(crate::secret_parameter(name)).canonical()
                    ),
                    "\"}".to_owned(),
                )
            },
        )
        .map(Some)
        .map_err(|e| RunError::new(format!("writing the parameters file: {e}")))
}

/// Reads the result of `az deployment group what-if`.
fn read_what_if(json: &str, deployment: &Deployment) -> Result<Plan, RunError> {
    let result = decode_value(json.trim().as_bytes())
        .map_err(|e| RunError::new(format!("az's preview could not be read: {e}")))?;
    let changes = result
        .as_map()
        .and_then(|m| m.get("changes"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    let mut plan = Plan::default();
    for change in changes {
        let Some(change) = change.as_map() else {
            continue;
        };
        let text = |key: &str| change.get(key).and_then(Value::as_str).unwrap_or("");
        let action = match text("changeType") {
            "Create" => ChangeAction::Create,
            "Delete" => ChangeAction::Delete,
            "Modify" => ChangeAction::Update,
            // Azure will deploy the resource but cannot say whether that
            // changes it, or cannot analyse the resource type at all.
            "Deploy" | "Unsupported" => {
                plan.unknown = true;
                continue;
            }
            // "NoChange" and "Ignore".
            _ => continue,
        };
        let resource_id = text("resourceId");
        let azure_type = resource_id
            .split_once("/providers/")
            .map_or("", |(_, tail)| tail)
            .rsplit_once('/')
            .map_or("", |(azure_type, _)| azure_type);
        plan.changes.push(Change {
            resource: deployment.resource(resource_id),
            action,
            summary: azure_type.to_owned(),
        });
    }
    Ok(plan)
}

/// Reads a deployment's `properties.outputs`, or the outputs object itself.
fn read_outputs(json: &str) -> Result<Vec<Output>, RunError> {
    let value = decode_value(json.trim().as_bytes())
        .map_err(|e| RunError::new(format!("the deployment's outputs could not be read: {e}")))?;
    let outputs = value
        .as_map()
        .and_then(|m| m.get("properties"))
        .and_then(Value::as_map)
        .and_then(|p| p.get("outputs"))
        .unwrap_or(&value);
    Ok(outputs
        .as_map()
        .map(|outputs| {
            outputs
                .iter()
                .filter_map(|(name, entry)| {
                    let entry = entry.as_map()?;
                    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
                    Some(Output {
                        name: name.clone(),
                        value: entry.get("value").cloned().unwrap_or(Value::Null),
                        sensitive: kind.to_ascii_lowercase().starts_with("secure"),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

impl Runner for BicepRunner {
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let secrets = secrets_file(context)?;
        Ok(self.preview(context, artifact, secrets.as_ref(), log)?.1)
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let secrets = secrets_file(context)?;
        let (deployment, plan) = self.preview(context, artifact, secrets.as_ref(), log)?;
        log.out(&format!(
            "resource group {}: deploying",
            deployment.resource_group
        ));
        let finished =
            Self::with_template(self.az(context, &deployment, "create"), secrets.as_ref())
                .args(["--output", "json"])
                .run_ok("az could not complete the deployment", log);
        drop(secrets);
        let finished = finished?;
        Ok(Applied {
            changes: plan.changes,
            outputs: read_outputs(&finished.stdout)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let settings =
            std::fs::read_to_string(context.work_dir.join(DEPLOYMENT_PATH)).map_err(|_| {
                RunError::new(
                    "nothing has been applied for this document yet, so it has no outputs",
                )
            })?;
        let deployment = Deployment::read(&settings)?;
        let finished = self
            .az(context, &deployment, "show")
            .args(["--output", "json"])
            .run_ok("az could not show the deployment", &NoLog)?;
        read_outputs(&finished.stdout)
    }
}
