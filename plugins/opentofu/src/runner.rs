//! The half that hands a configuration to `tofu`. It sits outside the
//! compile chain: nothing here is reached by a compile.

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    cloud_for, decode_value, Applied, Artifact, Change, ChangeAction, CloudHandover, Log,
    Operations, Output, Plan, RunContext, RunError, Runner, SavedPlan, Value,
};
use meridian_sdk::{settings, Setting};

use crate::CONFIG_PATH;

/// What `--tool-debug` adds to the tool, level 1 first. Terraform's log goes to standard error, beside its own output.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::env(&[("TF_LOG", "INFO")]),
    meridian_sdk::debug::DebugLevel::env(&[("TF_LOG", "DEBUG")]),
    meridian_sdk::debug::DebugLevel::env(&[("TF_LOG", "TRACE")]),
];

/// Names the program to run, when it is not `tofu` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_TOFU";
/// Extra arguments for `tofu init`, `tofu plan` and `tofu apply`, split like
/// a shell command line: `-parallelism=4`, `-lock-timeout=30s`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_TOFU_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "tofu",
            "The `tofu` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for `tofu init`, `tofu plan` and `tofu apply`, split like a shell command line: `-parallelism=4`, `-lock-timeout=30s`. The calls that read a saved plan or the outputs get none.",
        ),
    ]
}

/// The saved plan, so that what an apply does is exactly what was planned.
/// When the document encrypts its plan, this file is encrypted too.
const PLAN_FILE: &str = "meridian.tfplan";

/// Runs a compiled configuration.
///
/// The work directory is OpenTofu's working directory: the configuration is
/// written there, and OpenTofu keeps `.terraform` and, unless the document
/// declares a backend, its state file beside it.
pub struct OpenTofuRunner {
    /// The program to run.
    pub program: String,
    /// Arguments added to `init`, `plan` and `apply`, after OpenTofu's own.
    pub extra_args: Vec<String>,
}

impl OpenTofuRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        OpenTofuRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
        }
    }

    /// One call, in the work directory, told the cloud credential when
    /// the stage has one.
    fn tool(&self, context: &RunContext, cloud: Option<&CloudHandover>) -> Tool {
        let mut tool = Tool::new(&self.program)
            .dir(&context.work_dir)
            .env("TF_IN_AUTOMATION", "1")
            .env("TF_INPUT", "0")
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level));
        // Values handed over from earlier stages reach the configuration as
        // input variables, for a document that declares them with `variable`.
        // OpenTofu reads the same TF_VAR_ and TF_IN_AUTOMATION names
        // Terraform does.
        for (name, value) in &context.inputs {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.canonical(),
            };
            tool = tool.env(format!("TF_VAR_{name}"), text);
        }
        CloudHandover::tell_if_any(cloud, tool)
    }

    /// Writes the configuration, initialises the directory and saves a plan.
    fn prepare(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        if !artifact.files.contains_key(CONFIG_PATH) {
            return Err(RunError::new(format!(
                "the artifact has no {CONFIG_PATH}; it was not compiled by this plugin"
            )));
        }
        write_artifact(&context.work_dir, artifact)?;

        self.tool(context, cloud)
            .args(["init", "-input=false", "-no-color"])
            .args(self.extra_args.iter().cloned())
            .run_ok("tofu init failed", log)?;
        // Secrets reach tofu as input variables read from a file only the
        // user can read, named on the command line by its path and nothing
        // more. The file lives until the plan is saved. The saved plan holds
        // the values too, as every tofu plan file does, so it is scrubbed
        // as soon as it has been used (see `scrub`).
        let secrets = if context.secrets.is_empty() {
            None
        } else {
            Some(
                context
                    .secrets
                    .json_file("secrets.auto.tfvars.json", crate::secret_variable)
                    .map_err(|e| RunError::new(format!("writing the variables file: {e}")))?,
            )
        };
        let mut plan = self
            .tool(context, cloud)
            .args(["plan", "-input=false", "-no-color"])
            .args(self.extra_args.iter().cloned())
            .arg(format!("-out={PLAN_FILE}"));
        if let Some(file) = &secrets {
            plan = plan.arg(format!("-var-file={}", file.path().display()));
        }
        let planned = plan.run_ok("tofu plan failed", log);
        drop(secrets);
        planned?;
        let shown = self
            .tool(context, cloud)
            .args(["show", "-json", PLAN_FILE])
            .capture_stdout()
            .run_ok("tofu could not show the plan it saved", log)?;
        read_plan(&shown.stdout)
    }

    /// Writes the configuration, initialises the directory and puts back a
    /// plan `plan` saved, reading what it would do from the tool.
    fn restore(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        artifact: &Artifact,
        plan: &[u8],
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        if !artifact.files.contains_key(CONFIG_PATH) {
            return Err(RunError::new(format!(
                "the artifact has no {CONFIG_PATH}; it was not compiled by this plugin"
            )));
        }
        write_artifact(&context.work_dir, artifact)?;
        self.tool(context, cloud)
            .args(["init", "-input=false", "-no-color"])
            .args(self.extra_args.iter().cloned())
            .run_ok("tofu init failed", log)?;
        std::fs::write(context.work_dir.join(PLAN_FILE), plan)
            .map_err(|e| RunError::new(format!("writing the saved plan {PLAN_FILE}: {e}")))?;
        log.out("applying the plan saved by `meridian plan --out`");
        let shown = self
            .tool(context, cloud)
            .args(["show", "-json", PLAN_FILE])
            .capture_stdout()
            .run_ok("tofu could not show the saved plan", log)?;
        read_plan(&shown.stdout)
    }

    /// Removes the saved plan once it has served, when the run handed over
    /// secrets: a plan file holds the value of every variable, sensitive
    /// ones included, and the work directory is not the place to keep one.
    fn scrub(&self, context: &RunContext) {
        if context.secrets.is_empty() {
            return;
        }
        let path = context.work_dir.join(PLAN_FILE);
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&path) {
                use std::io::Write as _;
                let zeroes = vec![0u8; 64 * 1024];
                let mut left = meta.len();
                while left > 0 {
                    let n = usize::try_from(left)
                        .unwrap_or(usize::MAX)
                        .min(zeroes.len());
                    if file.write_all(&zeroes[..n]).is_err() {
                        break;
                    }
                    left -= n as u64;
                }
                let _ = file.sync_all();
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    fn read_outputs(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        log: &dyn Log,
    ) -> Result<Vec<Output>, RunError> {
        let finished = self
            .tool(context, cloud)
            .args(["output", "-json", "-no-color"])
            .capture_stdout()
            .run_ok("tofu could not report its outputs", log)?;
        read_outputs(&finished.stdout)
    }
}

impl Runner for OpenTofuRunner {
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
        // A cloud credential reaches the providers in the environment of
        // every call of the stage, and the files it needs live to the end.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let prepared = self.prepare(context, cloud.as_ref(), artifact, log);
        // The plan file is the tool's own saved plan, which `meridian plan
        // --out` keeps for the apply. It holds the value of every variable,
        // so when the run handed over a secret it is scrubbed instead of
        // saved, and the apply from the plan file plans again.
        let saved = if context.save_plan && context.secrets.is_empty() && prepared.is_ok() {
            match std::fs::read(context.work_dir.join(PLAN_FILE)) {
                Ok(bytes) => Some(SavedPlan::new(bytes)),
                Err(e) => {
                    return Err(RunError::new(format!(
                        "the saved plan {PLAN_FILE} could not be read back: {e}"
                    )))
                }
            }
        } else {
            None
        };
        self.scrub(context);
        Ok(Plan {
            changes: prepared?,
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
        // A plan `meridian plan --out` kept is put back where the tool saved
        // it and applied as it is; OpenTofu itself refuses a plan whose
        // state or configuration has moved on. Without one, the apply plans
        // first, as it always did.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let prepared = match &context.saved_plan {
            Some(saved) => self.restore(context, cloud.as_ref(), artifact, &saved.data, log),
            None => self.prepare(context, cloud.as_ref(), artifact, log),
        };
        let changes = match prepared {
            Ok(changes) => changes,
            Err(e) => {
                self.scrub(context);
                return Err(e);
            }
        };
        // The saved plan is applied, so nothing changes that was not just
        // reported. A plan file is its own approval.
        let applied = self
            .tool(context, cloud.as_ref())
            .args(["apply", "-input=false", "-no-color"])
            .args(self.extra_args.iter().cloned())
            .arg(PLAN_FILE)
            .run_ok("tofu apply failed", log);
        self.scrub(context);
        applied?;
        Ok(Applied {
            changes,
            outputs: self.read_outputs(context, cloud.as_ref(), log)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        if !context.work_dir.join(CONFIG_PATH).exists() {
            return Err(RunError::new(
                "nothing has been applied for this document yet, so it has no outputs",
            ));
        }
        // Outputs are read with no credential: the host hands one to plan
        // and apply only. A backend that needs the cloud's login to read
        // the state is read right after the apply, inside it.
        self.read_outputs(context, None, &meridian_sdk::NoLog)
    }

    fn reads_identifiers(&self) -> bool {
        true
    }

    fn identifiers(
        &self,
        context: &RunContext,
    ) -> Result<std::collections::BTreeMap<String, String>, RunError> {
        if !context.work_dir.join(CONFIG_PATH).exists() {
            return Err(RunError::new(
                "nothing has been applied for this document yet, so there is nothing to adopt",
            ));
        }
        let log = &meridian_sdk::NoLog;
        // A backend in the cloud is read as the document's cloud credential,
        // the way a plan reads it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        // The state is read through the tool, which needs the directory
        // initialised for its backend and providers.
        self.tool(context, cloud.as_ref())
            .args(["init", "-input=false", "-no-color"])
            .args(self.extra_args.iter().cloned())
            .run_ok("tofu init failed", log)?;
        let shown = self
            .tool(context, cloud.as_ref())
            .args(["show", "-json", "-no-color"])
            .capture_stdout()
            .run_ok("tofu could not show its state", log)?;
        read_identifiers(&shown.stdout)
    }
}

fn unreadable(what: &str, detail: impl std::fmt::Display) -> RunError {
    RunError::new(format!("tofu's {what} could not be read: {detail}"))
}

/// Reads `tofu show -json` of a saved plan into the changes it holds.
fn read_plan(json: &str) -> Result<Vec<Change>, RunError> {
    let plan = decode_value(json.as_bytes()).map_err(|e| unreadable("plan", e))?;
    let Some(plan) = plan.as_map() else {
        return Err(unreadable("plan", "it is not a JSON object"));
    };
    let mut changes = Vec::new();
    let resource_changes = plan
        .get("resource_changes")
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    for entry in resource_changes {
        let Some(entry) = entry.as_map() else {
            continue;
        };
        let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("");
        // A resource's `adopt`, compiled to an import block: the plan says
        // it is importing, and its actions say what else is done to it.
        let importing = entry
            .get("change")
            .and_then(Value::as_map)
            .is_some_and(|c| c.get("importing").is_some_and(|i| *i != Value::Null));
        let actions: Vec<&str> = entry
            .get("change")
            .and_then(Value::as_map)
            .and_then(|c| c.get("actions"))
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let action = match actions.as_slice() {
            ["no-op"] if importing => ChangeAction::Adopt,
            ["create"] => ChangeAction::Create,
            ["update"] => ChangeAction::Update,
            ["delete"] => ChangeAction::Delete,
            ["delete", "create"] | ["create", "delete"] => ChangeAction::Replace,
            // "no-op" and "read" change nothing, and "forget" only drops a
            // resource from the state without touching the real thing.
            _ => continue,
        };
        // A resource declared in the document is named by its id. One inside
        // a module, or a data source, keeps OpenTofu's full address.
        let top_level = text("mode") == "managed" && !entry.contains_key("module_address");
        changes.push(Change {
            resource: if top_level && !text("name").is_empty() && !entry.contains_key("index") {
                text("name").to_owned()
            } else {
                text("address").to_owned()
            },
            action,
            // An adoption that also changes the thing is reported as the
            // change, and says it adopts.
            summary: if importing && action != ChangeAction::Adopt {
                format!("{}, adopted", text("type"))
            } else {
                text("type").to_owned()
            },
        });
    }
    Ok(changes)
}

/// Reads `tofu output -json`.
fn read_outputs(json: &str) -> Result<Vec<Output>, RunError> {
    let outputs = decode_value(json.as_bytes()).map_err(|e| unreadable("outputs", e))?;
    let Some(outputs) = outputs.as_map() else {
        return Err(unreadable("outputs", "they are not a JSON object"));
    };
    Ok(outputs
        .iter()
        .filter_map(|(name, entry)| {
            let entry = entry.as_map()?;
            Some(Output {
                name: name.clone(),
                value: entry.get("value").cloned().unwrap_or(Value::Null),
                sensitive: entry.get("sensitive") == Some(&Value::Bool(true)),
            })
        })
        .collect())
}

/// Reads `tofu show -json` of the state into each top-level managed
/// resource's `id`, by the resource's name, which is its id in the document.
/// A resource inside a module, or one of several made by `count` or
/// `for_each`, has no one resource of the document to belong to.
fn read_identifiers(json: &str) -> Result<std::collections::BTreeMap<String, String>, RunError> {
    let mut found = std::collections::BTreeMap::new();
    if json.trim().is_empty() {
        return Ok(found);
    }
    let state = decode_value(json.as_bytes()).map_err(|e| unreadable("state", e))?;
    let resources = state
        .as_map()
        .and_then(|s| s.get("values"))
        .and_then(Value::as_map)
        .and_then(|v| v.get("root_module"))
        .and_then(Value::as_map)
        .and_then(|m| m.get("resources"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    for entry in resources {
        let Some(entry) = entry.as_map() else {
            continue;
        };
        let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("");
        if text("mode") != "managed" || entry.contains_key("index") || text("name").is_empty() {
            continue;
        }
        let id = entry
            .get("values")
            .and_then(Value::as_map)
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str);
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            found.insert(text("name").to_owned(), id.to_owned());
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_is_read_into_changes() {
        let json = r#"{"format_version":"1.2","resource_changes":[
          {"address":"aws_instance.web_vm","mode":"managed","type":"aws_instance","name":"web_vm","change":{"actions":["create"]}},
          {"address":"aws_eip.addr","mode":"managed","type":"aws_eip","name":"addr","change":{"actions":["delete","create"]}},
          {"address":"aws_s3_bucket.logs","mode":"managed","type":"aws_s3_bucket","name":"logs","change":{"actions":["no-op"]}},
          {"address":"data.aws_ami.ubuntu","mode":"data","type":"aws_ami","name":"ubuntu","change":{"actions":["read"]}},
          {"address":"module.net.aws_vpc.this","module_address":"module.net","mode":"managed","type":"aws_vpc","name":"this","change":{"actions":["update"]}},
          {"address":"aws_subnet.a[0]","mode":"managed","type":"aws_subnet","name":"a","index":0,"change":{"actions":["delete"]}},
          {"address":"aws_s3_bucket.kept","mode":"managed","type":"aws_s3_bucket","name":"kept","change":{"actions":["forget"]}}
        ]}"#;
        let shown: Vec<String> = read_plan(json)
            .unwrap()
            .iter()
            .map(Change::to_string)
            .collect();
        assert_eq!(
            shown,
            [
                "create web_vm: aws_instance",
                "replace addr: aws_eip",
                "update module.net.aws_vpc.this: aws_vpc",
                "delete aws_subnet.a[0]: aws_subnet",
            ]
        );
        assert!(read_plan("{}").unwrap().is_empty());
        assert!(read_plan("not json").is_err());
    }

    #[test]
    fn the_state_is_read_into_identifiers_by_resource_id() {
        let json = r#"{"format_version":"1.0","values":{"root_module":{"resources":[
          {"address":"aws_instance.web","mode":"managed","type":"aws_instance","name":"web","values":{"id":"i-0abc123","ami":"ami-1"}},
          {"address":"data.aws_ami.ubuntu","mode":"data","type":"aws_ami","name":"ubuntu","values":{"id":"ami-1"}},
          {"address":"aws_subnet.a[0]","mode":"managed","type":"aws_subnet","name":"a","index":0,"values":{"id":"subnet-1"}}
        ],"child_modules":[{"resources":[{"address":"module.net.aws_vpc.this","mode":"managed","type":"aws_vpc","name":"this","values":{"id":"vpc-1"}}]}]}}}"#;
        let found = read_identifiers(json).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found["web"], "i-0abc123");
        assert!(read_identifiers("").unwrap().is_empty());
        assert!(read_identifiers("{}").unwrap().is_empty());
        assert!(read_identifiers("not json").is_err());
    }

    #[test]
    fn an_import_is_read_as_an_adoption() {
        let json = r#"{"format_version":"1.2","resource_changes":[
          {"address":"aws_instance.web","mode":"managed","type":"aws_instance","name":"web","change":{"actions":["no-op"],"importing":{"id":"i-0abc123"}}},
          {"address":"aws_s3_bucket.logs","mode":"managed","type":"aws_s3_bucket","name":"logs","change":{"actions":["update"],"importing":{"id":"logs"}}},
          {"address":"aws_eip.addr","mode":"managed","type":"aws_eip","name":"addr","change":{"actions":["no-op"],"importing":null}}
        ]}"#;
        let shown: Vec<String> = read_plan(json)
            .unwrap()
            .iter()
            .map(Change::to_string)
            .collect();
        assert_eq!(
            shown,
            [
                "adopt web: aws_instance",
                "update logs: aws_s3_bucket, adopted",
            ]
        );
    }

    #[test]
    fn outputs_keep_their_types_and_their_sensitivity() {
        let json = r#"{"web_vm_ip":{"sensitive":false,"type":"string","value":"203.0.113.7"},
                       "db_password":{"sensitive":true,"type":"string","value":"hunter2"},
                       "ports":{"sensitive":false,"type":["list","number"],"value":[80,443]}}"#;
        let outputs = read_outputs(json).unwrap();
        assert_eq!(outputs.len(), 3);
        let password = outputs.iter().find(|o| o.name == "db_password").unwrap();
        assert!(password.sensitive);
        assert!(!format!("{password:?}").contains("hunter2"));
        let ip = outputs.iter().find(|o| o.name == "web_vm_ip").unwrap();
        assert_eq!(ip.value, Value::String("203.0.113.7".into()));
        let ports = outputs.iter().find(|o| o.name == "ports").unwrap();
        assert_eq!(
            ports.value,
            Value::List(vec![Value::Int(80), Value::Int(443)])
        );
    }
}
