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
    /// The arguments that name the resource the document calls `resource` to
    /// `az resource`; `None` when it is not one resource with a plain name.
    fn args_of(&self, resource: &str) -> Option<Vec<String>> {
        let (key, _) = self.resources.iter().find(|(_, id)| *id == resource)?;
        let parts: Vec<&str> = key.split('/').collect();
        let n = parts.len().checked_sub(1)? / 2;
        if n == 0 || parts.len() != 1 + 2 * n {
            return None;
        }
        crate::inspect::show_args(&parts[..=n].join("/"), &parts[n + 1..].join("/"))
    }

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
        self.az_on(context, deployment, &["deployment", "group", operation])
    }

    /// The error with the resources whose deployment operations failed.
    /// Operations that cannot be listed add nothing.
    fn with_failures(
        &self,
        mut error: RunError,
        context: &RunContext,
        deployment: &Deployment,
    ) -> RunError {
        let listed = self
            .az_on(
                context,
                deployment,
                &["deployment", "operation", "group", "list"],
            )
            .args(["--output", "json"])
            .run(&NoLog);
        if let Some(listed) = listed.ok().filter(|l| l.success()) {
            error.failures =
                crate::operations::failed_operations(&listed.stdout, |kind, name, id| {
                    if id.is_empty() {
                        deployment.resource(&format!("{kind}/{name}"))
                    } else {
                        deployment.resource(id)
                    }
                });
        }
        error
    }

    fn az_on(&self, context: &RunContext, deployment: &Deployment, command: &[&str]) -> Tool {
        let mut tool = Tool::new(&self.program)
            .args(command.iter().copied())
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

    /// The deployment an artifact makes, with the artifact written out.
    fn load(context: &RunContext, artifact: &Artifact) -> Result<Deployment, RunError> {
        let Some(settings) = artifact.files.get(DEPLOYMENT_PATH) else {
            return Err(RunError::new(format!(
                "the artifact has no {DEPLOYMENT_PATH}; it was not compiled by this plugin"
            )));
        };
        let deployment = Deployment::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;
        Ok(deployment)
    }

    /// The resources the deployment made, in the order to delete them, or
    /// `None` when there is no such deployment.
    fn removals(
        &self,
        context: &RunContext,
        deployment: &Deployment,
    ) -> Result<Option<Vec<String>>, RunError> {
        let shown = self
            .az(context, deployment, "show")
            .args(["--output", "json"])
            .run(&NoLog)?;
        if !shown.success() {
            if not_found(&shown.stderr) {
                return Ok(None);
            }
            return Err(shown.into_error("az could not show the deployment"));
        }
        let record = decode_value(shown.stdout.trim().as_bytes()).map_err(|e| {
            RunError::new(format!("the deployment's record could not be read: {e}"))
        })?;
        Ok(Some(removal_order(&record)))
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

/// The resources a deployment's record says it made
/// (`properties.outputResources`), in the order to delete them: children
/// before their parents, and otherwise last made first.
pub(crate) fn removal_order(record: &Value) -> Vec<String> {
    let mut ids: Vec<String> = record
        .as_map()
        .and_then(|m| m.get("properties"))
        .and_then(Value::as_map)
        .and_then(|p| p.get("outputResources"))
        .and_then(Value::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|r| Some(r.as_map()?.get("id")?.as_str()?.to_owned()))
        .collect();
    ids.reverse();
    ids.sort_by_key(|id| std::cmp::Reverse(id.trim_end_matches('/').matches('/').count()));
    ids
}

/// Whether Azure said what was asked for is not there.
fn not_found(stderr: &str) -> bool {
    stderr.contains("NotFound") || stderr.contains("could not be found")
}

/// The change that deleting one resource is, by the id the author wrote.
fn deletion(resource_id: &str, deployment: &Deployment) -> Change {
    let azure_type = resource_id
        .split_once("/providers/")
        .map_or("", |(_, tail)| tail)
        .rsplit_once('/')
        .map_or("", |(azure_type, _)| azure_type);
    Change {
        resource: deployment.resource(resource_id),
        action: ChangeAction::Delete,
        summary: azure_type.to_owned(),
    }
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
        let finished = finished.map_err(|e| self.with_failures(e, context, &deployment))?;
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

    fn inspects(&self) -> bool {
        true
    }

    /// The resource as Azure has it now, from `az resource show`. Bicep
    /// keeps no record to compare with, so whether it is as declared is
    /// left to the comparison.
    fn inspect(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        _endpoint: &str,
    ) -> Result<meridian_sdk::Inspection, RunError> {
        let Some(settings) = artifact.files.get(DEPLOYMENT_PATH) else {
            return Err(RunError::new(format!(
                "the artifact has no {DEPLOYMENT_PATH}; it was not compiled by this plugin"
            )));
        };
        let deployment = Deployment::read(settings)?;
        let args = deployment.args_of(resource).ok_or_else(|| {
            RunError::new(format!(
                "{resource} is not one Azure resource with a plain name ({})",
                meridian_sdk::rules::UNSUPPORTED_INSPECT.name
            ))
        })?;
        let mut tool = Tool::new(&self.program)
            .args([
                "resource",
                "show",
                "--resource-group",
                &deployment.resource_group,
            ])
            .dir(&context.work_dir);
        if !deployment.subscription.is_empty() {
            tool = tool.args(["--subscription", &deployment.subscription]);
        }
        let shown_as = format!(
            "az resource show --resource-group {} {}",
            deployment.resource_group,
            args.join(" ")
        );
        crate::inspect::show(tool, &args, &self.extra_args, shown_as)
    }

    fn acts(&self) -> Vec<meridian_sdk::ActSchema> {
        crate::act::acts("@*")
    }

    /// `az resource invoke-action` on the resource, after the deployment.
    fn act(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        action: &str,
        log: &dyn Log,
    ) -> Result<meridian_sdk::Acted, RunError> {
        let Some(settings) = artifact.files.get(DEPLOYMENT_PATH) else {
            return Err(RunError::new(format!(
                "the artifact has no {DEPLOYMENT_PATH}; it was not compiled by this plugin"
            )));
        };
        let deployment = Deployment::read(settings)?;
        let args = deployment.args_of(resource).ok_or_else(|| {
            RunError::new(format!(
                "{resource} is not one Azure resource with a plain name, so Meridian cannot \
                 {action} it"
            ))
        })?;
        let mut tool = Tool::new(&self.program)
            .args([
                "resource",
                "invoke-action",
                "--resource-group",
                &deployment.resource_group,
            ])
            .dir(&context.work_dir);
        if !deployment.subscription.is_empty() {
            tool = tool.args(["--subscription", &deployment.subscription]);
        }
        let shown_as = format!(
            "az resource invoke-action --resource-group {} --action {action} {}",
            deployment.resource_group,
            args.join(" ")
        );
        crate::act::invoke(tool, &args, action, &self.extra_args, shown_as, log)
    }

    fn destroys(&self) -> bool {
        true
    }

    fn plan_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        _log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let deployment = Self::load(context, artifact)?;
        let ids = self.removals(context, &deployment)?.unwrap_or_default();
        Ok(Plan {
            changes: ids.iter().map(|id| deletion(id, &deployment)).collect(),
            unknown: false,
            saved: None,
        })
    }

    /// Deletes each resource the deployment's record says it made, children
    /// first, then the record. A resource already gone is not an error.
    fn destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let deployment = Self::load(context, artifact)?;
        let Some(ids) = self.removals(context, &deployment)? else {
            log.out(&format!(
                "deployment {}: there is no such deployment",
                deployment.name
            ));
            return Ok(Vec::new());
        };
        let mut removed = Vec::new();
        for id in &ids {
            let change = deletion(id, &deployment);
            log.out(&format!("deleting {}", change.resource));
            let finished = Tool::new(&self.program)
                .args(["resource", "delete", "--ids", id])
                .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
                .dir(&context.work_dir)
                .args(self.extra_args.iter().cloned())
                .run(log)?;
            if !finished.success() && !not_found(&finished.stderr) {
                let mut error = finished.into_error(format!("az could not delete {id}"));
                error.changes = removed;
                return Err(error);
            }
            removed.push(change);
        }
        let forgotten = self.az(context, &deployment, "delete").run(log)?;
        if !forgotten.success() && !not_found(&forgotten.stderr) {
            let mut error = forgotten.into_error(format!(
                "az could not delete the record of the deployment {}",
                deployment.name
            ));
            error.changes = removed;
            return Err(error);
        }
        Ok(removed)
    }
}
