//! The half that hands a program to `pulumi`. It sits outside the compile
//! chain: nothing here is reached by a compile.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::tool::{write_artifact, Finished, Tool};
use meridian_sdk::{
    cloud_for, decode_value, Applied, Artifact, Change, ChangeAction, CloudHandover, Log,
    Operations, Output, Plan, RunContext, RunError, Runner, SavedPlan, Value,
};

use crate::{PROGRAM_PATH, STACK_PATH};
use meridian_sdk::{settings, Setting};

/// What `--tool-debug` adds to the tool, level 1 first. Pulumi's log goes to standard error.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["-v=3", "--logtostderr"]),
    meridian_sdk::debug::DebugLevel::args(&["-v=6", "--logtostderr"]),
    meridian_sdk::debug::DebugLevel::args(&["-v=9", "--logtostderr"]),
];

/// Where Pulumi's own saved plan is written while a run uses it. Pulumi
/// calls these update plans; they are behind `PULUMI_EXPERIMENTAL`.
const PLAN_FILE: &str = "meridian.plan.json";

/// Names the program to run, when it is not `pulumi` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_PULUMI";
/// Extra arguments for `pulumi preview`, `pulumi up` and `pulumi destroy`, split like a shell
/// command line: `--parallel 4`, `--diff`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_PULUMI_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "pulumi",
            "The `pulumi` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for `pulumi preview`, `pulumi up` and `pulumi destroy`, split like a shell command line: `--parallel 4`. The calls that select the stack or read its outputs get none. Pulumi's own `PULUMI_BACKEND_URL` and `PULUMI_CONFIG_PASSPHRASE` are read as Pulumi reads them.",
        ),
    ]
}

/// Pulumi's own variable for the backend that keeps a stack's state.
const BACKEND_VARIABLE: &str = "PULUMI_BACKEND_URL";
/// Pulumi's own variables for the passphrase that protects a stack's secrets
/// when no secrets service does.
const PASSPHRASE_VARIABLES: [&str; 2] =
    ["PULUMI_CONFIG_PASSPHRASE", "PULUMI_CONFIG_PASSPHRASE_FILE"];

/// What Pulumi prints in place of a secret output's value.
const MASKED: &str = "[secret]";

/// Runs a compiled program.
///
/// The work directory is the Pulumi project directory: the program is written
/// there, and Pulumi keeps the stack's settings file beside it. Unless the
/// document or the operator says where state lives, it is kept there too.
pub struct PulumiRunner {
    /// The program to run.
    pub program: String,
    /// Arguments added to `preview` and `up`, after Pulumi's own.
    pub extra_args: Vec<String>,
    /// The operator chose a backend through `PULUMI_BACKEND_URL`, so the
    /// runner must not choose one.
    pub backend_chosen: bool,
    /// The operator supplied a passphrase, so the runner must not set an
    /// empty one.
    pub passphrase_chosen: bool,
}

/// The settings a compile wrote beside the program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Stack {
    name: String,
    backend: String,
    encryption: String,
}

impl Stack {
    fn read(json: &str) -> Result<Stack, RunError> {
        let bad = || {
            RunError::new(format!(
                "{STACK_PATH} could not be read; it was not compiled by this plugin"
            ))
        };
        let value = decode_value(json.as_bytes()).map_err(|_| bad())?;
        let map = value.as_map().ok_or_else(bad)?;
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let stack = Stack {
            name: text("stack"),
            backend: text("backend"),
            encryption: text("secretsProvider"),
        };
        if stack.name.is_empty() {
            return Err(bad());
        }
        Ok(stack)
    }
}

fn set(variable: &str) -> bool {
    std::env::var_os(variable).is_some_and(|v| !v.is_empty())
}

impl PulumiRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        PulumiRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
            backend_chosen: set(BACKEND_VARIABLE),
            // A passphrase that is set but empty is still a choice.
            passphrase_chosen: PASSPHRASE_VARIABLES
                .iter()
                .any(|v| std::env::var_os(v).is_some()),
        }
    }

    /// One `pulumi` call in the project directory.
    fn pulumi(&self, context: &RunContext, cloud: Option<&CloudHandover>, stack: &Stack) -> Tool {
        let mut tool = Tool::new(&self.program)
            .dir(&context.work_dir)
            .env("PULUMI_SKIP_UPDATE_CHECK", "true")
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level));
        if stack.backend.is_empty() && !self.backend_chosen {
            // Nobody said where state lives, so it lives with the project,
            // as Terraform's does: no account and no login are needed.
            tool = tool.env(BACKEND_VARIABLE, local_backend(&context.work_dir));
        }
        if !self.passphrase_chosen {
            // Without a passphrase Pulumi stops to ask for one. An empty one
            // protects nothing, which is what a state file kept in the open
            // amounts to anyway; an operator who wants more sets their own.
            tool = tool.env(PASSPHRASE_VARIABLES[0], "");
        }
        CloudHandover::tell_if_any(cloud, tool)
    }

    /// A call that acts on the stack.
    fn on_stack(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        stack: &Stack,
        command: &[&str],
    ) -> Tool {
        self.pulumi(context, cloud, stack)
            .args(command.iter().copied())
            .args(["--stack", &stack.name])
            .args(["--non-interactive", "--color", "never"])
    }

    /// Values handed over from earlier stages reach the program as
    /// configuration, for a document that declares them. A value is never
    /// shown: it may be a secret another tool produced.
    fn with_inputs(mut tool: Tool, context: &RunContext) -> Tool {
        for (name, value) in &context.inputs {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.canonical(),
            };
            tool = tool.arg("--config").secret_arg(format!("{name}={text}"));
        }
        tool
    }

    /// Hands the run's secrets to the stack: each becomes a secret
    /// configuration value, set by `pulumi config set --secret` reading the
    /// value from standard input, so it is on no command line and in no
    /// environment. Pulumi encrypts it with the stack's secrets provider
    /// before writing it to the stack's settings file and its state, and
    /// prints `[secret]` wherever it would show it.
    fn set_secrets(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        stack: &Stack,
        log: &dyn Log,
    ) -> Result<(), RunError> {
        for (name, value) in context.secrets.iter() {
            let value = value
                .duplicate()
                .map_err(|e| RunError::new(format!("copying the secret {name:?}: {e}")))?;
            let key = crate::secret_key(name);
            self.on_stack(context, cloud, stack, &["config", "set", "--secret", &key])
                .stdin_secret(value)
                .run_ok("pulumi could not take a secret as configuration", log)?;
        }
        Ok(())
    }

    /// Takes the secrets out of the stack's settings file again once the
    /// stage is over, so that what was encrypted for one run does not stay
    /// behind for the next: with no passphrase chosen the encryption
    /// protects nothing (see `pulumi`). The state keeps what the resources
    /// were given, as Pulumi's state always does, encrypted the same way.
    fn forget_secrets(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        stack: &Stack,
        log: &dyn Log,
    ) {
        for name in context.secrets.names() {
            let key = crate::secret_key(name);
            let _ = self
                .on_stack(context, cloud, stack, &["config", "rm", &key])
                .run(log);
        }
    }

    /// Writes the program, makes sure the stack exists and hands it the
    /// run's secrets.
    fn select(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Stack, RunError> {
        let (Some(_), Some(settings)) = (
            artifact.files.get(PROGRAM_PATH),
            artifact.files.get(STACK_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {PROGRAM_PATH} and {STACK_PATH}; it was not compiled by \
                 this plugin"
            )));
        };
        let stack = Stack::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;

        let mut select = self
            .pulumi(context, cloud, &stack)
            .args(["stack", "select", &stack.name, "--create"])
            .args(["--non-interactive", "--color", "never"]);
        if !stack.encryption.is_empty() {
            select = select.args(["--secrets-provider", &stack.encryption]);
        }
        select.run_ok("pulumi could not select or create the stack", log)?;
        self.set_secrets(context, cloud, &stack, log)?;
        Ok(stack)
    }

    /// Writes the program, makes sure the stack exists and asks Pulumi what
    /// an update would change, saving the update plan to [`PLAN_FILE`] when
    /// `save` says to.
    fn prepare(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        artifact: &Artifact,
        log: &dyn Log,
        save: bool,
    ) -> Result<(Stack, Vec<Change>), RunError> {
        let stack = self.select(context, cloud, artifact, log)?;

        log.out(&format!(
            "stack {}: asking Pulumi what would change",
            stack.name
        ));
        let mut preview = self
            .on_stack(context, cloud, &stack, &["preview"])
            .args(self.extra_args.iter().cloned())
            .arg("--json");
        if save {
            preview = preview
                .args(["--save-plan", PLAN_FILE])
                .env("PULUMI_EXPERIMENTAL", "true");
        }
        let preview = Self::with_inputs(preview, context)
            .capture_stdout()
            .run(log);
        let preview = match preview {
            Ok(preview) => preview,
            Err(e) => {
                self.forget_secrets(context, cloud, &stack, log);
                return Err(e);
            }
        };
        // With --json everything Pulumi has to say is in the document on
        // standard output, including why a preview failed.
        let read = read_preview(&preview.stdout);
        if let Ok(preview) = &read {
            for diagnostic in &preview.diagnostics {
                log.err(diagnostic);
            }
        }
        if !preview.success() {
            // What went wrong is among the diagnostics when Pulumi got far
            // enough to write them. When it did not, or wrote none, whatever
            // it printed instead is the only account there is.
            let said = match read {
                Ok(preview) if !preview.diagnostics.is_empty() => preview.diagnostics.join("\n"),
                _ => tail(&preview.stdout).to_owned(),
            };
            let failures = read_preview_failures(&preview.stdout);
            let mut err = preview.into_error("pulumi preview failed");
            err.failures = failures;
            if !said.trim().is_empty() {
                if !err.output.is_empty() && !err.output.ends_with('\n') {
                    err.output.push('\n');
                }
                err.output.push_str(said.trim_end());
                err.output.push('\n');
            }
            self.forget_secrets(context, cloud, &stack, log);
            return Err(err);
        }
        match read {
            Ok(preview) => Ok((stack, preview.changes)),
            Err(e) => {
                self.forget_secrets(context, cloud, &stack, log);
                Err(e)
            }
        }
    }

    /// What a destroy of the stack would remove, from `pulumi stack export`.
    /// The export holds secrets encrypted, never in the clear.
    fn removals(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        stack: &Stack,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let exported = self
            .on_stack(context, cloud, stack, &["stack", "export"])
            .capture_stdout()
            .run_ok("pulumi could not export the stack", log)?;
        read_removals(&exported.stdout)
    }

    fn read_outputs(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        stack: &Stack,
        log: &dyn Log,
    ) -> Result<Vec<Output>, RunError> {
        // Asked twice: once as Pulumi would show them, with secrets masked,
        // and once in full. Whatever differs between the two is a secret.
        let masked = self
            .on_stack(context, cloud, stack, &["stack", "output"])
            .arg("--json")
            .capture_stdout()
            .run_ok("pulumi could not report the stack's outputs", log)?;
        let shown = self
            .on_stack(context, cloud, stack, &["stack", "output"])
            .args(["--json", "--show-secrets"])
            .capture_stdout()
            .run_ok("pulumi could not report the stack's outputs", log)?;
        read_outputs(&masked.stdout, &shown.stdout)
    }
}

impl Runner for PulumiRunner {
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn accepts_inputs(&self) -> bool {
        true
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
        // A cloud credential reaches the tool in the environment of every
        // call of the stage; the files a provider reads live to the end of it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        // Pulumi's update plan holds the inputs of every resource, secrets
        // among them encrypted with the stack's provider, which with no
        // passphrase chosen protects nothing; so a run that handed over a
        // secret saves no plan, and the apply from the plan file previews
        // again.
        let save = context.save_plan && context.secrets.is_empty();
        let (stack, changes) = self.prepare(context, cloud, artifact, log, save)?;
        self.forget_secrets(context, cloud, &stack, log);
        let plan_file = context.work_dir.join(PLAN_FILE);
        let saved = if save {
            let bytes = std::fs::read(&plan_file).map_err(|e| {
                RunError::new(format!(
                    "the update plan {PLAN_FILE} could not be read back: {e}"
                ))
            })?;
            Some(SavedPlan::new(bytes))
        } else {
            None
        };
        let _ = std::fs::remove_file(&plan_file);
        Ok(Plan {
            changes,
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
        // A cloud credential reaches the tool in the environment of every
        // call of the stage; the files a provider reads live to the end of it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        let (stack, changes, planned) = match &context.saved_plan {
            // The update plan `meridian plan --out` kept constrains the
            // update to what was previewed; Pulumi refuses one that no
            // longer matches. There is no second preview.
            Some(saved) => {
                let stack = self.select(context, cloud, artifact, log)?;
                std::fs::write(context.work_dir.join(PLAN_FILE), &saved.data).map_err(|e| {
                    RunError::new(format!("writing the update plan {PLAN_FILE}: {e}"))
                })?;
                log.out(&format!(
                    "stack {}: updating with the plan saved by `meridian plan --out`",
                    stack.name
                ));
                (stack, saved.changes.clone(), true)
            }
            None => {
                let (stack, changes) = self.prepare(context, cloud, artifact, log, false)?;
                (stack, changes, false)
            }
        };
        // The preview above is the report; the update does not print a
        // second one.
        let mut up = self
            .on_stack(context, cloud, &stack, &["up"])
            .args(self.extra_args.iter().cloned())
            .args(["--yes", "--skip-preview"]);
        if planned {
            up = up
                .args(["--plan", PLAN_FILE])
                .env("PULUMI_EXPERIMENTAL", "true");
        }
        let updated = run_naming_failures(&Self::with_inputs(up, context), "pulumi up failed", log);
        self.forget_secrets(context, cloud, &stack, log);
        let _ = std::fs::remove_file(context.work_dir.join(PLAN_FILE));
        updated?;
        Ok(Applied {
            changes,
            outputs: self.read_outputs(context, cloud, &stack, log)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        // A cloud credential reaches the tool in the environment of every
        // call of the stage; the files a provider reads live to the end of it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        let Ok(settings) = std::fs::read_to_string(context.work_dir.join(STACK_PATH)) else {
            return Err(RunError::new(
                "nothing has been applied for this document yet, so it has no outputs",
            ));
        };
        let stack = Stack::read(&settings)?;
        self.read_outputs(context, cloud, &stack, &meridian_sdk::NoLog)
    }

    fn reads_identifiers(&self) -> bool {
        true
    }

    fn identifiers(&self, context: &RunContext) -> Result<BTreeMap<String, String>, RunError> {
        let Ok(settings) = std::fs::read_to_string(context.work_dir.join(STACK_PATH)) else {
            return Err(RunError::new(
                "nothing has been applied for this document yet, so there is nothing to read",
            ));
        };
        let stack = Stack::read(&settings)?;
        // The export holds secrets encrypted, never in the clear; only the
        // ids are read from it.
        // A backend in the cloud is read as the document's cloud credential,
        // the way a preview reads it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let exported = self
            .on_stack(context, cloud.as_ref(), &stack, &["stack", "export"])
            .capture_stdout()
            .run_ok("pulumi could not export the stack", &meridian_sdk::NoLog)?;
        read_identifiers(&exported.stdout)
    }

    fn inspects(&self) -> bool {
        true
    }

    /// The resource's outputs as `pulumi stack export` records them; a
    /// secret is left as what stands for one. Pulumi records what was
    /// applied, so whether that is still the cloud's is not asked.
    fn inspect(
        &self,
        context: &RunContext,
        _artifact: &Artifact,
        resource: &str,
        _endpoint: &str,
    ) -> Result<meridian_sdk::Inspection, RunError> {
        let command = "pulumi stack export".to_owned();
        let Ok(settings) = std::fs::read_to_string(context.work_dir.join(STACK_PATH)) else {
            return Ok(meridian_sdk::Inspection {
                found: false,
                in_desired_state: Some(false),
                command,
                ..Default::default()
            });
        };
        let stack = Stack::read(&settings)?;
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let exported = self
            .on_stack(context, cloud.as_ref(), &stack, &["stack", "export"])
            .capture_stdout()
            .run_ok("pulumi could not export the stack", &meridian_sdk::NoLog)?;
        let found = read_outputs_of(&exported.stdout, resource)?;
        Ok(meridian_sdk::Inspection {
            found: found.is_some(),
            in_desired_state: if found.is_some() { None } else { Some(false) },
            actual: found.unwrap_or_default(),
            command,
        })
    }

    fn destroys(&self) -> bool {
        true
    }

    fn plan_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        let stack = self.select(context, cloud, artifact, log)?;
        let removals = self.removals(context, cloud, &stack, log);
        self.forget_secrets(context, cloud, &stack, log);
        Ok(Plan {
            changes: removals?,
            unknown: false,
            saved: None,
        })
    }

    /// `pulumi destroy` on the stack. The stack and its history are kept;
    /// only its resources go.
    fn destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        let stack = self.select(context, cloud, artifact, log)?;
        let removals = match self.removals(context, cloud, &stack, log) {
            Ok(removals) => removals,
            Err(e) => {
                self.forget_secrets(context, cloud, &stack, log);
                return Err(e);
            }
        };
        log.out(&format!("stack {}: destroying its resources", stack.name));
        let destroy = self
            .on_stack(context, cloud, &stack, &["destroy"])
            .args(self.extra_args.iter().cloned())
            .args(["--yes", "--skip-preview"]);
        let destroyed = run_naming_failures(&destroy, "pulumi destroy failed", log);
        self.forget_secrets(context, cloud, &stack, log);
        destroyed?;
        Ok(removals)
    }
}

/// One resource of the document in `pulumi stack export`.
struct Exported {
    /// The resource's id in the document.
    name: String,
    r#type: String,
    /// The cloud's id for it; empty until the cloud gave one.
    id: String,
}

/// Reads `pulumi stack export` into the resources of the document. The
/// stack itself and providers are left out: they are not resources of the
/// document.
fn read_export(json: &str) -> Result<Vec<Exported>, RunError> {
    let export = decode_value(json.as_bytes()).map_err(|e| unreadable("stack export", e))?;
    let resources = export
        .as_map()
        .and_then(|m| m.get("deployment"))
        .and_then(Value::as_map)
        .and_then(|m| m.get("resources"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    let mut found = Vec::new();
    for resource in resources {
        let Some(resource) = resource.as_map() else {
            continue;
        };
        let text = |key: &str| resource.get(key).and_then(Value::as_str).unwrap_or("");
        let urn = text("urn");
        let kind = urn_type(urn);
        if kind == "pulumi:pulumi:Stack" || kind.starts_with("pulumi:providers:") {
            continue;
        }
        let r#type = match text("type") {
            "" => kind,
            given => given,
        };
        found.push(Exported {
            name: urn_name(urn).to_owned(),
            r#type: r#type.to_owned(),
            id: text("id").to_owned(),
        });
    }
    Ok(found)
}

/// The outputs `pulumi stack export` records for the resource named
/// `name`, with each secret written as [`meridian_sdk::SENSITIVE`]; `None`
/// when the stack has no such resource.
fn read_outputs_of(json: &str, name: &str) -> Result<Option<meridian_sdk::Data>, RunError> {
    let export = decode_value(json.as_bytes()).map_err(|e| unreadable("stack export", e))?;
    let resources = export
        .as_map()
        .and_then(|m| m.get("deployment"))
        .and_then(Value::as_map)
        .and_then(|m| m.get("resources"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    for resource in resources.iter().filter_map(Value::as_map) {
        let urn = resource.get("urn").and_then(Value::as_str).unwrap_or("");
        let kind = urn_type(urn);
        if urn_name(urn) != name
            || kind == "pulumi:pulumi:Stack"
            || kind.starts_with("pulumi:providers:")
        {
            continue;
        }
        let outputs = resource
            .get("outputs")
            .and_then(Value::as_map)
            .cloned()
            .unwrap_or_default();
        return Ok(Some(
            outputs
                .into_iter()
                .map(|(k, v)| (k, hide_secrets(v)))
                .collect(),
        ));
    }
    Ok(None)
}

/// Pulumi's mark for a secret in an export.
const SECRET_SIG: &str = "4dabf18193072939515e22adb298388d";

fn hide_secrets(value: Value) -> Value {
    match value {
        Value::Map(map) if map.contains_key(SECRET_SIG) => Value::from(meridian_sdk::SENSITIVE),
        Value::Map(map) => Value::Map(map.into_iter().map(|(k, v)| (k, hide_secrets(v))).collect()),
        Value::List(items) => Value::List(items.into_iter().map(hide_secrets).collect()),
        other => other,
    }
}

/// Reads `pulumi stack export` into the cloud id of each resource of the
/// document, by the resource's id.
fn read_identifiers(json: &str) -> Result<BTreeMap<String, String>, RunError> {
    Ok(read_export(json)?
        .into_iter()
        .filter(|r| !r.id.is_empty())
        .map(|r| (r.name, r.id))
        .collect())
}

/// What a destroy of the stack removes: every resource of the document in
/// `pulumi stack export`, last created first, as Pulumi deletes them.
fn read_removals(json: &str) -> Result<Vec<Change>, RunError> {
    Ok(read_export(json)?
        .into_iter()
        .rev()
        .map(|r| Change {
            resource: r.name,
            action: ChangeAction::Delete,
            summary: r.r#type,
        })
        .collect())
}

/// The URL of a backend that keeps state in the project directory itself.
fn local_backend(work_dir: &Path) -> String {
    // Pulumi resolves a relative path against its own working directory,
    // which is this one, so the absolute path is the one that cannot be
    // misread.
    let dir = meridian_sdk::tool::absolute_path(work_dir);
    format!("file://{}", dir.display().to_string().replace('\\', "/"))
}

/// How much of a failed preview's output a failure carries.
const TAIL_BYTES: usize = 8 * 1024;

/// The end of a text, cut at a line where it can be.
fn tail(text: &str) -> &str {
    if text.len() <= TAIL_BYTES {
        return text;
    }
    let mut start = text.len() - TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    match text[start..].find('\n') {
        Some(newline) => &text[start + newline + 1..],
        None => &text[start..],
    }
}

fn unreadable(what: &str, detail: impl std::fmt::Display) -> RunError {
    RunError::new(format!("pulumi's {what} could not be read: {detail}"))
}

/// Runs `pulumi up` or `pulumi destroy`; a failure carries the resources
/// its `Diagnostics:` section names.
fn run_naming_failures(tool: &Tool, what: &str, log: &dyn Log) -> Result<Finished, RunError> {
    let finished = tool.run(log)?;
    if finished.success() {
        return Ok(finished);
    }
    let failures = read_diagnostics(&finished.stdout);
    let mut err = finished.into_error(what);
    err.failures = failures;
    Err(err)
}

/// One failure per resource with an `error:` in the `Diagnostics:` section
/// Pulumi prints at the end of an update:
///
/// ```text
/// Diagnostics:
///   aws:s3/bucket:Bucket (logs):
///     error: creating S3 Bucket (shop-logs): BucketAlreadyExists
/// ```
///
/// The stack's own entry (`update failed`) names no resource and is left to
/// the step's error.
fn read_diagnostics(stdout: &str) -> Vec<Failure> {
    let mut failures: Vec<Failure> = Vec::new();
    let mut inside = false;
    // The resource being read, its type, and its error so far.
    let mut current: Option<(String, String, String)> = None;
    let mut in_error = false;
    let mut flush = |current: &mut Option<(String, String, String)>| {
        if let Some((name, kind, message)) = current.take() {
            if kind != "pulumi:pulumi:Stack"
                && !message.trim().is_empty()
                && !failures.iter().any(|f| f.resource == name)
            {
                failures.push(diagnostic_failure(
                    &name,
                    &plain(&message),
                    Cause::ResourceFailed,
                ));
            }
        }
    };
    for line in stdout.lines() {
        let line = line.trim_end();
        if line == "Diagnostics:" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if !line.is_empty() && !line.starts_with(' ') {
            inside = false;
            flush(&mut current);
            continue;
        }
        if let Some(header) = line.strip_prefix("  ").filter(|h| !h.starts_with(' ')) {
            flush(&mut current);
            in_error = false;
            current = header
                .strip_suffix("):")
                .and_then(|h| h.split_once(" ("))
                .map(|(kind, name)| (name.to_owned(), kind.to_owned(), String::new()));
            continue;
        }
        let Some((_, _, message)) = current.as_mut() else {
            continue;
        };
        let Some(body) = line.strip_prefix("    ") else {
            continue;
        };
        if let Some(said) = body.strip_prefix("error: ") {
            in_error = true;
            message.push_str(said);
            message.push('\n');
        } else if body.starts_with("warning: ") || body.starts_with("info: ") {
            in_error = false;
        } else if in_error {
            message.push_str(body.trim_start());
            message.push('\n');
        }
    }
    flush(&mut current);
    failures
}

/// One failure per resource an `error` diagnostic of `pulumi preview --json`
/// is about: the program or a provider refused it before anything changed.
fn read_preview_failures(json: &str) -> Vec<Failure> {
    let Ok(preview) = decode_value(json.as_bytes()) else {
        return Vec::new();
    };
    let diagnostics = preview
        .as_map()
        .and_then(|p| p.get("diagnostics"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    // A program that fails to load (a YAML program naming a type its
    // provider lacks) says so on its standard error, which Pulumi passes on
    // as `info#err`.
    let has_error = diagnostics.iter().any(|d| {
        d.as_map()
            .and_then(|d| d.get("severity"))
            .and_then(Value::as_str)
            == Some("error")
    });
    let severity = if has_error { "error" } else { "info#err" };
    let mut failures: Vec<Failure> = Vec::new();
    for diagnostic in diagnostics {
        let Some(diagnostic) = diagnostic.as_map() else {
            continue;
        };
        let text = |key: &str| diagnostic.get(key).and_then(Value::as_str).unwrap_or("");
        if text("severity") != severity {
            continue;
        }
        let urn = text("urn");
        let name = if urn_type(urn) == "pulumi:pulumi:Stack" {
            ""
        } else {
            urn_name(urn)
        };
        if failures.iter().any(|f| f.resource == name) {
            continue;
        }
        failures.push(diagnostic_failure(
            name,
            &plain(text("message")),
            Cause::InvalidConfig,
        ));
    }
    failures
}

fn diagnostic_failure(resource: &str, message: &str, otherwise: Cause) -> Failure {
    let message = message.trim();
    let summary = message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    Failure::new(
        Cause::from_message(message).unwrap_or(otherwise),
        resource,
        summary,
    )
    .detail(message)
}

/// What `pulumi preview --json` reported.
#[derive(Debug, Default, PartialEq, Eq)]
struct Preview {
    changes: Vec<Change>,
    /// What Pulumi had to say, each prefixed with its severity.
    diagnostics: Vec<String>,
}

/// The name of the thing a URN identifies, which for a resource of the
/// document is its id: `urn:pulumi:<stack>::<project>::<type>::<name>`.
fn urn_name(urn: &str) -> &str {
    urn.rsplit_once("::").map_or(urn, |(_, name)| name)
}

/// The type in a URN, without the types of its parents.
fn urn_type(urn: &str) -> &str {
    let qualified = urn.split("::").nth(2).unwrap_or("");
    qualified.rsplit_once('$').map_or(qualified, |(_, t)| t)
}

/// Reads `pulumi preview --json` into the changes it holds.
fn read_preview(json: &str) -> Result<Preview, RunError> {
    let preview = decode_value(json.as_bytes()).map_err(|e| unreadable("preview", e))?;
    let Some(preview) = preview.as_map() else {
        return Err(unreadable("preview", "it is not a JSON object"));
    };

    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    let steps = preview.get("steps").and_then(Value::as_list).unwrap_or(&[]);
    for step in steps {
        let Some(step) = step.as_map() else {
            continue;
        };
        let text = |key: &str| step.get(key).and_then(Value::as_str).unwrap_or("");
        let action = match text("op") {
            "create" => ChangeAction::Create,
            // A resource's adopt, or Pulumi's own import option.
            "import" => ChangeAction::Adopt,
            "update" => ChangeAction::Update,
            "delete" => ChangeAction::Delete,
            "replace" => ChangeAction::Replace,
            // "same", "read" and "refresh" change nothing, and
            // "create-replacement" and "delete-replaced" are the two halves
            // of a "replace" that is reported in its own right.
            _ => continue,
        };
        let urn = text("urn");
        let name = urn_name(urn);
        let state_type = ["newState", "oldState"].iter().find_map(|key| {
            step.get(*key)
                .and_then(Value::as_map)
                .and_then(|state| state.get("type"))
                .and_then(Value::as_str)
        });
        let r#type = state_type.unwrap_or_else(|| urn_type(urn));
        // The stack is Pulumi's own bookkeeping, and so is a provider the
        // document did not declare; neither is something the author manages.
        if r#type == "pulumi:pulumi:Stack"
            || (r#type.starts_with("pulumi:providers:") && name.starts_with("default"))
        {
            continue;
        }
        if !seen.insert(urn.to_owned()) {
            continue;
        }
        changes.push(Change {
            resource: name.to_owned(),
            action,
            summary: r#type.to_owned(),
        });
    }

    let mut diagnostics = Vec::new();
    for diagnostic in preview
        .get("diagnostics")
        .and_then(Value::as_list)
        .unwrap_or(&[])
    {
        let Some(diagnostic) = diagnostic.as_map() else {
            continue;
        };
        let text = |key: &str| diagnostic.get(key).and_then(Value::as_str).unwrap_or("");
        let message = plain(text("message"));
        let message = message.trim();
        let severity = text("severity");
        // "debug" and "info" are a running commentary. Everything else is
        // something going wrong: "warning", "error", and "info#err", which is
        // how a provider's or the program's own error output is labelled.
        if message.is_empty() || matches!(severity, "" | "debug" | "info") {
            continue;
        }
        let about = urn_name(text("urn"));
        diagnostics.push(if about.is_empty() {
            format!("{severity}: {message}")
        } else {
            format!("{severity}: {about}: {message}")
        });
    }
    Ok(Preview {
        changes,
        diagnostics,
    })
}

/// A message without the colour codes Pulumi leaves in a diagnostic even when
/// it is told not to colour its output.
fn plain(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // An escape sequence: ESC, "[", parameters, then one final letter.
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// Reads `pulumi stack output --json`, taken once masked and once in full.
fn read_outputs(masked: &str, shown: &str) -> Result<Vec<Output>, RunError> {
    let read = |json: &str| -> Result<_, RunError> {
        let outputs = decode_value(json.as_bytes()).map_err(|e| unreadable("outputs", e))?;
        match outputs {
            Value::Map(map) => Ok(map),
            // A stack with no outputs may print nothing at all.
            Value::Null => Ok(Default::default()),
            _ => Err(unreadable("outputs", "they are not a JSON object")),
        }
    };
    let masked = if masked.trim().is_empty() {
        Default::default()
    } else {
        read(masked)?
    };
    let shown = if shown.trim().is_empty() {
        Default::default()
    } else {
        read(shown)?
    };
    Ok(shown
        .into_iter()
        .map(|(name, value)| {
            let sensitive = match masked.get(&name) {
                Some(as_shown) => as_shown != &value || holds_mask(as_shown),
                None => true,
            };
            Output {
                name,
                value,
                sensitive,
            }
        })
        .collect())
}

/// Whether a value Pulumi printed has a masked secret anywhere in it.
fn holds_mask(value: &Value) -> bool {
    match value {
        Value::String(s) => s == MASKED,
        Value::List(items) => items.iter().any(holds_mask),
        Value::Map(map) => map.values().any(holds_mask),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_resource_is_read_from_the_export_with_secrets_hidden() {
        let json = r#"{"deployment":{"resources":[
          {"urn":"urn:pulumi:dev::web::pulumi:pulumi:Stack::web-dev","type":"pulumi:pulumi:Stack"},
          {"urn":"urn:pulumi:dev::web::random:index/randomPet:RandomPet::name","id":"keen-cat",
           "outputs":{"length":2,"id":"keen-cat","keepers":{"4dabf18193072939515e22adb298388d":"1b47061264138c4ac30d75fd1eb44270","ciphertext":"v1:abc"}}}
        ]}}"#;
        let read = read_outputs_of(json, "name").unwrap().unwrap();
        assert_eq!(read.get("length"), Some(&Value::Int(2)));
        assert_eq!(
            read.get("keepers"),
            Some(&Value::from(meridian_sdk::SENSITIVE))
        );
        assert_eq!(read_outputs_of(json, "web-dev").unwrap(), None);
        assert_eq!(read_outputs_of(json, "other").unwrap(), None);
    }

    #[test]
    fn an_error_in_the_diagnostics_is_its_resources_failure() {
        let stdout = "Updating (dev):\n\
\x20    Type                 Name     Status\n\
\x20+   pulumi:pulumi:Stack  web-dev  **failed**\n\
\n\
Diagnostics:\n\
\x20 aws:s3/bucket:Bucket (logs):\n\
\x20   warning: deprecated\n\
\x20   error: creating S3 Bucket (shop-logs): BucketAlreadyExists\n\
\x20   \tstatus code: 409\n\
\n\
\x20 pulumi:pulumi:Stack (web-dev):\n\
\x20   error: update failed\n\
\n\
Resources:\n\
\x20   1 unchanged\n";
        let failures = read_diagnostics(stdout);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].resource, "logs");
        assert_eq!(failures[0].cause, Cause::ResourceFailed);
        assert_eq!(
            failures[0].summary,
            "creating S3 Bucket (shop-logs): BucketAlreadyExists"
        );
        assert!(
            failures[0].detail.contains("status code: 409"),
            "{:?}",
            failures[0].detail
        );
    }

    #[test]
    fn a_program_that_fails_to_load_is_the_configurations() {
        let preview = r#"{"steps":[],"diagnostics":[
            {"urn":"urn:pulumi:dev::web::pulumi:pulumi:Stack::web-dev","message":"Error: unable to find resource type \"random:index:NoSuchThing\"","severity":"info#err"}]}"#;
        let failures = read_preview_failures(preview);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].resource, "");
        assert_eq!(failures[0].cause, Cause::InvalidConfig);
    }

    #[test]
    fn an_error_in_a_preview_is_the_configurations() {
        let preview = r#"{"steps":[],"diagnostics":[
            {"urn":"urn:pulumi:dev::web::aws:s3/bucket:Bucket::logs","message":"aws:s3/bucket:Bucket resource 'logs' has a problem: unknown property \"nosuch\"","severity":"error"},
            {"urn":"urn:pulumi:dev::web::pulumi:pulumi:Stack::web-dev","message":"preview failed","severity":"error"},
            {"urn":"","message":"just a warning","severity":"warning"}]}"#;
        let failures = read_preview_failures(preview);
        let got: Vec<(&str, Cause)> = failures
            .iter()
            .map(|f| (f.resource.as_str(), f.cause))
            .collect();
        assert_eq!(
            got,
            [("logs", Cause::InvalidConfig), ("", Cause::InvalidConfig)]
        );
    }

    #[test]
    fn a_preview_is_read_into_changes() {
        let json = r#"{"config":{},"steps":[
          {"op":"create","urn":"urn:pulumi:dev::web-vm::pulumi:pulumi:Stack::web-vm-dev","newState":{"type":"pulumi:pulumi:Stack"}},
          {"op":"create","urn":"urn:pulumi:dev::web-vm::pulumi:providers:aws::default_6_0_0","newState":{"type":"pulumi:providers:aws"}},
          {"op":"create","urn":"urn:pulumi:dev::web-vm::pulumi:providers:aws::west","newState":{"type":"pulumi:providers:aws"}},
          {"op":"create","urn":"urn:pulumi:dev::web-vm::aws:ec2/instance:Instance::web_vm","newState":{"type":"aws:ec2/instance:Instance"}},
          {"op":"replace","urn":"urn:pulumi:dev::web-vm::aws:ec2/eip:Eip::web_ip","oldState":{"type":"aws:ec2/eip:Eip"},"newState":{"type":"aws:ec2/eip:Eip"}},
          {"op":"create-replacement","urn":"urn:pulumi:dev::web-vm::aws:ec2/eip:Eip::web_ip"},
          {"op":"delete-replaced","urn":"urn:pulumi:dev::web-vm::aws:ec2/eip:Eip::web_ip"},
          {"op":"same","urn":"urn:pulumi:dev::web-vm::aws:s3/bucket:Bucket::logs"},
          {"op":"update","urn":"urn:pulumi:dev::web-vm::my:module:Site$aws:s3/bucket:Bucket::site-bucket"},
          {"op":"delete","urn":"urn:pulumi:dev::web-vm::aws:ec2/securityGroup:SecurityGroup::old_sg","oldState":{"type":"aws:ec2/securityGroup:SecurityGroup"}}
        ],"diagnostics":[
          {"message":"using a deprecated property\n","severity":"warning","urn":"urn:pulumi:dev::web-vm::aws:ec2/instance:Instance::web_vm"},
          {"message":"debugging detail","severity":"debug"},
          {"message":"chatter","severity":"info"},
          {"message":"the program wrote to stderr","severity":"info#err"},
          {"message":"no credentials","severity":"error"}
        ],"changeSummary":{"create":2}}"#;
        let preview = read_preview(json).unwrap();
        let shown: Vec<String> = preview.changes.iter().map(Change::to_string).collect();
        assert_eq!(
            shown,
            [
                "create west: pulumi:providers:aws",
                "create web_vm: aws:ec2/instance:Instance",
                "replace web_ip: aws:ec2/eip:Eip",
                "update site-bucket: aws:s3/bucket:Bucket",
                "delete old_sg: aws:ec2/securityGroup:SecurityGroup",
            ]
        );
        assert_eq!(
            preview.diagnostics,
            [
                "warning: web_vm: using a deprecated property",
                "info#err: the program wrote to stderr",
                "error: no credentials"
            ]
        );
        assert_eq!(read_preview("{}").unwrap(), Preview::default());
        assert!(read_preview("not json").is_err());
        assert!(read_preview("[]").is_err());
    }

    #[test]
    fn outputs_keep_their_types_and_whatever_was_masked_is_sensitive() {
        let masked = r#"{"web_vm_ip":"203.0.113.7","db_password":"[secret]","ports":[80,443],
                         "login":{"user":"admin","password":"[secret]"}}"#;
        let shown = r#"{"web_vm_ip":"203.0.113.7","db_password":"hunter2","ports":[80,443],
                        "login":{"user":"admin","password":"hunter3"}}"#;
        let outputs = read_outputs(masked, shown).unwrap();
        assert_eq!(outputs.len(), 4);
        let find = |name: &str| outputs.iter().find(|o| o.name == name).unwrap();
        assert!(find("db_password").sensitive);
        assert!(find("db_password").value == Value::String("hunter2".into()));
        assert!(!format!("{:?}", find("db_password")).contains("hunter2"));
        assert!(find("login").sensitive);
        assert!(!find("web_vm_ip").sensitive);
        assert_eq!(
            find("ports").value,
            Value::List(vec![Value::Int(80), Value::Int(443)])
        );
        assert!(!find("ports").sensitive);
    }

    #[test]
    fn a_secret_that_happens_to_read_as_the_mask_is_still_sensitive() {
        let outputs = read_outputs(r#"{"odd":"[secret]"}"#, r#"{"odd":"[secret]"}"#).unwrap();
        assert!(outputs[0].sensitive);
        // An output the masked report left out is treated as one too.
        let outputs = read_outputs("{}", r#"{"late":"x"}"#).unwrap();
        assert!(outputs[0].sensitive);
    }

    #[test]
    fn a_stack_with_no_outputs_has_none() {
        assert!(read_outputs("{}", "{}").unwrap().is_empty());
        assert!(read_outputs("", "").unwrap().is_empty());
        assert!(read_outputs("{}", "not json").is_err());
    }

    #[test]
    fn colour_codes_are_taken_out_of_a_message() {
        assert_eq!(
            plain("\u{1b}[31mError\u{1b}[0m: type \u{1b}[1;4mrandom:index:Nope\u{1b}[0m"),
            "Error: type random:index:Nope"
        );
        assert_eq!(plain("plain text [not a code]"), "plain text [not a code]");
        assert_eq!(plain("cut short \u{1b}"), "cut short ");
    }

    #[test]
    fn a_long_output_is_cut_to_its_end_at_a_line() {
        assert_eq!(tail("short\n"), "short\n");
        let long = format!("{}\nlast line\n", "x".repeat(TAIL_BYTES));
        assert_eq!(tail(&long), "last line\n");
        let unbroken = "é".repeat(TAIL_BYTES);
        assert!(tail(&unbroken).len() <= TAIL_BYTES);
    }

    #[test]
    fn a_urn_gives_the_name_and_the_type() {
        let urn = "urn:pulumi:dev::web-vm::my:module:Site$aws:s3/bucket:Bucket::site-bucket";
        assert_eq!(urn_name(urn), "site-bucket");
        assert_eq!(urn_type(urn), "aws:s3/bucket:Bucket");
        assert_eq!(urn_name(""), "");
        assert_eq!(urn_type("nonsense"), "");
    }

    #[test]
    fn the_settings_file_names_the_stack() {
        let stack = Stack::read(
            r#"{"project":"web-vm","stack":"prod","backend":"s3://state","secretsProvider":"awskms://alias/x"}"#,
        )
        .unwrap();
        assert_eq!(
            stack,
            Stack {
                name: "prod".into(),
                backend: "s3://state".into(),
                encryption: "awskms://alias/x".into(),
            }
        );
        for bad in ["", "[]", r#"{"stack":""}"#, "{}"] {
            assert!(Stack::read(bad)
                .unwrap_err()
                .msg
                .contains("was not compiled by this plugin"));
        }
    }

    #[test]
    fn the_export_is_read_into_identifiers_by_resource_id() {
        let export = r#"{"version":3,"deployment":{"resources":[
            {"urn":"urn:pulumi:dev::p::pulumi:pulumi:Stack::p-dev","type":"pulumi:pulumi:Stack"},
            {"urn":"urn:pulumi:dev::p::pulumi:providers:aws::default","id":"abc"},
            {"urn":"urn:pulumi:dev::p::aws:s3/bucket:Bucket::logs","id":"logs-1234"},
            {"urn":"urn:pulumi:dev::p::aws:s3/bucket:Bucket::pending"}
        ]}}"#;
        let found = read_identifiers(export).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found["logs"], "logs-1234");
    }
}
