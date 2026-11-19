//! The half that involves `cloud-init` itself. It sits outside the compile
//! chain: nothing here is reached by a compile.
//!
//! cloud-init cannot be pointed at a machine. A machine is created with
//! user-data and applies it by itself on its first boot, so what Meridian can
//! do around that is limited, and this runner does exactly three things:
//!
//! - It has `cloud-init schema` check the user-data, on the machine running
//!   Meridian, before any machine is created with it.
//! - It keeps the checked files in a seed directory, where whatever creates
//!   the machine reads them from.
//! - If asked to, it waits over SSH until the machine's first boot is over,
//!   and fails if cloud-init says the boot failed. That is what lets a later
//!   stage rely on the machine being ready.

use std::fs;
use std::path::{Path, PathBuf};

use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{settings, Setting};
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Log, Operations, Output, Plan, RunContext, RunError,
    Runner, Value,
};

use crate::{META_DATA_PATH, USER_DATA_PATH};

/// What `--tool-debug` adds to the tool, level 1 first. Given to the local `cloud-init schema` check; the machine's own boot is unchanged.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] =
    &[meridian_sdk::debug::DebugLevel::args(&["--debug"])];

/// Names the `cloud-init` program to run, when it is not on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_CLOUD_INIT";
/// Set to `off` to skip the schema check, on a machine that has no
/// cloud-init to run it with.
pub const VALIDATE_VARIABLE: &str = "MERIDIAN_CLOUD_INIT_VALIDATE";
/// Set to `ssh` to have an apply wait for the first boot to finish.
pub const WAIT_VARIABLE: &str = "MERIDIAN_CLOUD_INIT_WAIT";
/// Names the `ssh` program to wait with, when it is not on `PATH`.
pub const SSH_VARIABLE: &str = "MERIDIAN_CLOUD_INIT_SSH";
/// Extra arguments for `ssh`, split like a shell command line: which user,
/// which key. For example `-l ubuntu -i ~/.ssh/deploy`.
pub const SSH_ARGS_VARIABLE: &str = "MERIDIAN_CLOUD_INIT_SSH_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "cloud-init",
            "The `cloud-init` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "validate",
            VALIDATE_VARIABLE,
            "on",
            "`on` or `off`: whether a plan and an apply check the user-data against cloud-init's schema. Set it to `off` on a machine that has no cloud-init to run the check with.",
        ),
        Setting::new(
            "wait",
            WAIT_VARIABLE,
            "off",
            "`ssh` or `off`: whether an apply waits, over SSH, for the host's first boot to finish.",
        ),
        Setting::new(
            "ssh",
            SSH_VARIABLE,
            "ssh",
            "The `ssh` program to wait with, when it is not on `PATH`.",
        ),
        Setting::new(
            "ssh-args",
            SSH_ARGS_VARIABLE,
            "",
            "Extra arguments for `ssh`, split like a shell command line: which user, which key. For example `-l ubuntu -i ~/.ssh/deploy`.",
        ),
    ]
}

/// The directory, inside a stage's work directory, that holds the files the
/// last apply accepted.
const SEED_DIR: &str = "seed";
/// The directory a candidate is written to for checking. It is kept apart
/// from the seed so a plan, which must change nothing, never touches it.
const CHECK_DIR: &str = "check";

/// How an apply waits for a machine's first boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The `ssh` program.
    pub program: String,
    /// Arguments placed before the host.
    pub args: Vec<String>,
}

/// Checks compiled user-data, keeps it as a seed, and optionally waits for
/// the machine to finish applying it.
pub struct CloudInitRunner {
    /// The `cloud-init` program.
    pub program: String,
    /// Whether `cloud-init schema` checks the user-data first.
    pub validate: bool,
    /// Whether, and how, an apply waits for the first boot.
    pub wait: Option<Wait>,
    /// A setting the environment gave that means nothing. It is reported
    /// when the runner is used, not swallowed when it is built.
    pub misconfigured: Option<String>,
}

impl CloudInitRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        let mut misconfigured = None;
        let validate = match settings::text(setting("validate")).as_str() {
            "on" => true,
            "off" => false,
            other => {
                misconfigured = Some(format!(
                    "{VALIDATE_VARIABLE} is {other:?}; it can be \"on\" or \"off\""
                ));
                true
            }
        };
        let wait = match settings::text(setting("wait")).as_str() {
            "off" => None,
            "ssh" => Some(Wait {
                program: settings::text(setting("ssh")),
                args: settings::args(setting("ssh-args")),
            }),
            other => {
                misconfigured = Some(format!(
                    "{WAIT_VARIABLE} is {other:?}; it can be \"ssh\" or \"off\""
                ));
                None
            }
        };
        CloudInitRunner {
            program: settings::text(setting("program")),
            validate,
            wait,
            misconfigured,
        }
    }

    /// Has cloud-init check a candidate, and says how it differs from the
    /// seed. Nothing but the check directory is written.
    fn check(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Option<ChangeAction>, RunError> {
        if let Some(why) = &self.misconfigured {
            return Err(RunError::new(why.clone()));
        }
        for needed in [USER_DATA_PATH, META_DATA_PATH] {
            if !artifact.files.contains_key(needed) {
                return Err(RunError::new(format!(
                    "the artifact has no {needed}; it was not compiled by this plugin"
                )));
            }
        }

        if self.validate {
            let dir = context.work_dir.join(CHECK_DIR);
            write_artifact(&dir, artifact)?;
            let finished = Tool::new(&self.program)
                .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
                .args(["schema", "--config-file", USER_DATA_PATH])
                .dir(&dir)
                .run(log)
                .map_err(|mut error| {
                    if error.msg.contains("it is not installed") {
                        error.msg.push_str(&format!(
                            ". cloud-init checks the user-data before a machine is created \
                             with it: install it, name it with {PROGRAM_VARIABLE}, or set \
                             {VALIDATE_VARIABLE}=off to go without the check"
                        ));
                    }
                    error
                })?;
            if !finished.success() {
                let said = format!("{}\n{}", finished.stdout, finished.stderr);
                let summary = said
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or_default()
                    .to_owned();
                let mut error = finished.into_error("cloud-init rejected the user-data");
                error
                    .failures
                    .push(Failure::new(Cause::InvalidConfig, "", summary));
                return Err(error);
            }
        }

        let seed = seed_dir(context);
        let current = |name: &str| fs::read_to_string(seed.join(name)).ok();
        Ok(match current(USER_DATA_PATH) {
            None => Some(ChangeAction::Create),
            Some(user_data)
                if user_data == artifact.files[USER_DATA_PATH]
                    && current(META_DATA_PATH).as_ref() == artifact.files.get(META_DATA_PATH) =>
            {
                None
            }
            Some(_) => Some(ChangeAction::Update),
        })
    }

    /// Blocks until cloud-init on the host says the first boot is over.
    fn wait_for_boot(&self, wait: &Wait, host: &str, log: &dyn Log) -> Result<(), RunError> {
        if host.is_empty() {
            return Err(RunError::new(format!(
                "{WAIT_VARIABLE}=ssh needs a host to wait for, and this document resolved none"
            )));
        }
        // The core checked the host already; this runner is the last thing
        // between it and ssh's command line, where a leading "-" is an
        // option.
        meridian_sdk::check_host(host)
            .map_err(|why| RunError::new(format!("ssh cannot be given this host: {why}")))?;
        let finished = Tool::new(&wait.program)
            // Waiting must never stop to ask for a password nobody is there
            // to type.
            .args(["-o", "BatchMode=yes"])
            .args(wait.args.iter().cloned())
            .arg(host)
            .args(["cloud-init", "status", "--wait", "--long"])
            .run(log)?;
        match finished.code {
            Some(0) => Ok(()),
            // Since cloud-init 23.4: the boot finished, and something along
            // the way went wrong without stopping it. The details are in the
            // output just relayed.
            Some(2) => {
                log.err(&format!(
                    "cloud-init on {host} finished the first boot with recoverable errors"
                ));
                Ok(())
            }
            Some(1) => {
                let failures = read_boot_errors(&finished.stdout, host);
                let mut error = finished.into_error(format!(
                    "cloud-init on {host} reports that the first boot failed"
                ));
                error.failures = failures;
                Err(error)
            }
            Some(255) => {
                let said = finished.stderr.trim().to_owned();
                let mut error = finished.into_error(format!("could not reach {host} over ssh"));
                error.failures.push(
                    Failure::new(
                        Cause::from_message(&said).unwrap_or(Cause::Unreachable),
                        "",
                        said.lines().last().unwrap_or_default(),
                    )
                    .endpoint(host)
                    .code("rc=255"),
                );
                Err(error)
            }
            _ => Err(finished.into_error(format!("waiting for cloud-init on {host} failed"))),
        }
    }
}

/// The modules `cloud-init status --long` lists under `errors:`, one
/// failure each on the host:
///
/// ```text
/// errors:
///     - ('scripts_user', RuntimeError('Runparts: 1 failures (part-001) in 1 attempted commands'))
/// recoverable_errors:
/// ```
fn read_boot_errors(status: &str, host: &str) -> Vec<Failure> {
    let mut failures: Vec<Failure> = Vec::new();
    let mut in_errors = false;
    for line in status.lines() {
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace) && trimmed.ends_with(':') {
            in_errors = trimmed == "errors:";
            continue;
        }
        if !in_errors {
            continue;
        }
        let Some(entry) = trimmed.strip_prefix("- (") else {
            continue;
        };
        // `'module', Exception('why')`, or a plain message.
        let (module, why) = match entry.strip_prefix('\'').and_then(|e| e.split_once("', ")) {
            Some((module, why)) => (module, why.strip_suffix(')').unwrap_or(why)),
            None => ("", entry.strip_suffix(')').unwrap_or(entry)),
        };
        let (code, message) = match why.split_once("('") {
            Some((class, message)) => (class, message.strip_suffix("')").unwrap_or(message)),
            None => ("", why),
        };
        failures.push(
            Failure::new(
                Cause::from_message(message).unwrap_or(Cause::ResourceFailed),
                module,
                message,
            )
            .endpoint(host)
            .code(code),
        );
    }
    failures
}

fn seed_dir(context: &RunContext) -> PathBuf {
    context.work_dir.join(SEED_DIR)
}

fn seed_change(action: ChangeAction, seed: &Path, done: bool) -> Change {
    let (verb, tense) = match (action, done) {
        (ChangeAction::Create, false) => ("seed", "would be written to"),
        (ChangeAction::Create, true) => ("seed", "written to"),
        (_, false) => ("seed", "would be replaced in"),
        (_, true) => ("seed", "replaced in"),
    };
    Change {
        resource: USER_DATA_PATH.to_owned(),
        action,
        summary: format!(
            "{verb} {tense} {}; a machine applies it on its first boot",
            seed.display()
        ),
    }
}

fn seed_outputs(seed: &Path) -> Vec<Output> {
    let text = |path: PathBuf| Value::String(path.to_string_lossy().into_owned());
    vec![
        Output {
            name: "seed_dir".to_owned(),
            value: text(seed.to_path_buf()),
            sensitive: false,
        },
        Output {
            name: "user_data_file".to_owned(),
            value: text(seed.join(USER_DATA_PATH)),
            sensitive: false,
        },
    ]
}

impl Runner for CloudInitRunner {
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    fn operations(&self) -> Operations {
        Operations::ALL
    }

    /// Checks the user-data and says whether the seed would change.
    ///
    /// cloud-init has no dry run, and the machine may not exist yet, so the
    /// plan is about the seed and never about the machine.
    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let change = self.check(context, artifact, log)?;
        Ok(Plan {
            changes: change
                .map(|action| seed_change(action, &seed_dir(context), false))
                .into_iter()
                .collect(),
            unknown: false,
            saved: None,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let change = self.check(context, artifact, log)?;
        let seed = seed_dir(context);
        let mut changes = Vec::new();
        if let Some(action) = change {
            write_artifact(&seed, artifact)?;
            changes.push(seed_change(action, &seed, true));
        }
        if let Some(wait) = &self.wait {
            self.wait_for_boot(wait, &context.host, log)
                .map_err(|mut error| {
                    error.changes = changes.clone();
                    error
                })?;
        }
        Ok(Applied {
            changes,
            outputs: seed_outputs(&seed),
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let seed = seed_dir(context);
        if !seed.join(USER_DATA_PATH).is_file() {
            return Err(RunError::new(
                "no seed has been written yet; apply the document first",
            ));
        }
        Ok(seed_outputs(&seed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_modules_the_long_status_lists_as_errors_are_failures_on_the_host() {
        let status = "status: error
extended_status: error - done
detail:
DataSourceNoCloud [seed=/dev/sr0]
errors:
\t- ('scripts_user', RuntimeError('Runparts: 1 failures (part-001) in 1 attempted commands'))
\t- ('package_update_upgrade_install', ProcessExecutionError('Unexpected error while running command.'))
recoverable_errors:
WARNING:
\t- Running module ssh_import_id failed
";
        let failures = read_boot_errors(status, "web01");
        let got: Vec<(&str, &str, &str)> = failures
            .iter()
            .map(|f| {
                (
                    f.resource.as_str(),
                    f.tool_code.as_str(),
                    f.endpoint.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("scripts_user", "RuntimeError", "web01"),
                (
                    "package_update_upgrade_install",
                    "ProcessExecutionError",
                    "web01"
                ),
            ]
        );
        assert_eq!(
            failures[0].summary,
            "Runparts: 1 failures (part-001) in 1 attempted commands"
        );
    }
}
