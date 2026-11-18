//! The half that deploys a template with the `aws` command line. It sits
//! outside the compile chain: nothing here is reached by a compile.
//!
//! Both a plan and an apply go through a change set, CloudFormation's own
//! preview. A plan creates one, reads it and deletes it. An apply creates
//! one, reads it and executes it, so what is applied is what was shown.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use meridian_sdk::tool::{write_artifact, Finished, Tool};
use meridian_sdk::{
    canonical_map, decode_map, decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog,
    Operations, Output, Plan, RunContext, RunError, Runner, SavedPlan, SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::{STACK_PATH, TEMPLATE_PATH};

/// Names the program to run, when it is not `aws` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_AWS";
/// Extra arguments for every call, split like a shell command line:
/// `--profile prod`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_AWS_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "aws",
            "The AWS CLI program to run, when it is not `aws` on `PATH`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every `aws` call, split like a shell command line: `--profile prod --region eu-west-1`.",
        ),
    ]
}

/// Deploys a compiled template as a stack.
pub struct CloudFormationRunner {
    pub program: String,
    /// Arguments added to every call, after Meridian's own.
    pub extra_args: Vec<String>,
}

/// What `stack.json` says about the deployment.
struct Stack {
    name: String,
    region: String,
    capabilities: Vec<String>,
    /// Logical id to the id the author wrote.
    resources: BTreeMap<String, String>,
    outputs: BTreeMap<String, String>,
}

impl Stack {
    fn read(text: &str) -> Result<Stack, RunError> {
        let bad = || RunError::new(format!("{STACK_PATH} is not one this plugin compiled"));
        let value = decode_value(text.as_bytes()).map_err(|_| bad())?;
        let map = value.as_map().ok_or_else(bad)?;
        let string = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let names = |key: &str| -> BTreeMap<String, String> {
            map.get(key)
                .and_then(Value::as_map)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default()
        };
        let stack = Stack {
            name: string("name"),
            region: string("region"),
            capabilities: map
                .get("capabilities")
                .and_then(Value::as_list)
                .unwrap_or(&[])
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            resources: names("resources"),
            outputs: names("outputs"),
        };
        if stack.name.is_empty() {
            return Err(bad());
        }
        Ok(stack)
    }
}

/// A change set that has been created and read.
struct Preview {
    change_set: String,
    /// The stack did not exist before the change set was created.
    new_stack: bool,
    changes: Vec<Change>,
}

impl Preview {
    /// The change set's name and kind, as a saved plan keeps them. The
    /// changes are not kept here: the host hands them back with the plan.
    fn to_json(&self) -> String {
        let mut map: BTreeMap<String, Value> = BTreeMap::new();
        map.insert(
            "changeSet".to_owned(),
            Value::from(self.change_set.as_str()),
        );
        map.insert("newStack".to_owned(), Value::Bool(self.new_stack));
        canonical_map(&map)
    }

    fn from_saved(saved: &SavedPlan) -> Result<Preview, RunError> {
        let map = decode_map(&saved.data).map_err(|e| {
            RunError::new(format!("the saved plan is not one this plugin saved: {e}"))
        })?;
        let change_set = map
            .get("changeSet")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| RunError::new("the saved plan names no change set"))?
            .to_owned();
        Ok(Preview {
            change_set,
            new_stack: matches!(map.get("newStack"), Some(Value::Bool(true))),
            changes: saved.changes.clone(),
        })
    }
}

impl CloudFormationRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        CloudFormationRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
        }
    }

    /// One `aws cloudformation` call against the stack. A waiter's name is
    /// part of the operation: `wait stack-create-complete`.
    fn aws(&self, context: &RunContext, stack: &Stack, operation: &str) -> Tool {
        let mut tool = Tool::new(&self.program)
            .arg("cloudformation")
            .args(operation.split(' '))
            .args(["--stack-name", &stack.name])
            .dir(&context.work_dir)
            .env("AWS_PAGER", "")
            .capture_stdout();
        if !stack.region.is_empty() {
            tool = tool.args(["--region", &stack.region]);
        }
        tool.args(self.extra_args.iter().cloned())
    }

    /// The stack's status, or `None` when there is no such stack.
    fn status(&self, context: &RunContext, stack: &Stack) -> Result<Option<Value>, RunError> {
        let finished = self
            .aws(context, stack, "describe-stacks")
            .args(["--output", "json"])
            .run(&NoLog)?;
        if !finished.success() {
            if finished.stderr.contains("does not exist") {
                return Ok(None);
            }
            return Err(finished.into_error("aws could not describe the stack"));
        }
        let described = parse(&finished, "the stack's description")?;
        Ok(described
            .as_map()
            .and_then(|m| m.get("Stacks"))
            .and_then(Value::as_list)
            .and_then(|stacks| stacks.first())
            .cloned())
    }

    /// Whether the stack exists only as the shell a change set leaves.
    fn in_review(&self, context: &RunContext, stack: &Stack) -> bool {
        self.status(context, stack).ok().flatten().is_some_and(|s| {
            s.as_map()
                .and_then(|s| s.get("StackStatus"))
                .and_then(Value::as_str)
                == Some("REVIEW_IN_PROGRESS")
        })
    }

    fn preview(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<(Stack, Preview), RunError> {
        let (Some(_), Some(settings)) = (
            artifact.files.get(TEMPLATE_PATH),
            artifact.files.get(STACK_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {TEMPLATE_PATH} and {STACK_PATH}; it was not compiled by this plugin"
            )));
        };
        let stack = Stack::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;
        // Secrets reach CloudFormation as the values of NoEcho parameters,
        // read from a file only the user can read and named on the command
        // line by its path alone. The file lives until the change set has
        // been created; the change set keeps the values, as CloudFormation
        // keeps every parameter, and shows `****` for them.
        let secrets = secrets_file(context)?;

        let existing = self.status(context, &stack)?;
        let status = existing
            .as_ref()
            .and_then(Value::as_map)
            .and_then(|s| s.get("StackStatus"))
            .and_then(Value::as_str)
            .unwrap_or("");
        // A stack left in review by an earlier preview has never been
        // created, so the change set that creates it is still a CREATE.
        let new_stack = existing.is_none() || status == "REVIEW_IN_PROGRESS";
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let change_set = format!("meridian-{seconds}");

        log.out(&format!(
            "stack {}: asking CloudFormation what would change",
            stack.name
        ));
        let mut create = self
            .aws(context, &stack, "create-change-set")
            .args(["--change-set-name", &change_set])
            .args([
                "--change-set-type",
                if new_stack { "CREATE" } else { "UPDATE" },
            ])
            .args(["--template-body", &format!("file://{TEMPLATE_PATH}")]);
        if let Some(file) = &secrets {
            create = create.args(["--parameters", &format!("file://{}", file.path().display())]);
        }
        if !stack.capabilities.is_empty() {
            create = create
                .arg("--capabilities")
                .args(stack.capabilities.iter().cloned());
        }
        let created = create.run_ok("aws could not create a change set", log);
        drop(secrets);
        if let Err(refused) = created {
            // A refusal can still leave the shell of a stack that was never
            // created. One that exists only in review is ours to remove.
            if new_stack && self.in_review(context, &stack) {
                let _ = self.aws(context, &stack, "delete-stack").run(&NoLog);
            }
            return Err(refused);
        }

        // The wait fails when the change set does; the description says why.
        let _ = self
            .aws(context, &stack, "wait change-set-create-complete")
            .args(["--change-set-name", &change_set])
            .run(&NoLog)?;
        let described = self
            .aws(context, &stack, "describe-change-set")
            .args(["--change-set-name", &change_set])
            .args(["--output", "json"])
            .run_ok("aws could not describe the change set", log)?;
        let description = parse(&described, "the change set")?;
        let field = |key: &str| {
            description
                .as_map()
                .and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
        };

        let preview = Preview {
            change_set,
            new_stack,
            changes: read_changes(&description, &stack.resources),
        };
        if field("Status") == "FAILED" {
            let reason = field("StatusReason");
            let nothing_to_do = reason.contains("didn't contain changes")
                || reason.contains("No updates are to be performed");
            if !nothing_to_do {
                let reason = reason.to_owned();
                self.discard(context, &stack, &preview);
                return Err(RunError::new(format!(
                    "CloudFormation rejected the change set: {reason}"
                )));
            }
        }
        Ok((stack, preview))
    }

    /// Removes a change set that will not be executed, and the empty shell of
    /// a stack that creating it left behind.
    fn discard(&self, context: &RunContext, stack: &Stack, preview: &Preview) {
        let _ = self
            .aws(context, stack, "delete-change-set")
            .args(["--change-set-name", &preview.change_set])
            .run(&NoLog);
        if preview.new_stack {
            let _ = self.aws(context, stack, "delete-stack").run(&NoLog);
        }
    }

    fn read_outputs(&self, context: &RunContext, stack: &Stack) -> Result<Vec<Output>, RunError> {
        let Some(described) = self.status(context, stack)? else {
            return Err(RunError::new(format!(
                "there is no stack named {}, so it has no outputs",
                stack.name
            )));
        };
        let outputs = described
            .as_map()
            .and_then(|s| s.get("Outputs"))
            .and_then(Value::as_list)
            .unwrap_or(&[]);
        Ok(outputs
            .iter()
            .filter_map(|output| {
                let output = output.as_map()?;
                let key = output.get("OutputKey")?.as_str()?;
                Some(Output {
                    name: stack.outputs.get(key).cloned().unwrap_or(key.to_owned()),
                    value: output.get("OutputValue").cloned().unwrap_or(Value::Null),
                    sensitive: false,
                })
            })
            .collect())
    }
}

/// The run's secrets as a CloudFormation parameters file, `[{"ParameterKey":
/// ..., "ParameterValue": ...}]`, in a private file; nothing when the run
/// has none.
fn secrets_file(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
    if context.secrets.is_empty() {
        return Ok(None);
    }
    context
        .secrets
        .json_file_shaped("[", ",", "]\n", "parameters.json", |name| {
            (
                format!(
                    "{{\"ParameterKey\":{},\"ParameterValue\":\"",
                    Value::String(crate::secret_parameter(name).unwrap_or_default()).canonical()
                ),
                "\"}".to_owned(),
            )
        })
        .map(Some)
        .map_err(|e| RunError::new(format!("writing the parameters file: {e}")))
}

fn parse(finished: &Finished, what: &str) -> Result<Value, RunError> {
    decode_value(finished.stdout.trim().as_bytes())
        .map_err(|e| RunError::new(format!("{what} could not be read: {e}")))
}

/// Reads the changes of a described change set.
fn read_changes(description: &Value, resources: &BTreeMap<String, String>) -> Vec<Change> {
    let changes = description
        .as_map()
        .and_then(|m| m.get("Changes"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    changes
        .iter()
        .filter_map(|change| {
            let change = change.as_map()?.get("ResourceChange")?.as_map()?;
            let text = |key: &str| change.get(key).and_then(Value::as_str).unwrap_or("");
            let mut summary = text("ResourceType").to_owned();
            let action = match (text("Action"), text("Replacement")) {
                ("Add", _) => ChangeAction::Create,
                ("Import", _) => {
                    summary.push_str(", imported");
                    ChangeAction::Create
                }
                ("Remove", _) => ChangeAction::Delete,
                ("Modify", "True") => ChangeAction::Replace,
                ("Modify", "Conditional") => {
                    summary.push_str(", may be replaced");
                    ChangeAction::Update
                }
                ("Modify", _) => ChangeAction::Update,
                _ => return None,
            };
            let logical = text("LogicalResourceId");
            Some(Change {
                resource: resources
                    .get(logical)
                    .cloned()
                    .unwrap_or(logical.to_owned()),
                action,
                summary,
            })
        })
        .collect()
}

impl Runner for CloudFormationRunner {
    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn saves_plans(&self) -> bool {
        true
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let (stack, preview) = self.preview(context, artifact, log)?;
        // The change set is CloudFormation's own saved plan: it stays, and
        // its name is what `meridian plan --out` keeps. The values it holds
        // stay with CloudFormation, so the name can be kept anywhere.
        let saved = if context.save_plan {
            log.out(&format!(
                "stack {}: keeping the change set {} for `meridian apply --plan`",
                stack.name, preview.change_set
            ));
            Some(SavedPlan::new(preview.to_json()))
        } else {
            self.discard(context, &stack, &preview);
            None
        };
        Ok(Plan {
            changes: preview.changes,
            unknown: false,
            saved,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let (stack, preview) = match &context.saved_plan {
            // The change set a plan kept is executed as it is. CloudFormation
            // refuses one whose stack has been changed since.
            Some(saved) => {
                let Some(settings) = artifact.files.get(STACK_PATH) else {
                    return Err(RunError::new(format!(
                        "the artifact has no {STACK_PATH}; it was not compiled by this plugin"
                    )));
                };
                let stack = Stack::read(settings)?;
                write_artifact(&context.work_dir, artifact)?;
                let preview = Preview::from_saved(saved)?;
                log.out(&format!(
                    "stack {}: executing the change set {} that `meridian plan --out` kept",
                    stack.name, preview.change_set
                ));
                (stack, preview)
            }
            None => self.preview(context, artifact, log)?,
        };
        if preview.changes.is_empty() {
            self.discard(context, &stack, &preview);
            log.out(&format!("stack {}: nothing to change", stack.name));
        } else {
            log.out(&format!(
                "stack {}: applying {} changes",
                stack.name,
                preview.changes.len()
            ));
            self.aws(context, &stack, "execute-change-set")
                .args(["--change-set-name", &preview.change_set])
                .run_ok("aws could not execute the change set", log)?;
            let wait = if preview.new_stack {
                "wait stack-create-complete"
            } else {
                "wait stack-update-complete"
            };
            self.aws(context, &stack, wait).run_ok(
                "the stack did not reach a complete state; CloudFormation rolls a failed \
                 deployment back, and the stack's events say which resource failed",
                log,
            )?;
        }
        Ok(Applied {
            changes: preview.changes,
            outputs: self.read_outputs(context, &stack)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let settings =
            std::fs::read_to_string(context.work_dir.join(STACK_PATH)).map_err(|_| {
                RunError::new(
                    "nothing has been applied for this document yet, so it has no outputs",
                )
            })?;
        self.read_outputs(context, &Stack::read(&settings)?)
    }
}
