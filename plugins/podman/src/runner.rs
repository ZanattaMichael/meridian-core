//! The half that puts the units on a host and has systemd run them. It sits
//! outside the compile chain: nothing here is reached by a compile.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use meridian_sdk::tool::{write_artifact, Finished, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog, Operations, Output, Plan,
    RunContext, RunError, Runner, Value,
};

use crate::script::{self, Desired, FileState, Generator, Probe, Settings, Steps};
use crate::META_PATH;

/// Names the `podman` program on the host, when it is not on its `PATH`.
pub const PODMAN_VARIABLE: &str = "MERIDIAN_PODMAN";
/// Names the `systemctl` program on the host.
pub const SYSTEMCTL_VARIABLE: &str = "MERIDIAN_PODMAN_SYSTEMCTL";
/// `user` (the default) runs the units as the connecting user's own, without
/// root. `system` installs them for the whole machine, using `sudo` when the
/// connecting user is not root.
pub const SCOPE_VARIABLE: &str = "MERIDIAN_PODMAN_SCOPE";
/// The directory unit files are installed in, when it is not the standard
/// one for the scope.
pub const UNIT_DIR_VARIABLE: &str = "MERIDIAN_PODMAN_UNIT_DIR";
/// Where Quadlet's generator is on the host, when it is not in one of the
/// places distributions put it.
pub const QUADLET_VARIABLE: &str = "MERIDIAN_PODMAN_QUADLET";
/// Names the `ssh` program used to reach another host.
pub const SSH_VARIABLE: &str = "MERIDIAN_PODMAN_SSH";
/// Extra arguments for every `ssh`, split on whitespace: which user, which
/// key, which port. For example `-l deploy -i ~/.ssh/deploy`.
pub const SSH_ARGS_VARIABLE: &str = "MERIDIAN_PODMAN_SSH_ARGS";

/// The file in the work directory that lists the unit files the last apply
/// left on the host, which is how a later one knows what to remove.
const INSTALLED: &str = "installed.json";

/// What a shell is asked to run: the script on its standard input, read whole
/// before any of it runs, so nothing the script starts can read the rest of
/// it by mistake.
const READ_AND_RUN: &str = "eval \"$(cat)\"";

/// Whose units they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// The connecting user's: rootless Podman, `systemctl --user`, units in
    /// `~/.config/containers/systemd`.
    #[default]
    User,
    /// The machine's: units in `/etc/containers/systemd`.
    System,
}

/// Runs compiled units.
///
/// A plan compares the unit files on the host with the compiled ones, asks
/// systemd which services are running, and has the host's own Quadlet read
/// the compiled units, so a unit the host's Podman cannot run is refused
/// before anything is touched. An apply writes and removes unit files,
/// reloads systemd, restarts what changed and starts the rest.
#[derive(Clone, Debug)]
pub struct PodmanRunner {
    pub podman: String,
    pub systemctl: String,
    pub journalctl: String,
    pub sudo: String,
    /// The shell that runs the scripts on the machine running Meridian.
    pub shell: String,
    pub ssh: String,
    /// Arguments added to every `ssh`, before the host.
    pub ssh_args: Vec<String>,
    pub scope: Scope,
    pub unit_dir: Option<String>,
    pub generator: Option<String>,
    /// A setting from the environment that could not be understood. Every
    /// run fails with it, instead of quietly running with a default.
    pub misconfigured: Option<String>,
}

impl Default for PodmanRunner {
    fn default() -> Self {
        PodmanRunner {
            podman: "podman".to_owned(),
            systemctl: "systemctl".to_owned(),
            journalctl: "journalctl".to_owned(),
            sudo: "sudo".to_owned(),
            shell: "sh".to_owned(),
            ssh: "ssh".to_owned(),
            ssh_args: Vec::new(),
            scope: Scope::User,
            unit_dir: None,
            generator: None,
            misconfigured: None,
        }
    }
}

fn setting(variable: &str) -> Option<String> {
    std::env::var(variable)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

impl PodmanRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let mut runner = PodmanRunner::default();
        if let Some(podman) = setting(PODMAN_VARIABLE) {
            runner.podman = podman;
        }
        if let Some(systemctl) = setting(SYSTEMCTL_VARIABLE) {
            runner.systemctl = systemctl;
        }
        if let Some(ssh) = setting(SSH_VARIABLE) {
            runner.ssh = ssh;
        }
        runner.ssh_args = setting(SSH_ARGS_VARIABLE)
            .map(|v| v.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        runner.unit_dir = setting(UNIT_DIR_VARIABLE);
        runner.generator = setting(QUADLET_VARIABLE);
        match setting(SCOPE_VARIABLE).as_deref() {
            None | Some("user") => runner.scope = Scope::User,
            Some("system") => runner.scope = Scope::System,
            Some(other) => {
                runner.misconfigured = Some(format!(
                    "{SCOPE_VARIABLE} is {other:?}; it must be \"user\" or \"system\""
                ))
            }
        }
        runner
    }

    fn settings(&self) -> Settings<'_> {
        Settings {
            scope: self.scope,
            unit_dir: self.unit_dir.as_deref(),
            generator: self.generator.as_deref(),
            podman: &self.podman,
            systemctl: &self.systemctl,
            journalctl: &self.journalctl,
            sudo: &self.sudo,
        }
    }

    /// Runs one script on the host a context names.
    fn on_host(
        &self,
        context: &RunContext,
        script: String,
        log: &dyn Log,
    ) -> Result<Finished, RunError> {
        let tool = if is_local(&context.host) {
            Tool::new(&self.shell).args(["-c", READ_AND_RUN])
        } else {
            let host = context.host.as_str();
            meridian_sdk::check_host(host)
                .map_err(|why| RunError::new(format!("ssh cannot be given this host: {why}")))?;
            Tool::new(&self.ssh)
                // A run has nobody to answer a password prompt.
                .args(["-o", "BatchMode=yes"])
                .args(self.ssh_args.iter().cloned())
                .arg(host)
                .arg(format!("sh -c {}", script::quote(READ_AND_RUN)))
        };
        let finished = tool.stdin(script).capture_stdout().run(log)?;
        // ssh keeps 255 for its own failures; the script never exits with it.
        if finished.code == Some(255) && !is_local(&context.host) {
            return Err(finished.into_error(format!("could not reach {} over ssh", context.host)));
        }
        Ok(finished)
    }

    /// Everything a plan and an apply both start with: the artifact on disk,
    /// the host looked at, the units checked by the host's own Quadlet.
    fn survey(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Survey, RunError> {
        if let Some(problem) = &self.misconfigured {
            return Err(RunError::new(problem.clone()));
        }
        let Some(meta) = artifact.files.get(META_PATH) else {
            return Err(RunError::new(format!(
                "the artifact has no {META_PATH}; it was not compiled by this plugin"
            )));
        };
        let meta = Meta::read(meta)?;
        let mut desired = Vec::with_capacity(meta.units.len());
        for unit in &meta.units {
            let Some(content) = artifact.files.get(&unit.file) else {
                return Err(RunError::new(format!(
                    "the artifact lists the unit {} and does not hold it",
                    unit.file
                )));
            };
            desired.push(Desired {
                file: unit.file.clone(),
                service: unit.service.clone(),
                content: content.clone(),
            });
        }
        write_artifact(&context.work_dir, artifact)?;

        let installed = read_installed(&context.work_dir)?;
        let stale: Vec<String> = installed
            .iter()
            .filter(|old| !meta.units.iter().any(|unit| unit.file == old.file))
            .map(|old| old.file.clone())
            .collect();

        let finished = self.on_host(context, self.settings().probe(&desired, &stale), log)?;
        if !finished.success() {
            return Err(finished.into_error(format!(
                "could not look at the units on {}",
                host_name(context)
            )));
        }
        let probe = script::read_probe(&finished.stdout).map_err(|problem| RunError {
            msg: format!(
                "the look at the units on {} could not be read: {problem}",
                host_name(context)
            ),
            output: finished.stdout.clone(),
            ..RunError::default()
        })?;
        check_generated(context, &meta, &probe, log)?;

        let changes = changes(&meta, &installed, &probe);
        Ok(Survey {
            meta,
            desired,
            installed,
            probe,
            changes,
        })
    }

    /// Reads a document's outputs, asking Podman for the parts only it knows.
    fn read_outputs(
        &self,
        context: &RunContext,
        meta: &Meta,
        log: &dyn Log,
    ) -> Result<Vec<Output>, RunError> {
        let mut wanted = Vec::new();
        for part in meta.outputs.iter().flat_map(|output| &output.parts) {
            if let Part::Inspect { noun, name, path } = part {
                wanted.push((noun.clone(), name.clone(), path.clone()));
            }
        }
        let mut values = BTreeMap::new();
        if !wanted.is_empty() {
            let finished = self.on_host(context, self.settings().inspect(&wanted), log)?;
            values = script::read_values(&finished.stdout);
            if !finished.success() || values.len() != wanted.len() {
                let (noun, name, path) = &wanted[values.len().min(wanted.len() - 1)];
                return Err(finished.into_error(format!(
                    "podman could not report {path} of the {noun} {name}, which an output reads"
                )));
            }
        }

        let mut next = 0;
        Ok(meta
            .outputs
            .iter()
            .map(|output| {
                let mut text = String::new();
                for part in &output.parts {
                    match part {
                        Part::Text(literal) => text.push_str(literal),
                        Part::Inspect { .. } => {
                            text.push_str(&values[&next]);
                            next += 1;
                        }
                    }
                }
                Output {
                    name: output.name.clone(),
                    value: Value::String(text),
                    sensitive: false,
                }
            })
            .collect())
    }
}

impl Runner for PodmanRunner {
    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        Ok(Plan {
            changes: self.survey(context, artifact, log)?.changes,
            unknown: false,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let survey = self.survey(context, artifact, log)?;
        let now_installed: Vec<Installed> = survey
            .meta
            .units
            .iter()
            .map(|unit| Installed {
                file: unit.file.clone(),
                id: unit.id.clone(),
                service: unit.service.clone(),
            })
            .collect();

        if !survey.changes.is_empty() {
            // Recorded before anything is written, so a unit this apply
            // installs is known to a later one even if this one is cut short.
            let mut both = survey.installed.clone();
            for unit in &now_installed {
                if !both.iter().any(|old| old.file == unit.file) {
                    both.push(unit.clone());
                }
            }
            write_installed(&context.work_dir, &both)?;

            let steps = survey.steps();
            let finished = self.on_host(context, self.settings().apply(&steps), log)?;
            if !finished.success() {
                let done = script::read_done(&finished.stdout);
                let what = match finished.code {
                    Some(71) => "could not create the directory the units are installed in",
                    Some(72) => "could not write or remove a unit file",
                    Some(73) => "systemctl daemon-reload failed",
                    Some(74) => "a changed unit did not restart",
                    Some(75) => "a unit did not start",
                    _ => "applying the units failed",
                };
                let mut error = finished.into_error(what);
                error.changes = survey
                    .changes
                    .iter()
                    .filter(|change| survey.was_done(change, &done))
                    .cloned()
                    .collect();
                return Err(error);
            }
        }
        write_installed(&context.work_dir, &now_installed)?;

        Ok(Applied {
            outputs: self.read_outputs(context, &survey.meta, log)?,
            changes: survey.changes,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let applied = context.work_dir.join(INSTALLED).exists();
        let meta = fs::read_to_string(context.work_dir.join(META_PATH)).ok();
        let (true, Some(meta)) = (applied, meta) else {
            return Err(RunError::new(
                "nothing has been applied for this document yet, so it has no outputs",
            ));
        };
        self.read_outputs(context, &Meta::read(&meta)?, &NoLog)
    }
}

/// Whether a host is the machine running Meridian.
fn is_local(host: &str) -> bool {
    matches!(host, "" | "localhost" | "127.0.0.1" | "::1")
}

fn host_name(context: &RunContext) -> &str {
    if is_local(&context.host) {
        "this machine"
    } else {
        &context.host
    }
}

/// The states systemd gives a service that is up or on its way there.
fn is_running(state: &str) -> bool {
    matches!(state, "active" | "activating" | "reloading")
}

/// What was learnt before anything was changed.
struct Survey {
    meta: Meta,
    desired: Vec<Desired>,
    installed: Vec<Installed>,
    probe: Probe,
    changes: Vec<Change>,
}

impl Survey {
    fn state(&self, file: &str) -> FileState {
        self.probe
            .files
            .get(file)
            .copied()
            .unwrap_or(FileState::New)
    }

    fn steps(&self) -> Steps {
        let service_of = |file: &String| {
            self.installed
                .iter()
                .find(|old| &old.file == file)
                .map(|old| old.service.clone())
                .unwrap_or_default()
        };
        Steps {
            remove: self
                .probe
                .stale
                .iter()
                .map(|file| (file.clone(), service_of(file)))
                .collect(),
            write: self
                .desired
                .iter()
                .filter(|unit| self.state(&unit.file) != FileState::Same)
                .cloned()
                .collect(),
            // A unit that is new has nothing running to restart.
            restart: self
                .desired
                .iter()
                .filter(|unit| self.state(&unit.file) == FileState::Changed)
                .map(|unit| unit.service.clone())
                .collect(),
            start: self
                .desired
                .iter()
                .map(|unit| unit.service.clone())
                .collect(),
        }
    }

    /// Whether a planned change had happened by the time an apply failed.
    fn was_done(&self, change: &Change, done: &script::Done) -> bool {
        match change.action {
            ChangeAction::Delete => self
                .installed
                .iter()
                .any(|old| old.id == change.resource && done.removed.contains(&old.file)),
            _ => self.meta.units.iter().any(|unit| {
                unit.id == change.resource
                    && match self.state(&unit.file) {
                        FileState::Same => done.started,
                        _ => done.wrote.contains(&unit.file),
                    }
            }),
        }
    }
}

/// The differences between the host and the document.
fn changes(meta: &Meta, installed: &[Installed], probe: &Probe) -> Vec<Change> {
    let mut changes = Vec::new();
    for unit in &meta.units {
        let (file, service) = (&unit.file, &unit.service);
        let state = probe.files.get(file).copied().unwrap_or(FileState::New);
        let (action, summary) = match state {
            FileState::New => (
                ChangeAction::Create,
                format!("install {file} and start {service}"),
            ),
            FileState::Changed => (
                ChangeAction::Update,
                format!("{file} changed; restart {service}"),
            ),
            FileState::Same => {
                let running = probe.active.get(service).map_or("unknown", String::as_str);
                if is_running(running) {
                    continue;
                }
                (
                    ChangeAction::Update,
                    format!("{service} is {running}; start it"),
                )
            }
        };
        changes.push(Change {
            resource: unit.id.clone(),
            action,
            summary,
        });
    }
    for old in installed {
        if probe.stale.contains(&old.file) {
            changes.push(Change {
                resource: old.id.clone(),
                action: ChangeAction::Delete,
                summary: format!("stop {} and remove {}", old.service, old.file),
            });
        }
    }
    changes
}

/// Refuses units the host's own Quadlet would not turn into services, which
/// would otherwise fail later and far less clearly: systemd's generator
/// reports nothing to whoever asked for the reload.
fn check_generated(
    context: &RunContext,
    meta: &Meta,
    probe: &Probe,
    log: &dyn Log,
) -> Result<(), RunError> {
    let host = host_name(context);
    let (code, generated, said) = match &probe.generator {
        Generator::Missing => {
            return Err(RunError::new(format!(
                "Podman's Quadlet was not found on {host}. Quadlet comes with Podman 4.4 and \
                 later; if it is installed somewhere unusual, name it with {QUADLET_VARIABLE}"
            )))
        }
        Generator::Ran {
            code,
            generated,
            said,
        } => (*code, generated, said),
    };
    let missing = |unit: &&MetaUnit| !generated.contains(&unit.service);
    // A kind of unit the host's Podman is too old for is the likeliest reason
    // for anything else Quadlet says, so it is reported first.
    let too_new = meta.units.iter().filter(missing).find_map(|unit| {
        let since = match unit.kind.as_str() {
            "pod" => "Quadlet learnt .pod units in Podman 5.0.",
            "build" => "Quadlet learnt .build units in Podman 5.2.",
            "image" => "Quadlet learnt .image units in Podman 4.8.",
            _ => return None,
        };
        Some((unit, since))
    });
    let version = if probe.podman.is_empty() {
        "its Podman".to_owned()
    } else {
        format!("its Podman ({})", probe.podman)
    };
    if let Some((unit, since)) = too_new {
        return Err(RunError {
            msg: format!(
                "{} would not become {} on {host}: {version} does not turn it into a \
                 service. {since}",
                unit.file, unit.service
            ),
            output: said.join("\n"),
            ..RunError::default()
        });
    }
    if code != 0 {
        return Err(RunError {
            msg: format!("the Quadlet on {host} refused the units"),
            exit_code: Some(code),
            output: said.join("\n"),
            ..RunError::default()
        });
    }
    if let Some(unit) = meta.units.iter().find(missing) {
        return Err(RunError::new(format!(
            "{} would not become {} on {host}: {version} does not turn it into a service",
            unit.file, unit.service
        )));
    }
    // What is left is advice, such as an image name that is not fully
    // qualified. The operator should see it; it stops nothing.
    for line in said {
        log.err(line);
    }
    Ok(())
}

/// A unit file an apply left on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Installed {
    file: String,
    id: String,
    service: String,
}

/// A name that is safe to use as a file name in the unit directory. The
/// record of what was installed is a file anyone can edit, and what it names
/// gets removed.
fn is_unit_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
}

fn read_installed(work_dir: &Path) -> Result<Vec<Installed>, RunError> {
    let path = work_dir.join(INSTALLED);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let unreadable = |problem: &str| {
        RunError::new(format!(
            "{} could not be read: {problem}. It records which units Meridian installed; \
             remove it only if none of them should be cleaned up",
            path.display()
        ))
    };
    let value = decode_value(text.as_bytes()).map_err(|e| unreadable(&e))?;
    let Some(entries) = value.as_list() else {
        return Err(unreadable("it is not a list"));
    };
    let mut installed = Vec::with_capacity(entries.len());
    for entry in entries {
        let field = |key: &str| {
            entry
                .as_map()
                .and_then(|map| map.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let old = Installed {
            file: field("file"),
            id: field("id"),
            service: field("service"),
        };
        if !is_unit_file_name(&old.file) || !is_unit_file_name(&old.service) {
            return Err(unreadable("it names something that is not a unit"));
        }
        installed.push(old);
    }
    Ok(installed)
}

fn write_installed(work_dir: &Path, installed: &[Installed]) -> Result<(), RunError> {
    let text = Value::List(
        installed
            .iter()
            .map(|unit| {
                Value::Map(BTreeMap::from([
                    ("file".to_owned(), Value::String(unit.file.clone())),
                    ("id".to_owned(), Value::String(unit.id.clone())),
                    ("service".to_owned(), Value::String(unit.service.clone())),
                ]))
            })
            .collect(),
    )
    .pretty();
    let path = work_dir.join(INSTALLED);
    fs::write(&path, text)
        .map_err(|e| RunError::new(format!("could not write {}: {e}", path.display())))
}

/// One unit, as the compile recorded it for the runner.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetaUnit {
    file: String,
    id: String,
    kind: String,
    service: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Text(String),
    Inspect {
        noun: String,
        name: String,
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MetaOutput {
    name: String,
    parts: Vec<Part>,
}

/// What the compile told the runner about the artifact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Meta {
    units: Vec<MetaUnit>,
    outputs: Vec<MetaOutput>,
}

impl Meta {
    fn read(json: &str) -> Result<Meta, RunError> {
        let unreadable =
            |problem: &str| RunError::new(format!("{META_PATH} could not be read: {problem}"));
        let value = decode_value(json.as_bytes()).map_err(|e| unreadable(&e))?;
        let Some(root) = value.as_map() else {
            return Err(unreadable("it is not a JSON object"));
        };
        let list = |key: &str| root.get(key).and_then(Value::as_list).unwrap_or(&[]);
        let text = |entry: &Value, key: &str| {
            entry
                .as_map()
                .and_then(|map| map.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };

        let mut meta = Meta::default();
        for entry in list("units") {
            let unit = MetaUnit {
                file: text(entry, "file"),
                id: text(entry, "id"),
                kind: text(entry, "kind"),
                service: text(entry, "service"),
            };
            // These names are written into shell scripts and used as file
            // names on the host, so an artifact that was tampered with is
            // refused here.
            if !is_unit_file_name(&unit.file) || !is_unit_file_name(&unit.service) {
                return Err(unreadable("it names a unit that is not a unit file"));
            }
            meta.units.push(unit);
        }
        for entry in list("outputs") {
            let mut parts = Vec::new();
            let values = entry
                .as_map()
                .and_then(|map| map.get("value"))
                .and_then(Value::as_list)
                .unwrap_or(&[]);
            for part in values {
                parts.push(match part {
                    Value::String(literal) => Part::Text(literal.clone()),
                    other => Part::Inspect {
                        noun: text(other, "inspect"),
                        name: text(other, "name"),
                        path: text(other, "path"),
                    },
                });
            }
            meta.outputs.push(MetaOutput {
                name: text(entry, "name"),
                parts,
            });
        }
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        let unit = |id: &str, kind: &str, service: &str| MetaUnit {
            file: format!("{id}.{kind}"),
            id: id.to_owned(),
            kind: kind.to_owned(),
            service: service.to_owned(),
        };
        Meta {
            units: vec![
                unit("web", "container", "web.service"),
                unit("data", "volume", "data-volume.service"),
                unit("app", "network", "app-network.service"),
            ],
            outputs: Vec::new(),
        }
    }

    fn probe(files: &[(&str, FileState)], active: &[(&str, &str)], stale: &[&str]) -> Probe {
        Probe {
            files: files.iter().map(|(f, s)| ((*f).to_owned(), *s)).collect(),
            stale: stale.iter().map(|f| (*f).to_owned()).collect(),
            active: active
                .iter()
                .map(|(s, state)| ((*s).to_owned(), (*state).to_owned()))
                .collect(),
            podman: "podman version 5.4.0".to_owned(),
            generator: Generator::Ran {
                code: 0,
                generated: ["web.service", "data-volume.service", "app-network.service"]
                    .map(str::to_owned)
                    .into(),
                said: Vec::new(),
            },
        }
    }

    #[test]
    fn a_plan_is_the_difference_between_the_host_and_the_document() {
        let installed = [Installed {
            file: "old.container".into(),
            id: "old".into(),
            service: "old.service".into(),
        }];
        let shown: Vec<String> = changes(
            &meta(),
            &installed,
            &probe(
                &[
                    ("web.container", FileState::Changed),
                    ("data.volume", FileState::Same),
                    ("app.network", FileState::New),
                ],
                &[("data-volume.service", "failed")],
                &["old.container"],
            ),
        )
        .iter()
        .map(Change::to_string)
        .collect();
        assert_eq!(
            shown,
            [
                "update web: web.container changed; restart web.service",
                "update data: data-volume.service is failed; start it",
                "create app: install app.network and start app-network.service",
                "delete old: stop old.service and remove old.container",
            ]
        );
    }

    #[test]
    fn a_host_that_matches_and_runs_everything_is_in_sync() {
        let same = [
            ("web.container", FileState::Same),
            ("data.volume", FileState::Same),
            ("app.network", FileState::Same),
        ];
        let running = [
            ("web.service", "active"),
            ("data-volume.service", "active"),
            ("app-network.service", "activating"),
        ];
        assert!(changes(&meta(), &[], &probe(&same, &running, &[])).is_empty());
    }

    #[test]
    fn a_unit_the_hosts_podman_cannot_run_is_refused_by_name() {
        let mut meta = meta();
        meta.units.push(MetaUnit {
            file: "shop.pod".into(),
            id: "shop".into(),
            kind: "pod".into(),
            service: "shop-pod.service".into(),
        });
        let mut probe = probe(&[], &[], &[]);
        probe.podman = "podman version 4.9.3".into();
        let err = check_generated(&RunContext::default(), &meta, &probe, &NoLog).unwrap_err();
        assert_eq!(
            err.msg,
            "shop.pod would not become shop-pod.service on this machine: its Podman (podman \
             version 4.9.3) does not turn it into a service. Quadlet learnt .pod units in \
             Podman 5.0."
        );
    }

    #[test]
    fn the_record_of_what_was_installed_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("meridian-podman-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(read_installed(&dir).unwrap().is_empty());
        let installed = vec![Installed {
            file: "web.container".into(),
            id: "web".into(),
            service: "web.service".into(),
        }];
        write_installed(&dir, &installed).unwrap();
        assert_eq!(read_installed(&dir).unwrap(), installed);

        // A name that would reach outside the unit directory is not removed.
        fs::write(
            dir.join(INSTALLED),
            r#"[{"file":"../../etc/passwd","id":"x","service":"x.service"}]"#,
        )
        .unwrap();
        let err = read_installed(&dir).unwrap_err();
        assert!(err.msg.contains("not a unit"), "{}", err.msg);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_this_machine_is_local() {
        for host in ["", "localhost", "127.0.0.1", "::1"] {
            assert!(is_local(host), "{host}");
        }
        assert!(!is_local("web01.example.com"));
    }
}
