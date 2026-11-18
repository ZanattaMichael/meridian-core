//! The half that converges a compiled cookbook. It sits outside the compile
//! chain: nothing here is reached by a compile.
//!
//! There are two ways to converge one. On the node itself, `chef-client` in
//! local mode does it, with no Chef server. From anywhere else, Chef
//! Workstation's `chef-run` copies the cookbook to the node and converges it
//! there over SSH or WinRM. The runner picks by the document's host and can
//! be told.

use std::path::PathBuf;

use meridian_sdk::tool::{absolute_path, write_artifact, Tool};
use meridian_sdk::{
    agent_for, credential_for_this_machine, AgentPrograms, Applied, Artifact, Change, ChangeAction,
    Log, Operations, Plan, RunContext, RunError, Runner, SecretFile,
};

/// Names the `chef-client` program, when it is not on `PATH`. Cinc's
/// `cinc-client` takes the same arguments.
pub const CLIENT_VARIABLE: &str = "MERIDIAN_CHEF_CLIENT";
/// Names the `chef-run` program, when it is not on `PATH`.
pub const RUN_VARIABLE: &str = "MERIDIAN_CHEF_RUN";
/// `local` or `remote`, to override the choice made from the host.
pub const TRANSPORT_VARIABLE: &str = "MERIDIAN_CHEF_TRANSPORT";
/// Extra arguments for every run, split on whitespace.
pub const ARGS_VARIABLE: &str = "MERIDIAN_CHEF_ARGS";

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

fn variable(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

impl ChefRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        ChefRunner {
            client: variable(CLIENT_VARIABLE, "chef-client"),
            run: variable(RUN_VARIABLE, "chef-run"),
            transport: match variable(TRANSPORT_VARIABLE, "")
                .to_ascii_lowercase()
                .as_str()
            {
                "local" => Some(Transport::Local),
                "remote" => Some(Transport::Remote),
                _ => None,
            },
            extra_args: std::env::var(ARGS_VARIABLE)
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            agent: AgentPrograms::from_env(),
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
    ) -> Result<Vec<Change>, RunError> {
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
            return Ok(changes);
        }
        let mut error = finished.into_error("chef-client could not converge the cookbook");
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

impl Runner for ChefRunner {
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
                changes: self.converge_here(context, &cookbook, true, log)?,
                unknown: false,
            }),
            Transport::Remote => {
                log.out(&format!(
                    "chef-run cannot preview a run on {}, so what an apply would change is not known",
                    context.host
                ));
                Ok(Plan {
                    changes: Vec::new(),
                    unknown: true,
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
            Transport::Local => self.converge_here(context, &cookbook, false, log)?,
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
                tool.arg(&context.host)
                    .arg(format!("cookbooks/{cookbook}"))
                    .args(self.extra_args.iter().cloned())
                    .dir(&context.work_dir)
                    .run_ok("chef-run could not converge the cookbook", log)?;
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
}
