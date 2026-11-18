//! The half that applies a compiled state file. It sits outside the compile
//! chain: nothing here is reached by a compile.
//!
//! There are two ways to apply a state file without a Salt master. On the
//! node itself, `salt-call --local` does it. From anywhere else, `salt-ssh`
//! copies the states over SSH and applies them there, with no minion
//! installed. The runner picks by the document's host and can be told.

use std::path::{Path, PathBuf};

use meridian_sdk::tool::{write_artifact, Finished, Tool};
use meridian_sdk::{
    agent_for, credential_for_this_machine, decode_value, AgentPrograms, Applied, Artifact, Change,
    ChangeAction, HeldCredential, Log, Operations, Part, Plan, RunContext, RunError, Runner,
    SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::{STATES_DIR, TOP_PATH};

/// Names the `salt-call` program, when it is not on `PATH`.
pub const SALT_CALL_VARIABLE: &str = "MERIDIAN_SALT_CALL";
/// Names the `salt-ssh` program, when it is not on `PATH`.
pub const SALT_SSH_VARIABLE: &str = "MERIDIAN_SALT_SSH";
/// `local` or `ssh`, to override the choice made from the host.
pub const TRANSPORT_VARIABLE: &str = "MERIDIAN_SALT_TRANSPORT";
/// Extra arguments for every run, split like a shell command line. For
/// example `--config-dir /srv/salt-config` locally, or `--user deploy --priv
/// /home/me/.ssh/deploy -i` over SSH.
pub const ARGS_VARIABLE: &str = "MERIDIAN_SALT_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    let mut declared = vec![
        Setting::new(
            "salt-call",
            SALT_CALL_VARIABLE,
            "salt-call",
            "The `salt-call` program, when it is not on `PATH`.",
        ),
        Setting::new(
            "salt-ssh",
            SALT_SSH_VARIABLE,
            "salt-ssh",
            "The `salt-ssh` program, when it is not on `PATH`.",
        ),
        Setting::new(
            "transport",
            TRANSPORT_VARIABLE,
            "",
            "`local` or `ssh`, to override the choice made from the host: `local` applies on this machine with `salt-call --local`, `ssh` reaches the host with `salt-ssh`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every run, split like a shell command line. For example `--config-dir /srv/salt-config` locally, or `--user deploy --priv /home/me/.ssh/deploy -i` over SSH.",
        ),
    ];
    declared.extend(meridian_sdk::agent::settings());
    declared
}

/// The files `salt-ssh` needs beside the states: its configuration, which
/// points it at the compiled file root and keeps its cache in the work
/// directory, and the roster that tells it where the host is.
const SSH_CONFIG_PATH: &str = "ssh/master";
const SSH_ROSTER_PATH: &str = "ssh/roster";

/// How a state file reaches its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// `salt-call --local`, on the machine Meridian runs on.
    Local,
    /// `salt-ssh`, against the document's host.
    Ssh,
}

/// Applies a compiled state file.
///
/// A plan is the tool's own dry run, `test=True`. An apply is the same
/// command without it. Salt reports no outputs.
pub struct SaltRunner {
    pub salt_call: String,
    pub salt_ssh: String,
    /// `None` chooses by the host: this machine means [`Transport::Local`],
    /// anything else [`Transport::Ssh`].
    pub transport: Option<Transport>,
    /// Arguments added to every run, after Meridian's own options.
    pub extra_args: Vec<String>,
    /// The programs an SSH agent is started with, when the document reaches
    /// its host as a credential with a key.
    pub agent: AgentPrograms,
}

impl SaltRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        SaltRunner {
            salt_call: settings::text(setting("salt-call")),
            salt_ssh: settings::text(setting("salt-ssh")),
            transport: match settings::text(setting("transport"))
                .to_ascii_lowercase()
                .as_str()
            {
                "local" => Some(Transport::Local),
                "ssh" => Some(Transport::Ssh),
                _ => None,
            },
            extra_args: settings::args(setting("args")),
            agent: AgentPrograms::from_settings(&declared),
        }
    }

    fn transport_for(&self, host: &str) -> Transport {
        self.transport
            .unwrap_or(if matches!(host, "" | "localhost" | "127.0.0.1" | "::1") {
                Transport::Local
            } else {
                Transport::Ssh
            })
    }

    fn run(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        test: bool,
        log: &dyn Log,
    ) -> Result<Report, RunError> {
        let sls = sls_name(artifact)?;
        let transport = self.transport_for(&context.host);
        if transport == Transport::Ssh && context.host.is_empty() {
            return Err(RunError::new(
                "salt-ssh needs a host to connect to, and the document names none",
            ));
        }
        if transport == Transport::Ssh {
            // The core checked the host already; this runner is the last
            // thing between it and salt-ssh's command line and roster.
            meridian_sdk::check_host(&context.host).map_err(|why| {
                RunError::new(format!("salt-ssh cannot be given this host: {why}"))
            })?;
        } else if let Some(credential) = &context.credential {
            // salt-call configures this machine as the user running it,
            // whoever the document names.
            return Err(credential_for_this_machine("salt-call", credential));
        }
        // The credential's key, when it has one, is in an agent for the
        // length of the run, and salt-ssh is told where the agent is.
        let agent = match transport {
            Transport::Ssh => agent_for(&self.agent, context.credential.as_ref(), log)?,
            Transport::Local => None,
        };

        // Salt wants its file root as an absolute path, and the work
        // directory can only be made absolute once it exists.
        std::fs::create_dir_all(&context.work_dir).map_err(|e| {
            RunError::new(format!(
                "could not create {}: {e}",
                context.work_dir.display()
            ))
        })?;
        let work = absolute(&context.work_dir);
        // The document's secrets go to Salt as pillar data, from a pillar
        // root only the user can read, removed when the run ends. The states
        // read each by its key.
        let pillar = pillar_root(context)?;
        let pillar_dir = pillar.as_ref().map(|p| p.dir().to_path_buf());
        let mut on_disk = artifact.clone();
        // The roster is in the work directory, unless it holds the
        // credential's password: then it is a file only the user can read,
        // removed when the run ends, and the work directory never holds it.
        let mut roster_path = work.join(SSH_ROSTER_PATH);
        let mut private_roster = None;
        if transport == Transport::Ssh {
            on_disk.files.insert(
                SSH_CONFIG_PATH.to_owned(),
                ssh_config(&work, pillar_dir.as_deref()),
            );
            let roster = ssh_roster(&context.host, context.credential.as_ref());
            match context
                .credential
                .as_ref()
                .and_then(|c| c.secret("password"))
            {
                Some(password) => {
                    let parts = [
                        Part::Text(&roster),
                        Part::Text("  passwd: \""),
                        Part::JsonString(password),
                        Part::Text("\"\n"),
                    ];
                    let file = SecretFile::create("roster", &parts)
                        .map_err(|e| RunError::new(format!("writing the roster: {e}")))?;
                    roster_path = file.path().to_path_buf();
                    private_roster = Some(file);
                }
                None => {
                    on_disk.files.insert(SSH_ROSTER_PATH.to_owned(), roster);
                }
            }
        }
        write_artifact(&work, &on_disk)?;

        let (program, tool) = match transport {
            Transport::Local => ("salt-call", {
                let mut tool = Tool::new(&self.salt_call)
                    .arg("--local")
                    .arg("--file-root")
                    .arg(path_text(&work.join(STATES_DIR)));
                if let Some(dir) = &pillar_dir {
                    tool = tool.arg("--pillar-root").arg(path_text(dir));
                }
                tool.args(["--out", "json", "--no-color", "--retcode-passthrough"])
                    .args(self.extra_args.iter().cloned())
            }),
            Transport::Ssh => ("salt-ssh", {
                let tool = Tool::new(&self.salt_ssh)
                    .arg("--config-dir")
                    .arg(path_text(&work.join("ssh")))
                    .arg("--roster-file")
                    .arg(path_text(&roster_path))
                    .args(["--out", "json", "--no-color"]);
                let tool = match &agent {
                    Some(agent) => agent.tell(tool),
                    None => tool,
                };
                tool.args(self.extra_args.iter().cloned())
                    .arg(&context.host)
            }),
        };
        let mut tool = tool.args(["state.apply", sls.as_str()]);
        if test {
            tool = tool.arg("test=True");
        }
        // The result is one JSON document printed at the end, so it is data
        // and not something to relay. What it says is relayed afterwards, a
        // line per state.
        let finished = tool.dir(&work).capture_stdout().run(log)?;
        drop(pillar);
        drop(private_roster);
        drop(agent);
        read_result(program, finished, test, log)
    }
}

impl Runner for SaltRunner {
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
        let report = self.run(context, artifact, true, log)?;
        Ok(Plan {
            changes: report.changes,
            unknown: false,
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

/// The name the artifact's state file is applied by: the one file under
/// `states/` that is not the top file.
fn sls_name(artifact: &Artifact) -> Result<String, RunError> {
    let foreign = || {
        RunError::new(format!(
            "the artifact has no {TOP_PATH} and one state file beside it; \
             it was not compiled by this plugin"
        ))
    };
    if !artifact.files.contains_key(TOP_PATH) {
        return Err(foreign());
    }
    let prefix = format!("{STATES_DIR}/");
    let mut names = artifact
        .files
        .keys()
        .filter(|path| *path != TOP_PATH)
        .filter_map(|path| path.strip_prefix(&prefix)?.strip_suffix(".sls"))
        .filter(|name| !name.contains('/'));
    match (names.next(), names.next()) {
        (Some(name), None) => Ok(name.to_owned()),
        _ => Err(foreign()),
    }
}

fn absolute(dir: &Path) -> PathBuf {
    meridian_sdk::tool::absolute_path(dir)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A JSON string is a YAML string, and a path can hold anything.
fn yaml_text(text: &str) -> String {
    Value::String(text.to_owned()).canonical()
}

fn ssh_config(work: &Path, pillar_dir: Option<&Path>) -> String {
    let mut config = format!(
        "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\
         root_dir: {}\n\
         file_roots:\n  base:\n    - {}\n",
        yaml_text(&path_text(&work.join("ssh/root"))),
        yaml_text(&path_text(&work.join(STATES_DIR))),
    );
    if let Some(dir) = pillar_dir {
        config.push_str(&format!(
            "pillar_roots:\n  base:\n    - {}\n",
            yaml_text(&path_text(dir))
        ));
    }
    config
}

/// Writes the document's secrets as a pillar root: a pillar file with each
/// under its key, and the top file that applies it to every minion, in a
/// directory only the user can enter. `None` when the document has none.
/// A value is written as a JSON string, which YAML reads the same, and goes
/// from protected memory to the file and nowhere else.
fn pillar_root(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
    if context.secrets.is_empty() {
        return Ok(None);
    }
    let keys: Vec<String> = context
        .secrets
        .iter()
        .map(|(name, _)| Value::String(crate::secret_key(name)).canonical())
        .collect();
    let mut parts = vec![Part::Text("#!yaml\n")];
    for (key, (_, value)) in keys.iter().zip(context.secrets.iter()) {
        parts.push(Part::Text(key));
        parts.push(Part::Text(": \""));
        parts.push(Part::JsonString(value));
        parts.push(Part::Text("\"\n"));
    }
    let file = SecretFile::create(&format!("{}.sls", crate::PILLAR_NAME), &parts)
        .map_err(|e| RunError::new(format!("writing the pillar file: {e}")))?;
    file.beside(
        "top.sls",
        &format!("#!yaml\nbase:\n  '*':\n    - {}\n", crate::PILLAR_NAME),
    )
    .map_err(|e| RunError::new(format!("writing the pillar top file: {e}")))?;
    Ok(Some(file))
}

/// The roster entry for the host, with what the credential says about how to
/// sign in: its user, and `priv: agent-forwarding` when its key is in the
/// agent Meridian started or, with `agent: true`, in the user's own. A
/// password is not here: the caller appends it, and keeps the file private.
fn ssh_roster(host: &str, credential: Option<&HeldCredential>) -> String {
    let mut roster = format!(
        "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\
         {}:\n  host: {}\n",
        yaml_text(host),
        yaml_text(host),
    );
    if let Some(credential) = credential {
        if let Some(user) = credential.text("user") {
            roster.push_str(&format!("  user: {}\n", yaml_text(user)));
        }
        if credential.secret("privateKey").is_some() || credential.flag("agent") {
            roster.push_str("  priv: agent-forwarding\n");
        }
    }
    roster
}

/// What a run said.
#[derive(Debug, Default, PartialEq)]
struct Report {
    changes: Vec<Change>,
}

/// One state's entry in Salt's result.
struct Outcome {
    id: String,
    function: String,
    /// `Some(true)` succeeded, `Some(false)` failed, and `None` is a dry
    /// run's "this would change".
    result: Option<bool>,
    changed: bool,
    comment: String,
    run_num: i64,
}

/// Reads the JSON document a run printed, relays a line per state, and turns
/// it into a report or the failure it amounts to.
fn read_result(
    program: &str,
    finished: Finished,
    test: bool,
    log: &dyn Log,
) -> Result<Report, RunError> {
    let failure = |msg: String, output: String| {
        let mut error = finished.clone().into_error(msg);
        error.output = output;
        error
    };
    let printed = finished.stdout.trim();
    let decoded = (!printed.is_empty())
        .then(|| decode_value(printed.as_bytes()).ok())
        .flatten();
    let Some(document) = decoded else {
        let said = format!("{}{}", finished.stdout, finished.stderr);
        return Err(if finished.success() {
            failure(
                format!("{program} printed something that is not its JSON result"),
                said,
            )
        } else {
            failure(format!("{program} failed"), said)
        });
    };

    // The result is keyed by the minion: `local` for salt-call, the host for
    // salt-ssh. There is one, because one host was asked.
    let body = match &document {
        Value::Map(map) if map.len() == 1 => map.values().next().expect("one entry"),
        other => other,
    };
    let body = match body {
        // salt-ssh wraps a host's answer when it carries more than the result.
        Value::Map(map) if map.contains_key("return") => &map["return"],
        other => other,
    };
    let states = match body {
        Value::Map(map) if map.values().all(is_state_result) => map,
        // A file that would not render or compile comes back as a list of
        // the reasons.
        Value::List(reasons) => {
            let text: String = reasons.iter().map(|r| format!("{}\n", plain(r))).collect();
            return Err(failure(
                format!("{program} could not compile the state file"),
                text,
            ));
        }
        Value::Map(map) if map.contains_key("stderr") || map.contains_key("stdout") => {
            let text = format!(
                "{}{}",
                map.get("stdout").map(plain).unwrap_or_default(),
                map.get("stderr").map(plain).unwrap_or_default()
            );
            return Err(failure(
                format!("{program} could not run on the host"),
                text,
            ));
        }
        Value::String(text) => {
            return Err(failure(
                format!("{program} could not run on the host"),
                text.clone(),
            ));
        }
        other => {
            return Err(failure(
                format!("{program} returned a result that is not a set of states"),
                other.canonical(),
            ));
        }
    };

    let mut outcomes: Vec<Outcome> = states
        .iter()
        .map(|(key, value)| outcome(key, value))
        .collect();
    outcomes.sort_by(|a, b| (a.run_num, &a.id).cmp(&(b.run_num, &b.id)));

    let mut report = Report::default();
    let mut failed = Vec::new();
    for outcome in &outcomes {
        let summary = outcome.comment.lines().next().unwrap_or_default().trim();
        let status = match outcome.result {
            Some(false) => "failed",
            None => "would change",
            Some(true) if outcome.changed && test => "would change",
            Some(true) if outcome.changed => "changed",
            Some(true) => "ok",
        };
        let line = format!("{} ({}): {status}", outcome.id, outcome.function);
        if summary.is_empty() {
            log.out(&line);
        } else {
            log.out(&format!("{line}: {summary}"));
        }
        match outcome.result {
            Some(false) => failed.push(format!("{}: {}", outcome.id, outcome.comment.trim())),
            _ if status != "ok" => report.changes.push(Change {
                resource: outcome.id.clone(),
                action: if outcome.function.ends_with(".absent")
                    || outcome.function.ends_with(".removed")
                {
                    ChangeAction::Delete
                } else {
                    ChangeAction::Update
                },
                summary: if summary.is_empty() {
                    status.to_owned()
                } else {
                    summary.to_owned()
                },
            }),
            _ => {}
        }
    }

    if failed.is_empty() {
        return Ok(report);
    }
    let what = if failed.len() == 1 {
        format!("{program} reported a failed state")
    } else {
        format!("{program} reported {} failed states", failed.len())
    };
    let mut error = failure(what, failed.join("\n") + "\n");
    if !test {
        error.changes = report.changes;
    }
    Err(error)
}

/// Whether a value looks like one state's result and not something else a
/// host may answer with.
fn is_state_result(value: &Value) -> bool {
    value
        .as_map()
        .is_some_and(|m| m.contains_key("result") && m.contains_key("comment"))
}

/// Reads one entry. Its key is `module_|-id_|-name_|-function`; the ID is
/// also a field of its own, which is used when it is there because an ID may
/// hold the separator.
fn outcome(key: &str, value: &Value) -> Outcome {
    let empty = Default::default();
    let map = value.as_map().unwrap_or(&empty);
    let parts: Vec<&str> = key.split("_|-").collect();
    let module = parts.first().copied().unwrap_or_default();
    let function = parts.last().copied().unwrap_or_default();
    Outcome {
        id: match map.get("__id__") {
            Some(Value::String(id)) => id.clone(),
            _ => parts.get(1).copied().unwrap_or(key).to_owned(),
        },
        function: format!("{module}.{function}"),
        result: match map.get("result") {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        },
        changed: match map.get("changes") {
            Some(Value::Map(changes)) => !changes.is_empty(),
            Some(Value::Null) | None => false,
            Some(_) => true,
        },
        comment: map.get("comment").map(plain).unwrap_or_default(),
        run_num: match map.get("__run_num__") {
            Some(Value::Int(n)) => *n,
            Some(Value::Uint(n)) => i64::try_from(*n).unwrap_or(i64::MAX),
            _ => i64::MAX,
        },
    }
}

/// A value as text a person reads: a string as it is, anything else as JSON.
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.canonical(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::NoLog;

    fn artifact(paths: &[&str]) -> Artifact {
        Artifact {
            files: paths
                .iter()
                .map(|p| ((*p).to_owned(), String::new()))
                .collect(),
        }
    }

    #[test]
    fn the_state_file_is_the_one_beside_the_top_file() {
        assert_eq!(
            sls_name(&artifact(&["states/top.sls", "states/web.sls"])).unwrap(),
            "web"
        );
        for foreign in [
            &["states/web.sls"][..],
            &["states/top.sls"],
            &["states/top.sls", "states/a.sls", "states/b.sls"],
            &["states/top.sls", "states/dir/a.sls"],
            &["playbook.yml"],
        ] {
            let err = sls_name(&artifact(foreign)).unwrap_err();
            assert!(
                err.msg.contains("not compiled by this plugin"),
                "{}",
                err.msg
            );
        }
    }

    #[test]
    fn the_host_chooses_the_transport_unless_told() {
        let mut runner = SaltRunner {
            salt_call: "salt-call".into(),
            salt_ssh: "salt-ssh".into(),
            transport: None,
            extra_args: Vec::new(),
            agent: AgentPrograms::default(),
        };
        assert_eq!(runner.transport_for("localhost"), Transport::Local);
        assert_eq!(runner.transport_for("127.0.0.1"), Transport::Local);
        assert_eq!(runner.transport_for(""), Transport::Local);
        assert_eq!(runner.transport_for("web01.example.com"), Transport::Ssh);
        runner.transport = Some(Transport::Local);
        assert_eq!(runner.transport_for("web01.example.com"), Transport::Local);
    }

    #[test]
    fn the_ssh_files_quote_what_they_are_given() {
        assert_eq!(
            ssh_roster("web01", None),
            "# Written by Meridian for salt-ssh. It is rewritten on every run.\n\"web01\":\n  host: \"web01\"\n"
        );
        // The paths are spelled the Unix way: salt-ssh, which reads this
        // file, does not run from a Windows machine.
        if cfg!(unix) {
            let config = ssh_config(Path::new("/work dir"), None);
            assert!(
                config.contains("root_dir: \"/work dir/ssh/root\"\n"),
                "{config}"
            );
            assert!(config.contains("    - \"/work dir/states\"\n"), "{config}");
        }
    }

    #[test]
    fn an_id_holding_the_separator_is_read_from_its_own_field() {
        let value = decode_value(
            br#"{"__id__": "odd_|-id", "result": true, "comment": "fine", "changes": {}, "__run_num__": 3}"#,
        )
        .unwrap();
        let outcome = outcome("cmd_|-odd_|-id_|-/bin/true_|-run", &value);
        assert_eq!(outcome.id, "odd_|-id");
        assert_eq!(outcome.function, "cmd.run");
        assert_eq!(outcome.run_num, 3);
        assert!(!outcome.changed);
    }

    #[test]
    fn output_that_is_not_json_is_a_failure_either_way() {
        for (code, want) in [
            (0, "salt-call printed something that is not its JSON result"),
            (1, "salt-call failed"),
        ] {
            let mut finished = Finished::default();
            finished.code = Some(code);
            finished.stdout = "Traceback".into();
            let err = read_result("salt-call", finished, true, &NoLog).unwrap_err();
            assert_eq!(err.msg, want);
            assert_eq!(err.output, "Traceback");
        }
    }
}
