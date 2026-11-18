//! The half that deploys a template with the `az` command line. It sits
//! outside the compile chain: nothing here is reached by a compile.
//!
//! A plan is Resource Manager's own preview, `what-if`. An apply runs the
//! same preview, so it can report what it changed, and then creates the
//! deployment. Outputs are read from the deployment's record.

use std::collections::BTreeMap;

use meridian_sdk::tool::{write_artifact, Finished, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog, Operations, Output, Plan,
    RunContext, RunError, Runner, SecretFile, Value,
};

use crate::validate::is_secure;
use crate::{DEPLOYMENT_PATH, TEMPLATE_PATH};

/// Names the program to run, when it is not `az` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_AZ";
/// Extra arguments for every call, split on whitespace: `--only-show-errors`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_AZ_ARGS";
/// Values for the template's parameters, split on whitespace. Each is given
/// to `--parameters`: `@prod.parameters.json adminUser=ops`.
pub const PARAMETERS_VARIABLE: &str = "MERIDIAN_AZ_PARAMETERS";

/// Deploys a compiled template.
pub struct ArmRunner {
    pub program: String,
    /// Arguments added to every call, after Meridian's own.
    pub extra_args: Vec<String>,
    /// Values given to `--parameters` when previewing and deploying.
    pub parameters: Vec<String>,
}

/// What `deployment.json` says about the deployment.
struct Deployment {
    name: String,
    /// Whether it is made into a subscription and not a resource group.
    subscription_scope: bool,
    resource_group: String,
    location: String,
    subscription: String,
    mode: String,
    /// Parameter name to its type.
    parameters: BTreeMap<String, String>,
    /// Lower-cased type and name to the id the author wrote.
    resources: BTreeMap<(String, String), String>,
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
        let entries = |key: &str| map.get(key).and_then(Value::as_map).into_iter().flatten();
        let deployment = Deployment {
            name: string("name"),
            subscription_scope: string("scope") == "subscription",
            resource_group: string("resourceGroup"),
            location: string("location"),
            subscription: string("subscription"),
            mode: string("mode"),
            parameters: entries("parameters")
                .filter_map(|(name, t)| Some((name.clone(), t.as_str()?.to_owned())))
                .collect(),
            resources: entries("resources")
                .filter_map(|(id, resource)| {
                    let resource = resource.as_map()?;
                    let part = |key: &str| Some(resource.get(key)?.as_str()?.to_lowercase());
                    Some(((part("type")?, part("name")?), id.clone()))
                })
                .collect(),
        };
        if deployment.name.is_empty() {
            return Err(bad());
        }
        Ok(deployment)
    }

    /// Where the deployment goes, for a log line.
    fn destination(&self) -> String {
        if self.subscription_scope {
            "the subscription".to_owned()
        } else {
            format!("resource group {}", self.resource_group)
        }
    }
}

impl ArmRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let words = |variable: &str| {
            std::env::var(variable)
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default()
        };
        ArmRunner {
            program: std::env::var(PROGRAM_VARIABLE)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "az".to_owned()),
            extra_args: words(ARGS_VARIABLE),
            parameters: words(PARAMETERS_VARIABLE),
        }
    }

    /// One `az deployment` call against the deployment.
    fn az(&self, context: &RunContext, deployment: &Deployment, operation: &str) -> Tool {
        let mut tool = Tool::new(&self.program)
            .args([
                "deployment",
                if deployment.subscription_scope {
                    "sub"
                } else {
                    "group"
                },
                operation,
            ])
            .args(["--name", &deployment.name])
            .dir(&context.work_dir)
            .capture_stdout();
        if !deployment.subscription_scope {
            tool = tool.args(["--resource-group", &deployment.resource_group]);
        }
        if !deployment.subscription.is_empty() {
            tool = tool.args(["--subscription", &deployment.subscription]);
        }
        tool.args(self.extra_args.iter().cloned())
            .args(["--output", "json"])
    }

    /// The arguments a preview and a deployment share: the template, where it
    /// goes and the values of its parameters.
    fn with_template(
        &self,
        mut tool: Tool,
        context: &RunContext,
        deployment: &Deployment,
        secrets: Option<&SecretFile>,
    ) -> Tool {
        tool = tool.args(["--template-file", TEMPLATE_PATH]);
        // The run's secrets are the values of securestring parameters, read
        // from a file only the user can read and named on the command line
        // by its path alone.
        if let Some(file) = secrets {
            tool = tool.args(["--parameters", &format!("@{}", file.path().display())]);
        }
        tool = if deployment.subscription_scope {
            tool.args(["--location", &deployment.location])
        } else {
            tool.args(["--mode", &deployment.mode])
        };
        for value in &self.parameters {
            // A value given by the operator may be a secret.
            tool = tool.arg("--parameters").secret_arg(value.clone());
        }
        // A value an earlier stage produced, under the name of a parameter
        // the template declares, is that parameter's value.
        for (name, parameter_type) in &deployment.parameters {
            let Some(value) = context.inputs.get(name) else {
                continue;
            };
            let pair = format!("{name}={}", value.canonical());
            tool = tool.arg("--parameters");
            tool = if is_secure(parameter_type) {
                tool.secret_arg(pair)
            } else {
                tool.arg(pair)
            };
        }
        tool
    }

    fn load(&self, context: &RunContext, artifact: &Artifact) -> Result<Deployment, RunError> {
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
        Ok(deployment)
    }

    fn preview(
        &self,
        context: &RunContext,
        deployment: &Deployment,
        secrets: Option<&SecretFile>,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        log.out(&format!(
            "deployment {}: asking Resource Manager what would change in {}",
            deployment.name,
            deployment.destination()
        ));
        let what_if = self
            .with_template(
                self.az(context, deployment, "what-if"),
                context,
                deployment,
                secrets,
            )
            .arg("--no-pretty-print")
            .run_ok("az could not preview the deployment", log)?;
        let result = parse(&what_if, "the preview")?;
        if let Some(error) = result.as_map().and_then(|m| m.get("error")) {
            if !matches!(error, Value::Null) {
                return Err(RunError::new(format!(
                    "Resource Manager rejected the template: {}",
                    error_text(error)
                )));
            }
        }
        Ok(read_changes(&result, &deployment.resources))
    }
}

/// The run's secrets as a Resource Manager parameters file in a private
/// file, held in memory where the system allows and removed when dropped;
/// nothing when the run has none.
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

fn parse(finished: &Finished, what: &str) -> Result<Value, RunError> {
    decode_value(finished.stdout.trim().as_bytes())
        .map_err(|e| RunError::new(format!("{what} could not be read: {e}")))
}

/// The code and message of an error as Resource Manager reports one.
fn error_text(error: &Value) -> String {
    let field = |key: &str| {
        error
            .as_map()
            .and_then(|m| m.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    match (field("code"), field("message")) {
        ("", "") => error.canonical(),
        (code, "") => code.to_owned(),
        ("", message) => message.to_owned(),
        (code, message) => format!("{code}: {message}"),
    }
}

/// Splits an Azure resource id into the resource's type and name:
/// `/subscriptions/s/resourceGroups/g/providers/Microsoft.Sql/servers/a/databases/b`
/// is `Microsoft.Sql/servers/databases` and `a/b`.
pub(crate) fn type_and_name(resource_id: &str) -> Option<(String, String)> {
    // An extension resource has several `/providers/`; the last is its own.
    let lower = resource_id.to_lowercase();
    if let Some(at) = lower.rfind("/providers/") {
        let mut parts = resource_id[at + "/providers/".len()..].split('/');
        let mut r#type = parts.next()?.to_owned();
        let mut names = Vec::new();
        loop {
            match (parts.next(), parts.next()) {
                (Some(t), Some(name)) => {
                    r#type.push('/');
                    r#type.push_str(t);
                    names.push(name);
                }
                (None, _) => break,
                (Some(_), None) => return None,
            }
        }
        if names.is_empty() {
            return None;
        }
        return Some((r#type, names.join("/")));
    }
    // A resource group has no provider in its id.
    let at = lower.rfind("/resourcegroups/")?;
    let name = &resource_id[at + "/resourcegroups/".len()..];
    (!name.is_empty() && !name.contains('/')).then(|| {
        (
            "Microsoft.Resources/resourceGroups".to_owned(),
            name.to_owned(),
        )
    })
}

/// Reads the changes of a preview.
fn read_changes(result: &Value, resources: &BTreeMap<(String, String), String>) -> Plan {
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
        let resource_id = text("resourceId");
        let parsed = type_and_name(resource_id);
        let mut summary = parsed.as_ref().map_or(String::new(), |(t, _)| t.clone());
        let action = match text("changeType") {
            "Create" => ChangeAction::Create,
            "Delete" => ChangeAction::Delete,
            "Modify" => {
                let paths: Vec<&str> = change
                    .get("delta")
                    .and_then(Value::as_list)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|d| d.as_map()?.get("path")?.as_str())
                    .collect();
                if !paths.is_empty() {
                    summary.push_str(", changes ");
                    summary.push_str(&paths[..paths.len().min(5)].join(", "));
                    if paths.len() > 5 {
                        summary.push_str(&format!(" and {} more", paths.len() - 5));
                    }
                }
                ChangeAction::Update
            }
            // Resource Manager will deploy it again and cannot say whether
            // that changes anything.
            "Deploy" => {
                summary.push_str(", deployed again; the preview cannot tell what changes");
                ChangeAction::Update
            }
            // The preview could not look at this resource at all.
            "Unsupported" => {
                plan.unknown = true;
                continue;
            }
            // NoChange, and Ignore: it exists, is not in the template, and is
            // left alone.
            _ => continue,
        };
        let resource = parsed
            .and_then(|(t, name)| {
                resources
                    .get(&(t.to_lowercase(), name.to_lowercase()))
                    .cloned()
            })
            .unwrap_or_else(|| resource_id.to_owned());
        plan.changes.push(Change {
            resource,
            action,
            summary,
        });
    }
    plan
}

/// Reads the outputs of a deployment's record.
fn read_outputs(record: &Value) -> Vec<Output> {
    let outputs = record
        .as_map()
        .and_then(|m| m.get("properties"))
        .and_then(Value::as_map)
        .and_then(|p| p.get("outputs"))
        .and_then(Value::as_map);
    outputs
        .into_iter()
        .flatten()
        .map(|(name, output)| {
            let field = |key: &str| output.as_map().and_then(|o| o.get(key));
            Output {
                name: name.clone(),
                value: field("value").cloned().unwrap_or(Value::Null),
                sensitive: field("type").and_then(Value::as_str).is_some_and(is_secure),
            }
        })
        .collect()
}

impl Runner for ArmRunner {
    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn accepts_inputs(&self) -> bool {
        true
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let deployment = self.load(context, artifact)?;
        let secrets = secrets_file(context)?;
        self.preview(context, &deployment, secrets.as_ref(), log)
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let deployment = self.load(context, artifact)?;
        let secrets = secrets_file(context)?;
        let plan = self.preview(context, &deployment, secrets.as_ref(), log)?;
        log.out(&format!(
            "deployment {}: deploying {} changes to {}",
            deployment.name,
            plan.changes.len(),
            deployment.destination()
        ));
        let created = self
            .with_template(
                self.az(context, &deployment, "create"),
                context,
                &deployment,
                secrets.as_ref(),
            )
            .run_ok(
                "the deployment failed; its operations say which resource failed and why",
                log,
            );
        drop(secrets);
        let created = created?;
        Ok(Applied {
            changes: plan.changes,
            outputs: read_outputs(&parse(&created, "the deployment's record")?),
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
        let shown = self.az(context, &deployment, "show").run(&NoLog)?;
        if !shown.success() {
            return Err(shown.into_error(format!(
                "az could not read the deployment {}",
                deployment.name
            )));
        }
        Ok(read_outputs(&parse(&shown, "the deployment's record")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_id_splits_into_type_and_name() {
        let group = "/subscriptions/0000/resourceGroups/web-rg";
        for (id, want) in [
            (
                format!("{group}/providers/Microsoft.Network/virtualNetworks/web-vnet"),
                Some(("Microsoft.Network/virtualNetworks", "web-vnet")),
            ),
            (
                format!("{group}/providers/Microsoft.Sql/servers/a/databases/b"),
                Some(("Microsoft.Sql/servers/databases", "a/b")),
            ),
            // A lock on a storage account: the last provider is the lock's.
            (
                format!("{group}/providers/Microsoft.Storage/storageAccounts/st/providers/Microsoft.Authorization/locks/keep"),
                Some(("Microsoft.Authorization/locks", "keep")),
            ),
            (group.to_owned(), Some(("Microsoft.Resources/resourceGroups", "web-rg"))),
            (format!("{group}/providers/Microsoft.Sql/servers"), None),
            (format!("{group}/providers/Microsoft.Sql"), None),
            ("/subscriptions/0000".to_owned(), None),
            (String::new(), None),
        ] {
            let got = type_and_name(&id);
            assert_eq!(
                got.as_ref().map(|(t, n)| (t.as_str(), n.as_str())),
                want,
                "{id}"
            );
        }
    }

    #[test]
    fn an_error_reads_as_its_code_and_message() {
        let error = decode_value(br#"{"code":"InvalidTemplate","message":"no"}"#).unwrap();
        assert_eq!(error_text(&error), "InvalidTemplate: no");
        let error = decode_value(br#"{"message":"no"}"#).unwrap();
        assert_eq!(error_text(&error), "no");
        let error = decode_value(br#"{"other":1}"#).unwrap();
        assert_eq!(error_text(&error), r#"{"other":1}"#);
    }
}
