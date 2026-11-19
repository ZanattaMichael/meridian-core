//! The half that applies a compiled manifest. It sits outside the compile
//! chain: nothing here is reached by a compile.
//!
//! Puppet itself has no way to push a catalog to a node, so there are two
//! ways to apply one. On the node itself, `puppet apply` does it. From
//! anywhere else, Bolt compiles the same manifest locally and applies it over
//! SSH or WinRM. The runner picks by the document's host and can be told.

use std::path::PathBuf;

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    agent_for, credential_for_this_machine, decode_value, AgentPrograms, Applied, Artifact, Change,
    ChangeAction, HeldCredential, Log, LogLine, LogStream, Operations, Part, Plan, RunContext,
    RunError, Runner, SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::{MANIFEST_PATH, SITE_PATH};

/// What `--tool-debug` adds to `puppet apply` on this machine, level 1 first.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["--evaltrace"]),
    meridian_sdk::debug::DebugLevel::args(&["--debug"]),
    meridian_sdk::debug::DebugLevel::args(&["--debug", "--trace"]),
];

/// What `--tool-debug` adds to `bolt apply`, level 1 first. Bolt logs to
/// standard error, so the JSON report on standard output is unchanged.
pub const BOLT_DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["--log-level", "info"]),
    meridian_sdk::debug::DebugLevel::args(&["--log-level", "debug"]),
    meridian_sdk::debug::DebugLevel::args(&["--log-level", "trace"]),
];

/// Names the `puppet` program, when it is not on `PATH`.
pub const PUPPET_VARIABLE: &str = "MERIDIAN_PUPPET";
/// Names the `bolt` program, when it is not on `PATH`.
pub const BOLT_VARIABLE: &str = "MERIDIAN_BOLT";
/// `local` or `bolt`, to override the choice made from the host.
pub const TRANSPORT_VARIABLE: &str = "MERIDIAN_PUPPET_TRANSPORT";
/// Extra arguments for every run, split like a shell command line.
pub const ARGS_VARIABLE: &str = "MERIDIAN_PUPPET_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    let mut declared = vec![
        Setting::new(
            "puppet",
            PUPPET_VARIABLE,
            "puppet",
            "The `puppet` program, when it is not on `PATH`.",
        ),
        Setting::new(
            "bolt",
            BOLT_VARIABLE,
            "bolt",
            "The `bolt` program, when it is not on `PATH`.",
        ),
        Setting::new(
            "transport",
            TRANSPORT_VARIABLE,
            "",
            "`local` or `bolt`, to override the choice made from the host: `local` applies on this machine with `puppet apply`, `bolt` reaches the host with `bolt apply`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every run, split like a shell command line.",
        ),
    ];
    declared.extend(meridian_sdk::agent::settings());
    declared
}

/// The manifest the runner hands to the tool: the compiled class followed by
/// the line that includes it. The compiled `site.pp` does the same through a
/// node statement, which only matches on a machine whose certificate name is
/// the host.
const APPLY_PATH: &str = "apply.pp";

/// How a manifest reaches its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// `puppet apply`, on the machine Meridian runs on.
    Local,
    /// `bolt apply`, against the document's host.
    Bolt,
}

/// Applies a compiled manifest.
///
/// A plan is the tool's own `--noop` run. Puppet reports no outputs.
pub struct PuppetRunner {
    pub puppet: String,
    pub bolt: String,
    /// `None` chooses by the host: this machine means [`Transport::Local`],
    /// anything else [`Transport::Bolt`].
    pub transport: Option<Transport>,
    /// Arguments added to every run, after Meridian's own.
    pub extra_args: Vec<String>,
    /// The programs an SSH agent is started with, when the document reaches
    /// its host as a credential with a key.
    pub agent: AgentPrograms,
}

impl PuppetRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        PuppetRunner {
            puppet: settings::text(setting("puppet")),
            bolt: settings::text(setting("bolt")),
            transport: match settings::text(setting("transport"))
                .to_ascii_lowercase()
                .as_str()
            {
                "local" => Some(Transport::Local),
                "bolt" => Some(Transport::Bolt),
                _ => None,
            },
            extra_args: settings::args(setting("args")),
            agent: AgentPrograms::from_settings(&declared),
        }
    }

    /// How a manifest for `host` would be applied: the forced transport, or
    /// the one the host implies.
    pub fn transport_for(&self, host: &str) -> Transport {
        self.transport
            .unwrap_or(if matches!(host, "" | "localhost" | "127.0.0.1" | "::1") {
                Transport::Local
            } else {
                Transport::Bolt
            })
    }

    fn run(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        noop: bool,
        log: &dyn Log,
    ) -> Result<Report, RunError> {
        let (Some(class_file), Some(site)) = (
            artifact.files.get(MANIFEST_PATH),
            artifact.files.get(SITE_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {MANIFEST_PATH} and {SITE_PATH}; it was not compiled by this plugin"
            )));
        };
        let Some(class) = site
            .lines()
            .find_map(|line| line.trim().strip_prefix("include "))
        else {
            return Err(RunError::new(format!(
                "{SITE_PATH} includes no class; it was not compiled by this plugin"
            )));
        };
        let mut on_disk = artifact.clone();
        on_disk.files.insert(
            APPLY_PATH.to_owned(),
            format!("{class_file}\ninclude {class}\n"),
        );
        write_artifact(&context.work_dir, &on_disk)?;

        // The document's secrets go to Puppet through Hiera: a data file
        // only the user can read, named by a hiera.yaml beside it, both
        // removed when the run ends. The manifest looks each up by name.
        let hiera = hiera_files(context)?;
        let config = hiera
            .as_ref()
            .map(|h| h.config.to_string_lossy().into_owned());
        let report = match self.transport_for(&context.host) {
            Transport::Local => match &context.credential {
                // puppet apply configures this machine as the user running
                // it, whoever the document names.
                Some(credential) => Err(credential_for_this_machine("puppet apply", credential)),
                None => self.run_puppet(context, noop, config.as_deref(), log),
            },
            Transport::Bolt => self.run_bolt(context, noop, config.as_deref(), log),
        };
        drop(hiera);
        report
    }

    fn run_puppet(
        &self,
        context: &RunContext,
        noop: bool,
        hiera_config: Option<&str>,
        log: &dyn Log,
    ) -> Result<Report, RunError> {
        let mut tool = Tool::new(&self.puppet)
            .args(["apply", "--detailed-exitcodes", "--color=false"])
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
            .dir(&context.work_dir);
        if noop {
            tool = tool.arg("--noop");
        }
        if let Some(config) = hiera_config {
            tool = tool.arg("--hiera_config").arg(config);
        }
        tool = tool.args(self.extra_args.iter().cloned()).arg(APPLY_PATH);

        let reader = Reader {
            inner: log,
            report: Default::default(),
        };
        let finished = tool.run(&reader)?;
        let report = reader
            .report
            .into_inner()
            .expect("no panic holds the report");
        // With --detailed-exitcodes, 2 means "changes were made" and is a
        // success; 4 and 6 mean a resource failed, 1 that nothing compiled.
        match finished.code {
            Some(0 | 2) => Ok(report),
            code => {
                let what = match code {
                    Some(4 | 6) => "puppet apply reported a failed resource",
                    _ => "puppet apply failed",
                };
                let mut error = finished.into_error(what);
                if !noop {
                    error.changes = report.changes;
                }
                Err(error)
            }
        }
    }

    fn run_bolt(
        &self,
        context: &RunContext,
        noop: bool,
        hiera_config: Option<&str>,
        log: &dyn Log,
    ) -> Result<Report, RunError> {
        // The core checked the host already; this runner is the last thing
        // between it and Bolt's command line.
        meridian_sdk::check_host(&context.host)
            .map_err(|why| RunError::new(format!("bolt cannot be given this target: {why}")))?;
        // The credential reaches Bolt as an inventory: the user and password
        // in a file only the user can read, removed when the run ends, and
        // the key through an agent Meridian starts, which Bolt is told of.
        let agent = agent_for(&self.agent, context.credential.as_ref(), log)?;
        let inventory = match &context.credential {
            Some(credential) => Some(inventory_file(&context.host, credential)?),
            None => None,
        };
        let mut tool = Tool::new(&self.bolt)
            .args(["apply", APPLY_PATH, "--targets", &context.host])
            .args(["--format", "json"])
            .dir(&context.work_dir)
            .capture_stdout();
        if noop {
            tool = tool.arg("--noop");
        }
        if let Some(config) = hiera_config {
            tool = tool.arg("--hiera-config").arg(config);
        }
        if let Some(inventory) = &inventory {
            tool = tool
                .arg("--inventoryfile")
                .arg(inventory.path().to_string_lossy().into_owned());
        }
        if let Some(agent) = &agent {
            tool = agent.tell(tool);
        }
        tool = tool
            .debug(meridian_sdk::debug::pick(BOLT_DEBUG, context.debug_level))
            .args(self.extra_args.iter().cloned());

        let finished = tool.run(log)?;
        drop(inventory);
        drop(agent);
        let report = read_bolt(&finished.stdout, noop);
        for change in &report.changes {
            log.out(&change.to_string());
        }
        if finished.success() {
            return Ok(report);
        }
        let mut error = finished.clone().into_error("bolt apply failed");
        if error.output.is_empty() {
            error.output = finished.stdout;
        }
        if !noop {
            error.changes = report.changes;
        }
        Err(error)
    }
}

/// Bolt's inventory for the host, as the credential says how to sign in:
/// the SSH transport, the user, and the password when there is one, written
/// as a JSON string, which YAML reads the same. A key is not here: it is in
/// the agent, which Bolt's SSH library reads from `SSH_AUTH_SOCK` as `ssh`
/// does. The file is in a directory only the user can enter, and is removed
/// when it is dropped.
fn inventory_file(host: &str, credential: &HeldCredential) -> Result<SecretFile, RunError> {
    let mut head = format!(
        "---\n# Written by Meridian for this run, and removed with it.\n\
         targets:\n  - uri: {}\n    config:\n      transport: ssh\n      ssh:\n",
        Value::String(host.to_owned()).canonical()
    );
    if let Some(user) = credential.text("user") {
        head.push_str(&format!(
            "        user: {}\n",
            Value::String(user.to_owned()).canonical()
        ));
    }
    let mut parts = vec![Part::Text(&head)];
    if let Some(password) = credential.secret("password") {
        parts.push(Part::Text("        password: \""));
        parts.push(Part::JsonString(password));
        parts.push(Part::Text("\"\n"));
    }
    SecretFile::create("inventory.yaml", &parts)
        .map_err(|e| RunError::new(format!("writing Bolt's inventory: {e}")))
}

/// The secrets as Hiera data: the data file, and the `hiera.yaml` that
/// names it, in a directory of their own that only the user can enter.
struct HieraFiles {
    /// Dropping it removes the directory and both files.
    _data: SecretFile,
    config: PathBuf,
}

/// Writes the document's secrets for Hiera, each under its key, or `None`
/// when the document has none. The data file is `common.yaml`: a YAML
/// mapping whose values are written as JSON strings, which YAML reads the
/// same. The value goes from protected memory to the file and nowhere else.
fn hiera_files(context: &RunContext) -> Result<Option<HieraFiles>, RunError> {
    if context.secrets.is_empty() {
        return Ok(None);
    }
    let keys: Vec<String> = context
        .secrets
        .iter()
        .map(|(name, _)| Value::String(crate::secret_key(name)).canonical())
        .collect();
    let mut parts = vec![Part::Text("---\n")];
    for (key, (_, value)) in keys.iter().zip(context.secrets.iter()) {
        parts.push(Part::Text(key));
        parts.push(Part::Text(": \""));
        parts.push(Part::JsonString(value));
        parts.push(Part::Text("\"\n"));
    }
    let data = SecretFile::create("common.yaml", &parts)
        .map_err(|e| RunError::new(format!("writing the Hiera data file: {e}")))?;
    let datadir = Value::String(data.dir().to_string_lossy().into_owned()).canonical();
    let config = data
        .beside(
            "hiera.yaml",
            &format!(
                "---\n# Written by Meridian for this run, and removed with it.\n\
                 version: 5\ndefaults:\n  datadir: {datadir}\n  data_hash: yaml_data\n\
                 hierarchy:\n  - name: \"Secrets handed over by Meridian\"\n    path: common.yaml\n"
            ),
        )
        .map_err(|e| RunError::new(format!("writing hiera.yaml: {e}")))?;
    Ok(Some(HieraFiles {
        _data: data,
        config,
    }))
}

impl Runner for PuppetRunner {
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

    fn more_debug_levels(
        &self,
    ) -> &'static [(&'static str, &'static [meridian_sdk::debug::DebugLevel])] {
        &[("bolt", BOLT_DEBUG)]
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
        let report = self.run(context, artifact, true, log)?;
        Ok(Plan {
            changes: report.changes,
            unknown: report.unreadable,
            saved: None,
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

#[derive(Default)]
struct Report {
    changes: Vec<Change>,
    /// The tool's report could not be read, so "no changes" is not known.
    unreadable: bool,
}

impl Report {
    /// Records one event against a resource, folding several properties of
    /// one resource into a single change.
    fn record(&mut self, resource: &str, property: &str, message: &str) {
        // A noop run says what a real one would, with a suffix. A new file
        // with content is "defined content as ...", not "created".
        let said = message.strip_suffix(" (noop)").unwrap_or(message);
        let action = if said == "created"
            || (property == "ensure" && said.starts_with("defined content as "))
            || message.contains("current_value 'absent'")
        {
            ChangeAction::Create
        } else if said == "removed" || message.contains("should be 'absent'") {
            ChangeAction::Delete
        } else {
            ChangeAction::Update
        };
        let detail = if property.is_empty() {
            message.to_owned()
        } else {
            format!("{property}: {message}")
        };
        match self.changes.iter_mut().find(|c| c.resource == resource) {
            Some(change) => {
                if action != ChangeAction::Update {
                    change.action = action;
                }
                change.summary.push_str("; ");
                change.summary.push_str(&detail);
            }
            None => self.changes.push(Change {
                resource: resource.to_owned(),
                action,
                summary: detail,
            }),
        }
    }
}

/// Relays every line and reads resource events out of `puppet apply`'s own
/// output on the way:
///
/// ```text
/// Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: created
/// Notice: /Stage[main]/Meridian_web/Service[app_svc]: Triggered 'refresh' from 1 event
/// ```
struct Reader<'a> {
    inner: &'a dyn Log,
    report: std::sync::Mutex<Report>,
}

impl Log for Reader<'_> {
    fn line(&self, line: LogLine) {
        if line.stream == LogStream::Stdout {
            if let Some((resource, property, message)) =
                read_notice(&line.text).filter(|(_, property, message)| is_event(property, message))
            {
                self.report
                    .lock()
                    .expect("no panic holds the report")
                    .record(resource, property, message);
            }
        }
        self.inner.line(line);
    }
}

/// The resource title, property and message of one resource event line.
fn read_notice(text: &str) -> Option<(&str, &str, &str)> {
    let path = text.strip_prefix("Notice: /Stage[main]/")?;
    // Past the class segment, to `Type[title]`.
    let declaration = &path[path.find('/')? + 1..];
    let title_start = declaration.find('[')? + 1;
    let rest = &declaration[title_start..];
    let slash = rest.find("]/");
    let colon = rest.find("]: ");
    let (title, after) = match (slash, colon) {
        // `Type[title]: message`, an event on the resource as a whole.
        (None, Some(colon)) => return Some((&rest[..colon], "", &rest[colon + 3..])),
        (Some(slash), Some(colon)) if colon < slash => {
            return Some((&rest[..colon], "", &rest[colon + 3..]))
        }
        (Some(slash), _) => (&rest[..slash], &rest[slash + 2..]),
        (None, None) => return None,
    };
    let (property, message) = after.split_once(": ")?;
    Some((title, property, message))
}

/// Whether a notice against a resource reports a change. Most do, and two
/// kinds do not: a command's own output, which Puppet logs against the
/// command's `returns` property, and a remark about a resource that was not
/// touched, such as one skipped because a dependency failed.
fn is_event(property: &str, message: &str) -> bool {
    match property {
        "" => {
            message.starts_with("Triggered 'refresh'")
                || message.starts_with("Would have triggered 'refresh'")
        }
        "returns" => message.strip_suffix(" (noop)").unwrap_or(message) == "executed successfully",
        _ => true,
    }
}

/// Reads the report Bolt prints with `--format json`: one item per target,
/// each carrying Puppet's own report with a status per resource.
fn read_bolt(stdout: &str, noop: bool) -> Report {
    let mut report = Report::default();
    let statuses = decode_value(stdout.trim().as_bytes()).ok().and_then(|v| {
        // `bolt apply` prints a list of results; `bolt task run` and older
        // releases wrap it as `{"items": [...]}`.
        let items = match v.as_map() {
            Some(map) => map.get("items")?.as_list()?.to_vec(),
            None => v.as_list()?.to_vec(),
        };
        let item = items.first()?.clone();
        let value = item.as_map()?.get("value")?.clone();
        let statuses = value
            .as_map()?
            .get("report")?
            .as_map()?
            .get("resource_statuses")?
            .clone();
        statuses.as_map().cloned()
    });
    let Some(statuses) = statuses else {
        report.unreadable = true;
        return report;
    };
    // The report keys statuses by name; the order Puppet evaluated them in
    // is each one's time, which sorts as text.
    let mut ordered: Vec<(&str, &String, &Value)> = statuses
        .iter()
        .map(|(name, status)| {
            let time = status
                .as_map()
                .and_then(|m| m.get("time"))
                .and_then(Value::as_str)
                .unwrap_or("");
            (time, name, status)
        })
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    for (_, name, status) in ordered {
        let Some(status) = status.as_map() else {
            continue;
        };
        // `Package[app_pkg]`: the title is the IR resource id.
        let title = name
            .split_once('[')
            .and_then(|(_, rest)| rest.strip_suffix(']'))
            .unwrap_or(name);
        let events = status.get("events").and_then(Value::as_list).unwrap_or(&[]);
        for event in events {
            let Some(event) = event.as_map() else {
                continue;
            };
            let text = |key: &str| event.get(key).and_then(Value::as_str).unwrap_or("");
            let wanted = if noop { "noop" } else { "success" };
            if text("status") == wanted {
                report.record(title, text("property"), text("message"));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_events_are_read_from_notices() {
        assert_eq!(
            read_notice("Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: created"),
            Some(("app_pkg", "ensure", "created"))
        );
        assert_eq!(
            read_notice(
                "Notice: /Stage[main]/Meridian_web/Service[app_svc]: Triggered 'refresh' from 1 event"
            ),
            Some(("app_svc", "", "Triggered 'refresh' from 1 event"))
        );
        assert_eq!(read_notice("Notice: Applied catalog in 0.42 seconds"), None);
        assert_eq!(
            read_notice(
                "Notice: Class[Meridian_web]: Would have triggered 'refresh' from 2 events"
            ),
            None
        );
    }

    /// Lines as `puppet apply` really prints them, with and without
    /// `--noop`.
    #[test]
    fn only_notices_that_report_a_change_are_events() {
        fn events(text: &str) -> Option<(&str, &str, &str)> {
            read_notice(text).filter(|(_, p, m)| is_event(p, m))
        }
        assert_eq!(
            events("Notice: /Stage[main]/Meridian_site/Exec[stamp]/returns: executed successfully"),
            Some(("stamp", "returns", "executed successfully"))
        );
        assert!(events(
            "Notice: /Stage[main]/Meridian_site/Exec[stamp]/returns: executed successfully (noop)"
        )
        .is_some());
        // A command's output is logged against the same property.
        assert_eq!(
            events("Notice: /Stage[main]/Meridian_site/Exec[broken]/returns: no such luck"),
            None
        );
        assert_eq!(
            events(
                "Notice: /Stage[main]/Meridian_site/File[after]: Dependency Exec[broken] has failures: true"
            ),
            None
        );
        assert!(events(
            "Notice: /Stage[main]/Meridian_site/Exec[reload]: Would have triggered 'refresh' from 1 event"
        )
        .is_some());
    }

    #[test]
    fn a_noop_run_is_read_as_the_real_one_would_be() {
        let mut report = Report::default();
        report.record("dir", "ensure", "created (noop)");
        report.record("conf", "ensure", "defined content as '{sha256}ca97' (noop)");
        report.record("conf2", "ensure", "defined content as '{sha256}ca97'");
        report.record("old", "ensure", "removed (noop)");
        report.record("drift", "mode", "mode changed '0644' to '0600' (noop)");
        let actions: Vec<ChangeAction> = report.changes.iter().map(|c| c.action).collect();
        assert_eq!(
            actions,
            [
                ChangeAction::Create,
                ChangeAction::Create,
                ChangeAction::Create,
                ChangeAction::Delete,
                ChangeAction::Update,
            ]
        );
    }

    #[test]
    fn several_properties_of_one_resource_are_one_change() {
        let mut report = Report::default();
        report.record("conf", "content", "content changed 'a' to 'b'");
        report.record("conf", "mode", "mode changed '0600' to '0640'");
        report.record(
            "pkg",
            "ensure",
            "current_value 'absent', should be 'present' (noop)",
        );
        report.record("old", "ensure", "removed");
        let shown: Vec<String> = report.changes.iter().map(Change::to_string).collect();
        assert_eq!(
            shown,
            [
                "update conf: content: content changed 'a' to 'b'; mode: mode changed '0600' to '0640'",
                "create pkg: ensure: current_value 'absent', should be 'present' (noop)",
                "delete old: ensure: removed",
            ]
        );
    }
}
