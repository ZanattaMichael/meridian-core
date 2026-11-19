//! The half that converges a compiled cookbook. It sits outside the compile
//! chain: nothing here is reached by a compile.
//!
//! There are two ways to converge one. On the node itself, `chef-client` in
//! local mode does it, with no Chef server. From anywhere else, Chef
//! Workstation's `chef-run` copies the cookbook to the node and converges it
//! there over SSH or WinRM. The runner picks by the document's host and can
//! be told.

use std::path::PathBuf;

use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::tool::{absolute_path, write_artifact, Tool};
use meridian_sdk::{
    agent_for, credential_for_this_machine, AgentPrograms, Applied, Artifact, Change, ChangeAction,
    Log, Operations, Plan, RunContext, RunError, Runner, SecretFile,
};
use meridian_sdk::{settings, Setting};

/// What `--tool-debug` adds to `chef-client` on this machine, level 1 first.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["-l", "info"]),
    meridian_sdk::debug::DebugLevel::args(&["-l", "debug"]),
    meridian_sdk::debug::DebugLevel::args(&["-l", "trace"]),
];

/// What `--tool-debug` does to `chef-run`, which has no log flag: a run
/// gets a config file with this `[log] level`, and its log is copied into
/// the step's output when it ends. Chef Workstation has no `trace`.
pub const CHEF_RUN_DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::does("[log] level info"),
    meridian_sdk::debug::DebugLevel::does("[log] level debug"),
];

/// Names the `chef-client` program, when it is not on `PATH`. Cinc's
/// `cinc-client` takes the same arguments.
pub const CLIENT_VARIABLE: &str = "MERIDIAN_CHEF_CLIENT";
/// Names the `chef-run` program, when it is not on `PATH`.
pub const RUN_VARIABLE: &str = "MERIDIAN_CHEF_RUN";
/// `local` or `remote`, to override the choice made from the host.
pub const TRANSPORT_VARIABLE: &str = "MERIDIAN_CHEF_TRANSPORT";
/// Extra arguments for every run, split like a shell command line.
pub const ARGS_VARIABLE: &str = "MERIDIAN_CHEF_ARGS";
/// Names the resource inspector `meridian schema sync` runs.
pub const INSPECTOR_VARIABLE: &str = "MERIDIAN_CHEF_INSPECTOR";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    let mut declared = vec![
        Setting::new(
            "client",
            CLIENT_VARIABLE,
            "chef-client",
            "The `chef-client` program, when it is not on `PATH`. Cinc's `cinc-client` takes the same arguments.",
        ),
        Setting::new(
            "run",
            RUN_VARIABLE,
            "chef-run",
            "The `chef-run` program, when it is not on `PATH`.",
        ),
        Setting::new(
            "transport",
            TRANSPORT_VARIABLE,
            "",
            "`local` or `remote`, to override the choice made from the host: `local` converges this machine with chef-client, `remote` reaches the host with chef-run.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every run, split like a shell command line.",
        ),
        Setting::new(
            "inspector",
            INSPECTOR_VARIABLE,
            "chef-resource-inspector",
            "The resource inspector `meridian schema sync` runs to read what each resource takes, when it is not `chef-resource-inspector` on `PATH`; `cinc-resource-inspector`, then the copies the Cinc and Chef packages keep under `/opt/cinc` and `/opt/chef`, are tried when that does not run.",
        ),
    ];
    declared.extend(meridian_sdk::agent::settings());
    declared
}

/// How a cookbook reaches its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// `chef-client --local-mode`, on the machine Meridian runs on.
    Local,
    /// `chef-run`, against the document's host.
    Remote,
}

/// Converges a compiled cookbook.
///
/// A plan is `chef-client --why-run`. `chef-run` has no such mode, so a plan
/// for another machine says that it is unknown instead of guessing. Chef
/// reports no outputs.
pub struct ChefRunner {
    pub client: String,
    pub run: String,
    /// `None` chooses by the host: this machine means [`Transport::Local`],
    /// anything else [`Transport::Remote`].
    pub transport: Option<Transport>,
    /// Arguments added to every run, after Meridian's own.
    pub extra_args: Vec<String>,
    /// The programs an SSH agent is started with, when the document reaches
    /// its host as a credential with a key.
    pub agent: AgentPrograms,
}

impl ChefRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        ChefRunner {
            client: settings::text(setting("client")),
            run: settings::text(setting("run")),
            transport: match settings::text(setting("transport"))
                .to_ascii_lowercase()
                .as_str()
            {
                "local" => Some(Transport::Local),
                "remote" => Some(Transport::Remote),
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
                Transport::Remote
            })
    }

    /// Writes the cookbook to the work directory and names it.
    fn prepare(&self, context: &RunContext, artifact: &Artifact) -> Result<String, RunError> {
        let cookbook = artifact.files.keys().find_map(|path| {
            path.strip_prefix("cookbooks/")?
                .strip_suffix("/recipes/default.rb")
        });
        let Some(cookbook) = cookbook else {
            return Err(RunError::new(
                "the artifact holds no cookbook; it was not compiled by this plugin",
            ));
        };
        write_artifact(&context.work_dir, artifact)?;
        Ok(cookbook.to_owned())
    }

    fn converge_here(
        &self,
        context: &RunContext,
        cookbook: &str,
        why_run: bool,
        log: &dyn Log,
    ) -> Result<(Vec<Change>, String), RunError> {
        if let Some(credential) = &context.credential {
            // chef-client configures this machine as the user running it,
            // whoever the document names.
            return Err(credential_for_this_machine("chef-client", credential));
        }
        let attributes = secret_attributes(context)?;
        let mut tool = Tool::new(&self.client)
            // Local mode takes the directory it runs in for the repository
            // when it holds a `cookbooks` directory, which the work
            // directory does.
            .args(["--local-mode", "--no-color", "--force-formatter"])
            .args(["--override-runlist", &format!("recipe[{cookbook}]")])
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
            .dir(&context.work_dir);
        if let Some(attributes) = &attributes {
            // The attributes file is read as the node's JSON attributes, and
            // the configuration beside it keeps the node object local mode
            // writes back, which copies every attribute, out of the work
            // directory and in the private one.
            tool = tool
                .args(["--config", &attributes.config.to_string_lossy()])
                .args([
                    "--json-attributes",
                    &attributes.file.path().to_string_lossy(),
                ]);
        }
        if why_run {
            tool = tool.arg("--why-run");
        }
        let finished = tool.args(self.extra_args.iter().cloned()).run(log)?;
        drop(attributes);
        let changes = read_report(&finished.stdout);
        if finished.success() {
            return Ok((changes, finished.stdout));
        }
        let failures = read_failures(&finished.stdout);
        let mut error = finished.into_error("chef-client could not converge the cookbook");
        error.failures = failures;
        if !why_run {
            error.changes = changes;
        }
        Err(error)
    }
}

/// The document's secrets as node attributes, in files only the user can
/// read, and the client configuration that goes with them.
struct SecretAttributes {
    /// `attributes.json`: each secret under [`crate::secret_key`].
    file: SecretFile,
    /// `client.rb` beside it, which names the repository and keeps the node
    /// file in the same private directory.
    config: PathBuf,
}

/// Writes the secrets for a local run. `None` when the document has none,
/// so a run without secrets is exactly what it was before.
fn secret_attributes(context: &RunContext) -> Result<Option<SecretAttributes>, RunError> {
    if context.secrets.is_empty() {
        return Ok(None);
    }
    let file = context
        .secrets
        .json_file("attributes.json", crate::secret_key)
        .map_err(|e| RunError::new(format!("writing the attributes file: {e}")))?;
    // Local mode writes the node, attributes and all, to `node_path` after
    // the run. The default is `nodes` under the repository, which is the
    // work directory that lives on after the run; this keeps it private and
    // short-lived.
    let text = format!(
        "chef_repo_path '{}'
node_path '{}'
",
        ruby_string(&absolute_path(&context.work_dir).to_string_lossy()),
        ruby_string(&file.dir().to_string_lossy()),
    );
    let config = file
        .beside("client.rb", &text)
        .map_err(|e| RunError::new(format!("writing the client configuration: {e}")))?;
    Ok(Some(SecretAttributes { file, config }))
}

/// `text` as the body of a single-quoted Ruby string.
fn ruby_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Reads chef-client's report. Each resource is a line such as
/// `* file[nginx_conf] action create`, followed by a `- ` line for each thing
/// it changed, or closed with `(up to date)` or `(skipped due to only_if)`
/// when it changed nothing.
/// Other bullets under a resource are Chef's why-run assumptions.
fn read_report(stdout: &str) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(step) = line.strip_prefix("* ") {
            // A resource's header is `type[name] action <action>`. In why-run
            // mode Chef also writes its assumptions as bullets under the
            // resource they belong to ("* Parent directory /srv/app does not
            // exist."); those say nothing about which resource is current.
            let header = step.split_once(" action ").and_then(|(resource, action)| {
                let name = resource
                    .split_once('[')
                    .and_then(|(_, rest)| rest.strip_suffix(']'))?;
                Some((name, action))
            });
            let Some((name, action)) = header else {
                continue;
            };
            current = (!action.contains('(')).then(|| (name.to_owned(), action.trim().to_owned()));
        } else if line.starts_with("- ") {
            let Some((name, action)) = current.take() else {
                continue;
            };
            // A user's group memberships are named after the user.
            let resource = name.split_once(':').map_or(name.as_str(), |(id, _)| id);
            if changes.iter().any(|c| c.resource == resource) {
                continue;
            }
            changes.push(Change {
                resource: resource.to_owned(),
                action: match action.as_str() {
                    "delete" | "remove" => ChangeAction::Delete,
                    _ => ChangeAction::Update,
                },
                summary: action,
            });
        }
    }
    changes
}

/// The lines why-run printed under `resource`: what Chef would do to it.
fn would_do(stdout: &str, resource: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = false;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(step) = line.strip_prefix("* ") {
            if let Some((header, _)) = step.split_once(" action ") {
                let name = header
                    .split_once('[')
                    .and_then(|(_, rest)| rest.strip_suffix(']'))
                    .unwrap_or("");
                current = name.split_once(':').map_or(name, |(id, _)| id) == resource;
            }
        } else if let Some(said) = line.strip_prefix("- ").filter(|_| current) {
            lines.push(said.to_owned());
        }
    }
    lines
}

/// Reads the failures chef-client's formatter prints. A failed resource is
///
/// ```text
/// Error executing action `run` on resource 'execute[migrate]'
/// ======================================================================
///
/// Mixlib::ShellOut::ShellCommandFailed
/// ------------------------------------
/// Expected process to exit with [0], but received '3'
/// ```
///
/// and a recipe that did not compile is `Recipe Compile Error in ...`
/// followed the same way by the exception and its message.
fn read_failures(stdout: &str) -> Vec<Failure> {
    let mut failures: Vec<Failure> = Vec::new();
    // The resource the error is about ("" for the recipe), and the
    // exception once read.
    let mut pending: Option<(String, Option<String>)> = None;
    for line in stdout.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Error executing action `") {
            let name = rest
                .split_once("on resource '")
                .and_then(|(_, r)| r.strip_suffix('\''))
                .and_then(|r| r.split_once('['))
                .and_then(|(_, r)| r.strip_suffix(']'))
                .unwrap_or_default();
            // A user's group memberships are named after the user.
            let id = name.split_once(':').map_or(name, |(id, _)| id);
            pending = Some((id.to_owned(), None));
            continue;
        }
        if line.starts_with("Recipe Compile Error") {
            pending = Some((String::new(), None));
            continue;
        }
        let Some((resource, exception)) = &mut pending else {
            continue;
        };
        if line.is_empty() || line.starts_with('=') || line.starts_with('-') {
            continue;
        }
        let Some(class) = exception else {
            *exception = Some(line.to_owned());
            continue;
        };
        let cause = if resource.is_empty() {
            Cause::InvalidConfig
        } else if class.contains("CommandTimeout") {
            Cause::TimedOut
        } else {
            Cause::from_message(line).unwrap_or(Cause::ResourceFailed)
        };
        let mut failure = Failure::new(cause, resource.clone(), line).code(class.clone());
        if let Some(code) = line
            .split_once("but received '")
            .and_then(|(_, r)| r.split_once('\''))
            .map(|(code, _)| code)
        {
            failure = failure.code(format!("rc={code}"));
        }
        if !failures.iter().any(|f| f.resource == failure.resource) {
            failures.push(failure);
        }
        pending = None;
    }
    failures
}

impl Runner for ChefRunner {
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

    fn more_debug_levels(
        &self,
    ) -> &'static [(&'static str, &'static [meridian_sdk::debug::DebugLevel])] {
        &[("chef-run", CHEF_RUN_DEBUG)]
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
        let cookbook = self.prepare(context, artifact)?;
        match self.transport_for(&context.host) {
            Transport::Local => Ok(Plan {
                changes: self.converge_here(context, &cookbook, true, log)?.0,
                unknown: false,
                saved: None,
            }),
            Transport::Remote => {
                log.out(&format!(
                    "chef-run cannot preview a run on {}, so what an apply would change is not known",
                    context.host
                ));
                Ok(Plan {
                    changes: Vec::new(),
                    unknown: true,
                    saved: None,
                })
            }
        }
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let cookbook = self.prepare(context, artifact)?;
        let changes = match self.transport_for(&context.host) {
            Transport::Local => self.converge_here(context, &cookbook, false, log)?.0,
            Transport::Remote => {
                if !context.secrets.is_empty() {
                    return Err(RunError::new(format!(
                        "chef-run has no private way to hand a secret to {}: it takes attributes on its command line only, where every user of this machine could read them. Run chef-client on the host, with the local transport, or keep the secret out of this document",
                        context.host
                    )));
                }
                // The core checked the host already; this runner is the
                // last thing between it and chef-run's command line.
                meridian_sdk::check_host(&context.host).map_err(|why| {
                    RunError::new(format!("chef-run cannot be given this host: {why}"))
                })?;
                // The credential's user is chef-run's --user; its key is in
                // an agent for the length of the run, which chef-run is told
                // of and nothing more.
                let agent = agent_for(&self.agent, context.credential.as_ref(), log)?;
                let mut tool = Tool::new(&self.run);
                if let Some(user) = context.credential.as_ref().and_then(|c| c.text("user")) {
                    tool = tool.args(["--user", user]);
                }
                if let Some(agent) = &agent {
                    tool = agent.tell(tool);
                }
                let debug = match meridian_sdk::debug::pick(CHEF_RUN_DEBUG, context.debug_level) {
                    Some(level) => Some(chef_run_debug(context, level)?),
                    None => None,
                };
                if let Some((config, _)) = &debug {
                    tool = tool
                        .arg("--config")
                        .arg(config.to_string_lossy().into_owned());
                }
                let ran = tool
                    .arg(&context.host)
                    .arg(format!("cookbooks/{cookbook}"))
                    .args(self.extra_args.iter().cloned())
                    .dir(&context.work_dir)
                    .run_ok("chef-run could not converge the cookbook", log);
                if let Some((_, logs)) = &debug {
                    relay_chef_run_log(logs, log);
                }
                ran?;
                drop(agent);
                log.out("chef-run does not report which resources it changed");
                Vec::new()
            }
        };
        Ok(Applied {
            changes,
            outputs: Vec::new(),
        })
    }

    fn inspects(&self) -> bool {
        true
    }

    /// Chef cannot read one resource alone. A why-run of the cookbook on
    /// this machine says whether it would change the resource, and what.
    fn inspect(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        _endpoint: &str,
    ) -> Result<meridian_sdk::Inspection, RunError> {
        if self.transport_for(&context.host) == Transport::Remote {
            return Err(RunError::new(format!(
                "chef-run has no why-run mode, so {resource} on {} cannot be read ({})",
                context.host,
                meridian_sdk::rules::UNSUPPORTED_INSPECT.name
            )));
        }
        let cookbook = self.prepare(context, artifact)?;
        let (changes, stdout) =
            self.converge_here(context, &cookbook, true, &meridian_sdk::NoLog)?;
        let would = would_do(&stdout, resource);
        let mut actual = meridian_sdk::Data::new();
        if !would.is_empty() {
            actual.insert(
                "wouldChange".to_owned(),
                meridian_sdk::Value::List(
                    would.into_iter().map(meridian_sdk::Value::String).collect(),
                ),
            );
        }
        Ok(meridian_sdk::Inspection {
            found: true,
            actual,
            in_desired_state: Some(!changes.iter().any(|c| c.resource == resource)),
            command: format!("chef-client --local-mode --why-run, resource {resource}"),
        })
    }
}

/// The config file a debug `chef-run` is given, and where its log goes: the
/// user's own `~/.chef-workstation/config.toml` with its `[log]` table
/// replaced by one at the level asked for, logging under the work
/// directory.
fn chef_run_debug(
    context: &RunContext,
    level: &meridian_sdk::debug::DebugLevel,
) -> Result<(PathBuf, PathBuf), RunError> {
    let word = level.does.rsplit(' ').next().unwrap_or("debug");
    let logs = context.work_dir.join("chef-run-logs");
    let _ = std::fs::remove_dir_all(&logs);
    let _ = std::fs::remove_file(&logs);
    std::fs::create_dir_all(&logs)
        .map_err(|e| RunError::new(format!("creating {}: {e}", logs.display())))?;
    let own = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".chef-workstation/config.toml"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let config = context.work_dir.join("chef-run-debug.toml");
    let text = format!(
        "{}\n[log]\nlevel = \"{word}\"\nlocation = {}\n",
        without_table(&own, "log"),
        meridian_sdk::Value::String(logs.to_string_lossy().into_owned()).canonical()
    );
    std::fs::write(&config, text)
        .map_err(|e| RunError::new(format!("writing {}: {e}", config.display())))?;
    Ok((config, logs))
}

/// A TOML text without one top-level table: from its `[name]` line to the
/// next table's.
fn without_table(text: &str, name: &str) -> String {
    let header = format!("[{name}]");
    let mut skipping = false;
    let mut kept = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skipping = trimmed == header || trimmed.starts_with(&format!("[{name}."));
        }
        if !skipping {
            kept.push(line);
        }
    }
    kept.join("\n")
}

/// Copies what `chef-run` logged into the step's output, as standard
/// error: the location may be read as a directory or as the file itself.
fn relay_chef_run_log(logs: &std::path::Path, log: &dyn Log) {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(logs) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect(),
        Err(_) if logs.is_file() => vec![logs.to_path_buf()],
        Err(_) => Vec::new(),
    };
    files.sort();
    for file in files {
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                log.err(line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What chef-client really prints in why-run mode for a file whose
    /// directory the same run would create: its assumptions come first, as
    /// bullets of their own, and the change after them.
    #[test]
    fn a_why_run_assumption_does_not_hide_the_change_that_follows_it() {
        // As Cinc Client 18 prints it: an assumption is a bullet of its own
        // under the resource, before or instead of the change.
        let report = "\
Recipe: meridian_site::default
  * group[team] action create
    - Would create group meridian-team
  * group[deploy:meridian-team] action modify
    * Cannot modify group[deploy:meridian-team] - group does not exist!
    * Group group[deploy:meridian-team] does not exist. Unless it would have been created earlier in this run, this attempt to modify it would fail.
    - Would modify group meridian-team
    - Would add missing member(s): meridian-deploy
  * service[scheduler] action enable
    * Service status not available. Assuming a prior action would have installed the service.
    * Assuming status of not running.
     (up to date)
  * apt_package[tool] action install (up to date)
";
        let shown: Vec<String> = read_report(report).iter().map(|c| c.to_string()).collect();
        assert_eq!(shown, ["update team: create", "update deploy: modify"]);
    }

    #[test]
    fn a_failed_resource_and_a_recipe_that_did_not_compile_are_read_from_the_formatter() {
        let output = "\
  * execute[migrate] action run
    ================================================================================
    Error executing action `run` on resource 'execute[migrate]'
    ================================================================================

    Mixlib::ShellOut::ShellCommandFailed
    ------------------------------------
    Expected process to exit with [0], but received '3'
    ---- Begin output of /bin/false ----
";
        let failures = read_failures(output);
        assert_eq!(failures.len(), 1);
        let f = &failures[0];
        assert_eq!(
            (f.resource.as_str(), f.cause),
            ("migrate", Cause::ResourceFailed)
        );
        assert_eq!(f.tool_code, "rc=3");
        assert_eq!(
            f.summary,
            "Expected process to exit with [0], but received '3'"
        );

        let output = "\
================================================================================
Recipe Compile Error in /work/cookbooks/meridian_web/recipes/default.rb
================================================================================

NoMethodError
-------------
undefined method `no_such_resource' for cookbook: meridian_web
";
        let failures = read_failures(output);
        assert_eq!(failures[0].cause, Cause::InvalidConfig);
        assert_eq!(failures[0].resource, "");
        assert_eq!(failures[0].tool_code, "NoMethodError");
    }

    #[test]
    fn changes_are_read_from_the_report() {
        let report = "\
Recipe: meridian_web::default
  * apt_package[nginx_pkg] action install
    - install version 1.24.0 of package nginx
  * file[nginx_conf] action create
    - create new file /etc/nginx/nginx.conf
    - update content in file /etc/nginx/nginx.conf from none to 4a1b2c
  * service[nginx_svc] action enable (up to date)
  * service[nginx_svc] action start
    - start service service[nginx_svc]
  * execute[warm_cache] action run (skipped due to only_if)
  * group[deploy_user:sudo] action modify
    - modify group sudo
  * file[stale_conf] action delete
    - delete file /etc/old-app.conf
";
        let shown: Vec<String> = read_report(report).iter().map(|c| c.to_string()).collect();
        assert_eq!(
            shown,
            [
                "update nginx_pkg: install",
                "update nginx_conf: create",
                "update nginx_svc: start",
                "update deploy_user: modify",
                "delete stale_conf: delete",
            ]
        );
    }

    #[test]
    fn the_log_table_is_taken_out_and_the_rest_kept() {
        let own = "[telemetry]\nenable = false\n[log]\nlevel = \"warn\"\n[log.extra]\nx = 1\n[connection]\ndefault_protocol = \"ssh\"";
        assert_eq!(
            without_table(own, "log"),
            "[telemetry]\nenable = false\n[connection]\ndefault_protocol = \"ssh\""
        );
    }
}
