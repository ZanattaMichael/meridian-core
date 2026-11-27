//! The half that hands a playbook to `ansible-playbook`. It sits outside the
//! compile chain: nothing here is reached by a compile.

use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, LogLine, Operations, Part, Plan,
    RunContext, RunError, Runner, SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::inverse::{self, Kept};
use crate::{DESTROY_PLAYBOOK_PATH, INVENTORY_PATH, PLAYBOOK_PATH};

/// What `--tool-debug` adds to the tool, level 1 first. The verbose lines are interleaved with the play's own output.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["-v"]),
    meridian_sdk::debug::DebugLevel::args(&["-vvv"]),
    meridian_sdk::debug::DebugLevel::args(&["-vvvv"]),
];

/// Names the `ansible-playbook` program to run, when it is not on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_ANSIBLE_PLAYBOOK";
/// Extra arguments for every run, split like a shell command line: how to
/// connect, which user, which key. For example `--user deploy --private-key
/// ~/.ssh/deploy`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_ANSIBLE_ARGS";
/// The `ansible-doc` program `meridian schema sync` asks about modules.
pub const DOC_VARIABLE: &str = "MERIDIAN_ANSIBLE_DOC";
/// The `ansible-galaxy` program `meridian schema sync` asks for the
/// installed collections' versions.
pub const GALAXY_VARIABLE: &str = "MERIDIAN_ANSIBLE_GALAXY";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "ansible-playbook",
            "The `ansible-playbook` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every run, split like a shell command line: how to connect, which user, which key. For example `--user deploy --private-key ~/.ssh/deploy`.",
        ),
        Setting::new(
            "doc",
            DOC_VARIABLE,
            "ansible-doc",
            "The `ansible-doc` program `meridian schema sync` runs to describe the modules the documents use, when it is not on `PATH`.",
        ),
        Setting::new(
            "galaxy",
            GALAXY_VARIABLE,
            "ansible-galaxy",
            "The `ansible-galaxy` program `meridian schema sync` runs to read the installed collections' versions, when it is not on `PATH`.",
        ),
    ]
}

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
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        AnsibleRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
        }
    }

    /// `ansible-playbook` set up for the context, before the playbook is
    /// named: the inventory, check mode when asked, the secrets' and the
    /// key's files, and the operator's arguments. The files live as long as
    /// the values returned for them.
    pub(crate) fn tool(
        &self,
        context: &RunContext,
        check: bool,
    ) -> Result<(Tool, Option<SecretFile>, Option<SecretFile>), RunError> {
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
            .env("ANSIBLE_RETRY_FILES_ENABLED", "0")
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level));
        if check {
            tool = tool.args(["--check", "--diff"]);
        }
        // Secrets reach ansible-playbook as extra variables read from a file
        // only the user can read, named on the command line by its path and
        // nothing more. A credential's password goes in the same file, and
        // its private key in a file of its own, named by `--private-key`.
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
        Ok((tool.args(self.extra_args.iter().cloned()), secrets, key))
    }

    fn run(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        playbook: &str,
        check: bool,
        log: &dyn Log,
    ) -> Result<Report, RunError> {
        write_artifact(&context.work_dir, artifact)?;
        // The play named must be in the work directory: the forward play and
        // the inventory come from the artifact, the destroy play the runner
        // writes beside them.
        for needed in [playbook, INVENTORY_PATH] {
            if !context.work_dir.join(needed).is_file() {
                return Err(RunError::new(format!(
                    "the artifact has no {needed}; it was not compiled by this plugin"
                )));
            }
        }
        let (tool, secrets, key) = self.tool(context, check)?;
        let tool = tool.arg(playbook);

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
        error.failures = report.failures;
        if !check {
            error.changes = report.changes;
        }
        Err(error)
    }

    /// Runs the inverse of the artifact's play, written beside it first.
    /// Returns the report and the resources the inverse could not remove.
    fn run_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        check: bool,
        log: &dyn Log,
    ) -> Result<(Report, Vec<Kept>), RunError> {
        let source = artifact.files.get(PLAYBOOK_PATH).ok_or_else(|| {
            RunError::new(format!(
                "the artifact has no {PLAYBOOK_PATH}; it was not compiled by this plugin"
            ))
        })?;
        let (destroy, kept) = inverse::destroy_playbook(source).map_err(RunError::new)?;
        write_artifact(&context.work_dir, artifact)?;
        let path = context.work_dir.join(DESTROY_PLAYBOOK_PATH);
        std::fs::write(&path, destroy)
            .map_err(|e| RunError::new(format!("writing {}: {e}", path.display())))?;
        let mut report = self.run(context, artifact, DESTROY_PLAYBOOK_PATH, check, log)?;
        // What a destroy removed is a `delete`, not the `update` Ansible's
        // `changed:` line reads as.
        let summary = if check { "would remove" } else { "removed" };
        for change in &mut report.changes {
            change.action = ChangeAction::Delete;
            change.summary = summary.to_owned();
        }
        Ok((report, kept))
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
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

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
        let report = self.run(context, artifact, PLAYBOOK_PATH, true, log)?;
        Ok(Plan {
            changes: report.changes,
            // A task Ansible skips in check mode is one it cannot preview: a
            // command with no `creates` to test, or a condition only the node
            // can answer.
            unknown: report.skipped,
            saved: None,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let report = self.run(context, artifact, PLAYBOOK_PATH, false, log)?;
        Ok(Applied {
            changes: report.changes,
            outputs: Vec::new(),
        })
    }

    /// Ansible keeps no state, so `destroy` is Meridian's: it compiles the
    /// document's inverse into the tool's own play and applies it. A task
    /// whose module has no absent state is left in place and reported.
    fn destroys(&self) -> bool {
        true
    }

    fn plan_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let (report, _) = self.run_destroy(context, artifact, true, log)?;
        Ok(Plan {
            changes: report.changes,
            unknown: report.skipped,
            saved: None,
        })
    }

    fn destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let (report, kept) = self.run_destroy(context, artifact, false, log)?;
        for k in &kept {
            log.err(&format!(
                "meridian: [{}]: {} was not removed: {}",
                meridian_sdk::rules::UNSUPPORTED_DESTROY.name,
                k.resource,
                k.reason
            ));
        }
        Ok(report.changes)
    }

    fn inspects(&self) -> bool {
        true
    }

    fn inspect(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        _endpoint: &str,
    ) -> Result<meridian_sdk::Inspection, RunError> {
        crate::inspect::inspect(self, context, artifact, resource)
    }
}

/// What a run's output said, read as it went by.
#[derive(Default)]
struct Report {
    /// The task whose results the following lines belong to.
    task: String,
    changes: Vec<Change>,
    skipped: bool,
    /// The failed and unreachable task results, one per task and host.
    failures: Vec<Failure>,
    /// A failed result whose JSON runs over several lines (`-vvv` indents
    /// it): the line's start and the JSON read so far.
    pending: Option<(Outcome, String, String)>,
}

/// What a `fatal:` or `failed:` line said of its task.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Failed,
    Unreachable,
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
    if let Some((outcome, host, mut json)) = report.pending.take() {
        json.push('\n');
        json.push_str(text);
        match decode_value(json.as_bytes()) {
            Ok(result) => fail(report, outcome, &host, Some(&result)),
            // The JSON never closed: the result is still a failure.
            Err(_) if text.starts_with("TASK [") || text.starts_with("PLAY ") => {
                fail(report, outcome, &host, None);
                return read_line(report, text, check);
            }
            Err(_) => report.pending = Some((outcome, host, json)),
        }
        return;
    }
    if let Some((outcome, host, payload)) = failed_line(text) {
        match payload.map(|json| (json, decode_value(json.as_bytes()))) {
            Some((_, Ok(result))) => fail(report, outcome, &host, Some(&result)),
            Some((json, Err(_))) if json.starts_with('{') => {
                report.pending = Some((outcome, host, json.to_owned()))
            }
            _ => fail(report, outcome, &host, None),
        }
        return;
    }
    if text.trim() == "...ignoring" {
        // `ignore_errors`: the task's failure did not fail the play.
        let task = report.task.clone();
        report.failures.retain(|f| f.resource != task);
        return;
    }
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

/// The outcome, host and JSON result of a failed task's line:
///
/// ```text
/// fatal: [web01]: FAILED! => {"changed": false, "msg": "..."}
/// fatal: [web01]: UNREACHABLE! => {"msg": "...", "unreachable": true}
/// failed: [web01] (item=nginx) => {"ansible_loop_var": "item", "msg": "..."}
/// ```
fn failed_line(text: &str) -> Option<(Outcome, String, Option<&str>)> {
    let rest = text
        .strip_prefix("fatal: [")
        .or_else(|| text.strip_prefix("failed: ["))?;
    let end = rest.find(']')?;
    // A delegated task names both hosts: `web01 -> localhost`. The failure is
    // the play host's.
    let host = rest[..end]
        .split(" -> ")
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    let after = &rest[end + 1..];
    let outcome = if after.contains("UNREACHABLE!") {
        Outcome::Unreachable
    } else {
        Outcome::Failed
    };
    let payload = after.split_once("=> ").map(|(_, json)| json.trim());
    Some((outcome, host, payload))
}

/// Records one failed result on the current task, once per task and host: a
/// looped task's first failed item gives the reason, and the `fatal:` line
/// that follows it adds nothing.
fn fail(report: &mut Report, outcome: Outcome, host: &str, result: Option<&Value>) {
    if report
        .failures
        .iter()
        .any(|f| f.resource == report.task && f.endpoint == host)
    {
        return;
    }
    let field = |name: &str| {
        result
            .and_then(Value::as_map)
            .and_then(|m| m.get(name))
            .map(|v| match v {
                Value::String(s) => s.trim().to_owned(),
                Value::Int(n) => n.to_string(),
                Value::Uint(n) => n.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default()
    };
    let msg = field("msg");
    let stderr = field("stderr");
    let rc = field("rc");
    // A command's own failure reads `non-zero return code`; its stderr says why.
    let summary = if !stderr.is_empty() && (msg.is_empty() || msg == "non-zero return code") {
        stderr.clone()
    } else {
        msg.clone()
    };
    let summary = summary.lines().next().unwrap_or("").to_owned();
    let cause = cause(outcome, &msg);
    let mut failure = Failure::new(cause, report.task.clone(), summary).endpoint(host);
    if !rc.is_empty() {
        failure = failure.code(format!("rc={rc}"));
    }
    if !stderr.is_empty() {
        failure = failure.detail(stderr);
    }
    report.failures.push(failure);
}

/// The cause word for a failed result, from its outcome and Ansible's message.
fn cause(outcome: Outcome, msg: &str) -> Cause {
    let msg = msg.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| msg.contains(w));
    if has(&[
        "permission denied (publickey",
        "authentication failed",
        "invalid/incorrect password",
        "missing sudo password",
        "incorrect sudo password",
    ]) {
        return Cause::AuthFailed;
    }
    if outcome == Outcome::Unreachable {
        return Cause::Unreachable;
    }
    if has(&["timed out", "timeout"]) {
        Cause::TimedOut
    } else if has(&[
        "couldn't resolve module",
        "unsupported parameters",
        "is not a valid attribute",
        "missing required arguments",
        "value of",
    ]) {
        Cause::InvalidConfig
    } else if has(&[
        "no module named",
        "failed to import",
        "is required for this module",
        "python interpreter",
    ]) {
        Cause::DependencyMissing
    } else {
        Cause::ResourceFailed
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

    fn read(lines: &[&str]) -> Report {
        let mut report = Report::default();
        for line in lines {
            read_line(&mut report, line, false);
        }
        report
    }

    #[test]
    fn a_failed_task_is_a_failure_of_that_task_on_that_host() {
        let report = read(&[
            "TASK [nginx_svc] ***",
            r#"fatal: [web01]: FAILED! => {"changed": false, "msg": "Could not find the requested service nginx: host"}"#,
        ]);
        let f = &report.failures[0];
        assert_eq!(
            (f.resource.as_str(), f.endpoint.as_str(), f.cause),
            ("nginx_svc", "web01", Cause::ResourceFailed)
        );
        assert_eq!(
            f.summary,
            "Could not find the requested service nginx: host"
        );
    }

    #[test]
    fn a_command_failure_is_summarised_by_its_stderr_and_coded_by_its_return_code() {
        let report = read(&[
            "TASK [migrate] ***",
            r#"fatal: [db01 -> localhost]: FAILED! => {"changed": true, "msg": "non-zero return code", "rc": 3, "stderr": "no such upstream\nsecond line"}"#,
        ]);
        let f = &report.failures[0];
        assert_eq!(f.endpoint, "db01");
        assert_eq!(f.summary, "no such upstream");
        assert_eq!(f.tool_code, "rc=3");
        assert_eq!(
            f.detail,
            "no such upstream
second line"
        );
    }

    #[test]
    fn an_unreachable_host_and_a_refused_login_have_their_own_causes() {
        let report = read(&[
            "TASK [Gathering Facts] ***",
            r#"fatal: [web01]: UNREACHABLE! => {"changed": false, "msg": "Failed to connect to the host via ssh: connect to host web01 port 22: Connection timed out", "unreachable": true}"#,
            r#"fatal: [web02]: UNREACHABLE! => {"changed": false, "msg": "Failed to connect to the host via ssh: deploy@web02: Permission denied (publickey,password).", "unreachable": true}"#,
            "fatal: [web03]: UNREACHABLE!",
        ]);
        let causes: Vec<Cause> = report.failures.iter().map(|f| f.cause).collect();
        assert_eq!(
            causes,
            [Cause::Unreachable, Cause::AuthFailed, Cause::Unreachable]
        );
    }

    #[test]
    fn a_looped_task_keeps_its_first_failed_items_reason() {
        let report = read(&[
            "TASK [packages] ***",
            r#"failed: [web01] (item=nginx) => {"ansible_loop_var": "item", "item": "nginx", "msg": "No package matching 'nginx' is available"}"#,
            r#"fatal: [web01]: FAILED! => {"msg": "One or more items failed"}"#,
        ]);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            report.failures[0].summary,
            "No package matching 'nginx' is available"
        );
    }

    #[test]
    fn an_indented_result_is_read_across_its_lines_and_an_ignored_failure_is_dropped() {
        let report = read(&[
            "TASK [conf] ***",
            "fatal: [web01]: FAILED! => {",
            r#"    "changed": false,"#,
            r#"    "msg": "Unsupported parameters for (ansible.builtin.copy) module: no_such_argument""#,
            "}",
            "TASK [optional] ***",
            r#"fatal: [web01]: FAILED! => {"msg": "absent"}"#,
            "...ignoring",
        ]);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].resource, "conf");
        assert_eq!(report.failures[0].cause, Cause::InvalidConfig);
    }
}
