//! The half that hands a program to `pulumi`. It sits outside the compile
//! chain: nothing here is reached by a compile.

use std::collections::BTreeSet;
use std::path::Path;

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, Operations, Output, Plan,
    RunContext, RunError, Runner, Value,
};

use crate::{PROGRAM_PATH, STACK_PATH};

/// Names the program to run, when it is not `pulumi` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_PULUMI";

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
        PulumiRunner {
            program: std::env::var(PROGRAM_VARIABLE)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "pulumi".to_owned()),
            backend_chosen: set(BACKEND_VARIABLE),
            // A passphrase that is set but empty is still a choice.
            passphrase_chosen: PASSPHRASE_VARIABLES
                .iter()
                .any(|v| std::env::var_os(v).is_some()),
        }
    }

    /// One `pulumi` call in the project directory.
    fn pulumi(&self, context: &RunContext, stack: &Stack) -> Tool {
        let mut tool = Tool::new(&self.program)
            .dir(&context.work_dir)
            .env("PULUMI_SKIP_UPDATE_CHECK", "true");
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
        tool
    }

    /// A call that acts on the stack.
    fn on_stack(&self, context: &RunContext, stack: &Stack, command: &[&str]) -> Tool {
        self.pulumi(context, stack)
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

    /// Writes the program, makes sure the stack exists and asks Pulumi what
    /// an update would change.
    fn prepare(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<(Stack, Vec<Change>), RunError> {
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
            .pulumi(context, &stack)
            .args(["stack", "select", &stack.name, "--create"])
            .args(["--non-interactive", "--color", "never"]);
        if !stack.encryption.is_empty() {
            select = select.args(["--secrets-provider", &stack.encryption]);
        }
        select.run_ok("pulumi could not select or create the stack", log)?;

        log.out(&format!(
            "stack {}: asking Pulumi what would change",
            stack.name
        ));
        let preview = Self::with_inputs(
            self.on_stack(context, &stack, &["preview"]).arg("--json"),
            context,
        )
        .capture_stdout()
        .run(log)?;
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
            let mut err = preview.into_error("pulumi preview failed");
            if !said.trim().is_empty() {
                if !err.output.is_empty() && !err.output.ends_with('\n') {
                    err.output.push('\n');
                }
                err.output.push_str(said.trim_end());
                err.output.push('\n');
            }
            return Err(err);
        }
        Ok((stack, read?.changes))
    }

    fn read_outputs(
        &self,
        context: &RunContext,
        stack: &Stack,
        log: &dyn Log,
    ) -> Result<Vec<Output>, RunError> {
        // Asked twice: once as Pulumi would show them, with secrets masked,
        // and once in full. Whatever differs between the two is a secret.
        let masked = self
            .on_stack(context, stack, &["stack", "output"])
            .arg("--json")
            .capture_stdout()
            .run_ok("pulumi could not report the stack's outputs", log)?;
        let shown = self
            .on_stack(context, stack, &["stack", "output"])
            .args(["--json", "--show-secrets"])
            .capture_stdout()
            .run_ok("pulumi could not report the stack's outputs", log)?;
        read_outputs(&masked.stdout, &shown.stdout)
    }
}

impl Runner for PulumiRunner {
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
        let (_, changes) = self.prepare(context, artifact, log)?;
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
        let (stack, changes) = self.prepare(context, artifact, log)?;
        // The preview above is the report; the update does not print a
        // second one.
        Self::with_inputs(
            self.on_stack(context, &stack, &["up"])
                .args(["--yes", "--skip-preview"]),
            context,
        )
        .run_ok("pulumi up failed", log)?;
        Ok(Applied {
            changes,
            outputs: self.read_outputs(context, &stack, log)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let Ok(settings) = std::fs::read_to_string(context.work_dir.join(STACK_PATH)) else {
            return Err(RunError::new(
                "nothing has been applied for this document yet, so it has no outputs",
            ));
        };
        let stack = Stack::read(&settings)?;
        self.read_outputs(context, &stack, &meridian_sdk::NoLog)
    }
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
            "create" | "import" => ChangeAction::Create,
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
}
