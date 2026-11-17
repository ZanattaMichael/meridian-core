//! The half that hands a playbook to `ansible-playbook`. It sits outside the
//! compile chain: nothing here is reached by a compile.

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Log, LogLine, Operations, Part, Plan, RunContext,
    RunError, Runner, SecretFile, Value,
};

use crate::{INVENTORY_PATH, PLAYBOOK_PATH};

/// Names the `ansible-playbook` program to run, when it is not on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_ANSIBLE_PLAYBOOK";
/// Extra arguments for every run, split on whitespace: how to connect, which
/// user, which key. For example `--user deploy --private-key ~/.ssh/deploy`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_ANSIBLE_ARGS";

/// Runs a compiled playbook.
///
/// A plan is `ansible-playbook --check --diff`, Ansible's own dry run. An
/// apply is the same command without `--check`. Ansible reports no outputs.
pub struct AnsibleRunner {
    /// The program to run.
    pub program: String,
    /// Arguments added to every run, after Meridian's own.
    pub extra_args: Vec<String>,
}

impl AnsibleRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        AnsibleRunner {
            program: std::env::var(PROGRAM_VARIABLE)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "ansible-playbook".to_owned()),
            extra_args: std::env::var(ARGS_VARIABLE)
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
        }
    }

    fn run(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        check: bool,
        log: &dyn Log,
    ) -> Result<Report, RunError> {
        for needed in [PLAYBOOK_PATH, INVENTORY_PATH] {
            if !artifact.files.contains_key(needed) {
                return Err(RunError::new(format!(
                    "the artifact has no {needed}; it was not compiled by this plugin"
                )));
            }
        }
        write_artifact(&context.work_dir, artifact)?;

        let mut tool = Tool::new(&self.program)
            .args(["--inventory", INVENTORY_PATH])
            .dir(&context.work_dir)
            // The report is read from the default output, so nothing in the
            // operator's own configuration may reshape it.
            .env("ANSIBLE_STDOUT_CALLBACK", "default")
            .env("ANSIBLE_NOCOLOR", "1")
            .env("ANSIBLE_FORCE_COLOR", "0")
            .env("ANSIBLE_DISPLAY_OK_HOSTS", "1")
            .env("ANSIBLE_DISPLAY_SKIPPED_HOSTS", "1")
            .env("ANSIBLE_RETRY_FILES_ENABLED", "0");
        if check {
            tool = tool.args(["--check", "--diff"]);
        }
        // Secrets reach ansible-playbook as extra variables read from a file
        // only the user can read, named on the command line by its path and
        // nothing more. A credential's password goes in the same file, and
        // its private key in a file of its own, named by `--private-key`.
        // The files live until the run is over.
        let secrets = extra_vars_file(context)?;
        if let Some(file) = &secrets {
            tool = tool
                .arg("--extra-vars")
                .arg(format!("@{}", file.path().display()));
        }
        let key = private_key_file(context)?;
        if let Some(file) = &key {
            tool = tool
                .arg("--private-key")
                .arg(file.path().to_string_lossy().into_owned());
        }
        tool = tool
            .args(self.extra_args.iter().cloned())
            .arg(PLAYBOOK_PATH);

        let reader = Reader {
            inner: log,
            report: Default::default(),
            check,
        };
        let finished = tool.run(&reader)?;
        drop(secrets);
        drop(key);
        let report = reader
            .report
            .into_inner()
            .expect("no panic holds the report");
        if finished.success() {
            return Ok(report);
        }
        let what = match finished.code {
            Some(4) => "ansible-playbook could not reach the host",
            Some(2) => "ansible-playbook reported a failed task",
            _ => "ansible-playbook failed",
        };
        let mut error = finished.into_error(what);
        if !check {
            error.changes = report.changes;
        }
        Err(error)
    }
}

/// The extra-vars file: every secret of the document under its variable's
/// name, and the credential's password as `ansible_password`. `None` when
/// there is nothing to hand over.
fn extra_vars_file(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
    let mut entries: Vec<(String, &meridian_sdk::SecretValue)> = context
        .secrets
        .iter()
        .map(|(name, value)| {
            (
                Value::String(crate::secret_variable(name)).canonical(),
                value,
            )
        })
        .collect();
    if let Some(password) = context
        .credential
        .as_ref()
        .and_then(|c| c.secret("password"))
    {
        entries.push((
            Value::String(crate::PASSWORD_VARIABLE.to_owned()).canonical(),
            password,
        ));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let mut parts = vec![Part::Text("{")];
    for (i, (key, value)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(Part::Text(","));
        }
        parts.push(Part::Text(key));
        parts.push(Part::Text(":\""));
        parts.push(Part::JsonString(value));
        parts.push(Part::Text("\""));
    }
    parts.push(Part::Text("}\n"));
    let file = SecretFile::create("extra-vars.json", &parts)
        .map_err(|e| RunError::new(format!("writing the extra-vars file: {e}")))?;
    Ok(Some(file))
}

/// The credential's private key in a file only the user can read, for
/// `--private-key`. `None` when the credential has none.
fn private_key_file(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
    let Some(key) = context
        .credential
        .as_ref()
        .and_then(|c| c.secret("privateKey"))
    else {
        return Ok(None);
    };
    // OpenSSH wants the key to end in a line break; one more is harmless.
    let file = SecretFile::create("private-key", &[Part::Secret(key), Part::Text("\n")])
        .map_err(|e| RunError::new(format!("writing the private key file: {e}")))?;
    Ok(Some(file))
}

impl Runner for AnsibleRunner {
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
        let report = self.run(context, artifact, true, log)?;
        Ok(Plan {
            changes: report.changes,
            // A task Ansible skips in check mode is one it cannot preview: a
            // command with no `creates` to test, or a condition only the node
            // can answer.
            unknown: report.skipped,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let report = self.run(context, artifact, false, log)?;
        Ok(Applied {
            changes: report.changes,
            outputs: Vec::new(),
        })
    }
}

/// What a run's output said, read as it went by.
#[derive(Default)]
struct Report {
    /// The task whose results the following lines belong to.
    task: String,
    changes: Vec<Change>,
    skipped: bool,
}

/// Relays every line to the host's log and reads the task results out of
/// Ansible's default output on the way:
///
/// ```text
/// TASK [nginx_pkg] *************
/// changed: [web01.example.com]
/// RUNNING HANDLER [restart nginx_svc] ******
/// changed: [web01.example.com]
/// ```
struct Reader<'a> {
    inner: &'a dyn Log,
    report: std::sync::Mutex<Report>,
    check: bool,
}

impl Log for Reader<'_> {
    fn line(&self, line: LogLine) {
        if line.stream == meridian_sdk::LogStream::Stdout {
            let mut report = self.report.lock().expect("no panic holds the report");
            read_line(&mut report, &line.text, self.check);
        }
        self.inner.line(line);
    }
}

fn read_line(report: &mut Report, text: &str, check: bool) {
    for banner in ["TASK [", "RUNNING HANDLER ["] {
        if let Some(rest) = text.strip_prefix(banner) {
            // The name ends at the last `]` before the row of asterisks, so a
            // name holding a bracket of its own survives.
            if let Some(end) = rest.rfind("] *").or_else(|| rest.rfind(']')) {
                report.task = rest[..end].to_owned();
            }
            return;
        }
    }
    if text.starts_with("changed: [") {
        let summary = if check { "would change" } else { "changed" };
        let change = Change {
            resource: report.task.clone(),
            action: ChangeAction::Update,
            summary: summary.to_owned(),
        };
        // One host per play, but a looped task reports once per item.
        if !report.changes.contains(&change) {
            report.changes.push(change);
        }
    } else if text.starts_with("skipping: [") && check {
        report.skipped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_attributed_to_the_task_above_them() {
        let mut report = Report::default();
        for line in [
            "PLAY [web] *********",
            "TASK [Gathering Facts] *********",
            "ok: [web01]",
            "TASK [nginx_pkg] ***********",
            "changed: [web01]",
            "TASK [odd ] name] ***********",
            "ok: [web01]",
            "RUNNING HANDLER [restart nginx_svc] *****",
            "changed: [web01]",
            "PLAY RECAP *****",
            "web01 : ok=4 changed=2",
        ] {
            read_line(&mut report, line, false);
        }
        let names: Vec<&str> = report.changes.iter().map(|c| c.resource.as_str()).collect();
        assert_eq!(names, ["nginx_pkg", "restart nginx_svc"]);
        assert_eq!(report.task, "restart nginx_svc");
        assert!(!report.skipped);
    }
}
