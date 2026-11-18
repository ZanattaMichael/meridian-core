//! The half that brings a Compose project up with `docker compose`. It sits
//! outside the compile chain: nothing here is reached by a compile.
//!
//! A plan is worked out from what Compose and Docker report as data, not
//! from what `up --dry-run` prints. Compose stamps every container it creates
//! with a hash of its service's configuration, and recreates the container
//! when the hash of the file's service differs. A plan asks Compose for the
//! hashes of the compiled file and Docker for the hashes of the running
//! containers, and compares them the same way.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog, Operations, Output, Plan,
    RunContext, RunError, Runner, SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::{secret_variable, COMPOSE_PATH, PROJECT_PATH};

/// The secrets of a run, written for Compose: one private `.env` file,
/// given to every `docker compose` call of the stage with `--env-file` and
/// removed when the stage is over. Compose reads it for its interpolation
/// and for a secret element's `environment`, and the values go nowhere but
/// into what Compose makes of the file.
fn secrets_file(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
    if context.secrets.is_empty() {
        return Ok(None);
    }
    context
        .secrets
        .dotenv_file("secrets.env", secret_variable)
        .map(Some)
        .map_err(|e| RunError::new(format!("the secrets could not be written for Compose: {e}")))
}

/// Names the program to run, when it is not `docker` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_DOCKER";
/// Extra arguments for `docker compose up`, split like a shell command line:
/// `--pull always`, `--force-recreate`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_DOCKER_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "docker",
            "The `docker` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for `docker compose up`, split like a shell command line: `--pull always`, `--force-recreate`.",
        ),
    ]
}

/// Brings a compiled Compose project up on a Docker daemon.
///
/// A plan compares the compiled file with what Docker is running and touches
/// nothing. An apply is `docker compose up --detach --remove-orphans`, so a
/// service the document no longer declares is removed, and by default it
/// waits for every service to be running or healthy.
pub struct DockerRunner {
    pub program: String,
    /// Arguments added to `docker compose up`, after Meridian's own.
    pub extra_args: Vec<String>,
}

/// One piece of an output, as `project.json` records it.
enum Part {
    Text(String),
    Port { service: String, port: String },
    Inspect { service: String, path: String },
}

/// What `project.json` says.
struct Project {
    name: String,
    host: String,
    context: String,
    directory: String,
    profiles: Vec<String>,
    wait: bool,
    wait_timeout: u64,
    outputs: Vec<(String, Vec<Part>)>,
}

/// The label Compose puts on everything that belongs to a project.
const PROJECT_LABEL: &str = "com.docker.compose.project";

/// One line per container: its service, the hash of the configuration it was
/// created from, its state, its name, and whether `compose run` made it.
const CONTAINER_FORMAT: &str = "{{.Label \"com.docker.compose.service\"}}|{{.Label \"com.docker.compose.config-hash\"}}|{{.State}}|{{.Names}}|{{.Label \"com.docker.compose.oneoff\"}}";

impl Project {
    fn read(text: &str) -> Result<Project, RunError> {
        let bad = || RunError::new(format!("{PROJECT_PATH} is not one this plugin compiled"));
        let value = decode_value(text.as_bytes()).map_err(|_| bad())?;
        let map = value.as_map().ok_or_else(bad)?;
        let string = |map: &std::collections::BTreeMap<String, Value>, key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let mut outputs = Vec::new();
        for output in map.get("outputs").and_then(Value::as_list).unwrap_or(&[]) {
            let output = output.as_map().ok_or_else(bad)?;
            let mut parts = Vec::new();
            for part in output.get("parts").and_then(Value::as_list).unwrap_or(&[]) {
                let part = part.as_map().ok_or_else(bad)?;
                let service = string(part, "service");
                parts.push(
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        Part::Text(text.to_owned())
                    } else if part.contains_key("port") {
                        Part::Port {
                            service,
                            port: string(part, "port"),
                        }
                    } else {
                        Part::Inspect {
                            service,
                            path: string(part, "inspect"),
                        }
                    },
                );
            }
            outputs.push((string(output, "name"), parts));
        }
        let project = Project {
            name: string(map, "name"),
            host: string(map, "host"),
            context: string(map, "context"),
            directory: string(map, "directory"),
            profiles: map
                .get("profiles")
                .and_then(Value::as_list)
                .unwrap_or(&[])
                .iter()
                .filter_map(|s| Some(s.as_str()?.to_owned()))
                .collect(),
            wait: map.get("wait") == Some(&Value::Bool(true)),
            wait_timeout: map.get("wait_timeout").and_then(whole).unwrap_or(0),
            outputs,
        };
        if project.name.is_empty() {
            return Err(bad());
        }
        Ok(project)
    }
}

impl DockerRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        DockerRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
        }
    }

    /// `docker`, aimed at the project's daemon.
    fn docker(&self, context: &RunContext, project: &Project) -> Tool {
        let mut tool = Tool::new(&self.program).dir(&context.work_dir);
        if !project.host.is_empty() {
            tool = tool.args(["--host", &project.host]);
        }
        if !project.context.is_empty() {
            tool = tool.args(["--context", &project.context]);
        }
        tool
    }

    /// `docker compose`, aimed at the project, reading the run's secrets
    /// when it has any.
    fn compose(
        &self,
        context: &RunContext,
        project: &Project,
        secrets: Option<&SecretFile>,
    ) -> Tool {
        let mut tool = self
            .docker(context, project)
            .args(["compose", "--project-name", &project.name])
            .args(["--file", COMPOSE_PATH]);
        if let Some(secrets) = secrets {
            tool = tool.args(["--env-file", &secrets.path().to_string_lossy()]);
        }
        if !project.directory.is_empty() {
            tool = tool.args(["--project-directory", &project.directory]);
        }
        for profile in &project.profiles {
            tool = tool.args(["--profile", profile]);
        }
        tool
    }

    /// One listing from Docker of what belongs to the project.
    fn listing(
        &self,
        context: &RunContext,
        project: &Project,
        what: &[&str],
        format: &str,
    ) -> Result<String, RunError> {
        Ok(self
            .docker(context, project)
            .args(what.iter().copied())
            .args([
                "--filter",
                &format!("label={PROJECT_LABEL}={}", project.name),
            ])
            .args(["--format", format])
            .capture_stdout()
            .run_ok("docker could not list what the project has running", &NoLog)?
            .stdout)
    }

    /// Compares the compiled file with what Docker has.
    fn survey(
        &self,
        context: &RunContext,
        project: &Project,
        secrets: Option<&SecretFile>,
        log: &dyn Log,
    ) -> Result<(Desired, Plan), RunError> {
        log.out(&format!(
            "project {}: comparing the file with what Docker is running",
            project.name
        ));
        // Compose's own reading of the file, which is also its validation.
        let config = self
            .compose(context, project, secrets)
            .args(["config", "--format", "json"])
            .capture_stdout()
            .run_ok("docker compose rejected the compiled file", log)?;
        let hashes = self
            .compose(context, project, secrets)
            .args(["config", "--hash", "*"])
            .capture_stdout()
            .run_ok("docker compose could not hash the services", log)?;
        let desired = read_desired(&config.stdout, &hashes.stdout).map_err(|e| {
            RunError::new(format!(
                "what docker compose said about the file could not be read: {e}"
            ))
        })?;

        let lines = |text: String| text.lines().map(|l| l.trim().to_owned()).collect();
        let actual = Actual {
            containers: read_containers(&self.listing(
                context,
                project,
                &["ps", "--all"],
                CONTAINER_FORMAT,
            )?),
            networks: lines(self.listing(context, project, &["network", "ls"], "{{.Name}}")?),
            volumes: lines(self.listing(context, project, &["volume", "ls"], "{{.Name}}")?),
        };
        let plan = diff(&desired, &actual);
        Ok((desired, plan))
    }

    fn prepare(&self, context: &RunContext, artifact: &Artifact) -> Result<Project, RunError> {
        let (Some(_), Some(settings)) = (
            artifact.files.get(COMPOSE_PATH),
            artifact.files.get(PROJECT_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {COMPOSE_PATH} and {PROJECT_PATH}; it was not compiled by this plugin"
            )));
        };
        let project = Project::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;
        Ok(project)
    }

    /// One part of an output, read from the running project.
    fn read_part(
        &self,
        context: &RunContext,
        project: &Project,
        output: &str,
        part: &Part,
    ) -> Result<Value, RunError> {
        let what = format!("docker could not read output {output:?}");
        match part {
            Part::Text(text) => Ok(Value::String(text.clone())),
            Part::Port { service, port } => {
                let (number, protocol) = port.split_once('/').unwrap_or((port, "tcp"));
                // Reading needs no secret: Compose finds the container by
                // the project's labels, and warns about an unset variable
                // without failing.
                let finished = self
                    .compose(context, project, None)
                    .args(["port", "--protocol", protocol, service, number])
                    .capture_stdout()
                    .run_ok(&what, &NoLog)?;
                // `0.0.0.0:32768`, or `[::]:32768`.
                let published = finished.stdout.lines().next().unwrap_or("").trim();
                match published.rsplit_once(':') {
                    Some((_, host_port)) if !host_port.is_empty() => {
                        Ok(Value::String(host_port.to_owned()))
                    }
                    _ => Err(RunError::new(format!(
                        "output {output:?}: service {service:?} does not publish port {port}"
                    ))),
                }
            }
            Part::Inspect { service, path } => {
                let finished = self
                    .compose(context, project, None)
                    .args(["ps", "--all", "--quiet", service])
                    .capture_stdout()
                    .run_ok(&what, &NoLog)?;
                let Some(container) = finished.stdout.split_whitespace().next() else {
                    return Err(RunError::new(format!(
                        "output {output:?}: service {service:?} has no container"
                    )));
                };
                let finished = self
                    .docker(context, project)
                    .args(["inspect", container])
                    .capture_stdout()
                    .run_ok(&what, &NoLog)?;
                let inspected = decode_value(finished.stdout.trim().as_bytes())
                    .map_err(|e| RunError::new(format!("{what}: {e}")))?;
                let found = inspected
                    .as_list()
                    .and_then(|list| list.first())
                    .and_then(|container| walk(container, path));
                match found {
                    Some(Value::Null) | None => Err(RunError::new(format!(
                        "output {output:?}: the container of service {service:?} has nothing at {path}"
                    ))),
                    Some(value) => Ok(value.clone()),
                }
            }
        }
    }

    fn read_outputs(
        &self,
        context: &RunContext,
        project: &Project,
    ) -> Result<Vec<Output>, RunError> {
        let mut outputs = Vec::new();
        for (name, parts) in &project.outputs {
            let mut values = Vec::new();
            for part in parts {
                values.push(self.read_part(context, project, name, part)?);
            }
            // One reference alone keeps its type; among text it is text.
            let value = if values.len() == 1 {
                values.remove(0)
            } else {
                Value::String(
                    values
                        .iter()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.canonical(),
                        })
                        .collect(),
                )
            };
            outputs.push(Output {
                name: name.clone(),
                value,
                sensitive: false,
            });
        }
        Ok(outputs)
    }
}

/// Follows `.field` and `[index]` steps into a value.
fn walk<'v>(mut value: &'v Value, path: &str) -> Option<&'v Value> {
    for step in path.split(['.', '[']).filter(|s| !s.is_empty()) {
        value = match step.strip_suffix(']') {
            Some(index) => value.as_list()?.get(index.parse::<usize>().ok()?)?,
            None => value.as_map()?.get(step)?,
        };
    }
    Some(value)
}

/// A whole number, however the JSON decoder chose to hold it.
fn whole(value: &Value) -> Option<u64> {
    match value {
        Value::Int(i) => u64::try_from(*i).ok(),
        Value::Uint(u) => Some(*u),
        Value::Float(f) if *f >= 0.0 && f.fract() == 0.0 => Some(*f as u64),
        _ => None,
    }
}

/// One service of the compiled file, as Compose reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Service {
    pub name: String,
    /// The hash Compose would stamp on a container created from it.
    pub hash: String,
    pub image: String,
    /// The service is built from source, not only pulled.
    pub builds: bool,
    /// How many containers the service should have.
    pub scale: u64,
}

/// What the compiled file asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Desired {
    pub services: Vec<Service>,
    /// The key in the file and the name Docker knows it by, for every
    /// network Compose would create.
    pub networks: Vec<(String, String)>,
    pub volumes: Vec<(String, String)>,
}

/// One container of the project that exists now.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Container {
    pub service: String,
    pub hash: String,
    pub state: String,
    pub name: String,
}

/// What Docker has for the project now.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Actual {
    pub containers: Vec<Container>,
    pub networks: BTreeSet<String>,
    pub volumes: BTreeSet<String>,
}

/// Reads `docker compose config --format json` and `config --hash '*'`.
pub(crate) fn read_desired(config: &str, hashes: &str) -> Result<Desired, String> {
    let config = decode_value(config.trim().as_bytes())?;
    let section = |key: &str| {
        config
            .as_map()
            .and_then(|m| m.get(key))
            .and_then(Value::as_map)
    };
    let hashes: BTreeMap<&str, &str> = hashes
        .lines()
        .filter_map(|line| line.trim().split_once(' '))
        .collect();

    let mut desired = Desired::default();
    for (name, body) in section("services").into_iter().flatten() {
        let body = body.as_map();
        let field = |key: &str| body.and_then(|b| b.get(key));
        let replicas = field("deploy")
            .and_then(Value::as_map)
            .and_then(|d| d.get("replicas"));
        desired.services.push(Service {
            name: name.clone(),
            hash: hashes.get(name.as_str()).copied().unwrap_or("").to_owned(),
            image: field("image")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            builds: field("build").is_some_and(|b| *b != Value::Null),
            scale: field("scale").or(replicas).and_then(whole).unwrap_or(1),
        });
    }
    let named = |key: &str| -> Vec<(String, String)> {
        section(key)
            .into_iter()
            .flatten()
            .filter_map(|(key, body)| {
                let body = body.as_map()?;
                // An external one is somebody else's to create.
                if body
                    .get("external")
                    .is_some_and(|e| *e != Value::Bool(false))
                {
                    return None;
                }
                Some((key.clone(), body.get("name")?.as_str()?.to_owned()))
            })
            .collect()
    };
    desired.networks = named("networks");
    desired.volumes = named("volumes");
    Ok(desired)
}

/// Reads the listing [`CONTAINER_FORMAT`] asks Docker for. A container that
/// `compose run` started is not part of the project's desired state.
pub(crate) fn read_containers(listing: &str) -> Vec<Container> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim().split('|');
            let container = Container {
                service: fields.next()?.to_owned(),
                hash: fields.next()?.to_owned(),
                state: fields.next()?.to_owned(),
                name: fields.next()?.to_owned(),
            };
            let one_off = fields.next().unwrap_or("").eq_ignore_ascii_case("true");
            (!one_off && !container.service.is_empty()).then_some(container)
        })
        .collect()
}

/// What bringing the project up would change.
pub(crate) fn diff(desired: &Desired, actual: &Actual) -> Plan {
    let mut changes = Vec::new();
    let change = |resource: &str, action, summary: String| Change {
        resource: resource.to_owned(),
        action,
        summary,
    };
    for (key, name) in &desired.networks {
        if !actual.networks.contains(name) {
            changes.push(change(key, ChangeAction::Create, format!("network {name}")));
        }
    }
    for (key, name) in &desired.volumes {
        if !actual.volumes.contains(name) {
            changes.push(change(key, ChangeAction::Create, format!("volume {name}")));
        }
    }

    for service in &desired.services {
        let containers: Vec<&Container> = actual
            .containers
            .iter()
            .filter(|c| c.service == service.name)
            .collect();
        let existing = containers.len() as u64;
        let from = if service.builds {
            "built from source".to_owned()
        } else {
            format!("image {}", service.image)
        };
        let stopped = containers.iter().find(|c| c.state != "running");
        if existing == 0 {
            if service.scale > 0 {
                let summary = match service.scale {
                    1 => format!("container, {from}"),
                    n => format!("{n} containers, {from}"),
                };
                changes.push(change(&service.name, ChangeAction::Create, summary));
            }
        } else if service.scale == 0 {
            changes.push(change(
                &service.name,
                ChangeAction::Delete,
                "scaled to no containers".to_owned(),
            ));
        } else if containers.iter().any(|c| c.hash != service.hash) {
            changes.push(change(
                &service.name,
                ChangeAction::Replace,
                "its configuration changed, so its container is recreated".to_owned(),
            ));
        } else if existing != service.scale {
            changes.push(change(
                &service.name,
                ChangeAction::Update,
                format!("scaled from {existing} to {}", service.scale),
            ));
        } else if let Some(container) = stopped {
            changes.push(change(
                &service.name,
                ChangeAction::Update,
                format!(
                    "started; container {} is {}",
                    container.name, container.state
                ),
            ));
        }
    }

    // `up --remove-orphans` removes the containers of a service the file no
    // longer declares.
    let orphans: BTreeSet<&str> = actual
        .containers
        .iter()
        .map(|c| c.service.as_str())
        .filter(|name| !desired.services.iter().any(|s| s.name == *name))
        .collect();
    for orphan in orphans {
        changes.push(change(
            orphan,
            ChangeAction::Delete,
            "no longer in the document, so its containers are removed".to_owned(),
        ));
    }

    Plan {
        changes,
        // Whether a build would produce a different image cannot be known
        // without building it.
        unknown: desired.services.iter().any(|s| s.builds),
        saved: None,
    }
}

impl Runner for DockerRunner {
    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let project = self.prepare(context, artifact)?;
        let secrets = secrets_file(context)?;
        let (_, plan) = self.survey(context, &project, secrets.as_ref(), log)?;
        drop(secrets);
        for change in &plan.changes {
            log.out(&change.to_string());
        }
        Ok(plan)
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let project = self.prepare(context, artifact)?;
        let secrets = secrets_file(context)?;
        let (desired, plan) = self.survey(context, &project, secrets.as_ref(), log)?;
        let mut up = self.compose(context, &project, secrets.as_ref()).args([
            "up",
            "--detach",
            "--remove-orphans",
        ]);
        // An image built from source is built again, so a change to the
        // source is applied and not only a change to the file.
        if desired.services.iter().any(|s| s.builds) {
            up = up.arg("--build");
        }
        if project.wait {
            up = up.arg("--wait");
            if project.wait_timeout > 0 {
                up = up.args(["--wait-timeout", &project.wait_timeout.to_string()]);
            }
        }
        up.args(self.extra_args.iter().cloned()).run_ok(
            "docker compose could not bring the project up; what it printed says which service \
             failed",
            log,
        )?;
        drop(secrets);
        Ok(Applied {
            changes: plan.changes,
            outputs: self.read_outputs(context, &project)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let settings =
            std::fs::read_to_string(context.work_dir.join(PROJECT_PATH)).map_err(|_| {
                RunError::new(
                    "nothing has been applied for this document yet, so it has no outputs",
                )
            })?;
        let project = Project::read(&settings)?;
        self.read_outputs(context, &project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, hash: &str) -> Service {
        Service {
            name: name.to_owned(),
            hash: hash.to_owned(),
            image: format!("{name}:1"),
            builds: false,
            scale: 1,
        }
    }

    fn container(service: &str, hash: &str, state: &str) -> Container {
        Container {
            service: service.to_owned(),
            hash: hash.to_owned(),
            state: state.to_owned(),
            name: format!("shop-{service}-1"),
        }
    }

    fn shown(plan: &Plan) -> Vec<String> {
        plan.changes.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn composes_own_reading_of_the_file_becomes_the_desired_state() {
        let config = r#"{
          "name": "shop",
          "services": {
            "web": {"image": "nginx:1.27", "deploy": {"replicas": 3}},
            "api": {"build": {"context": "/srv/api"}, "scale": 2},
            "db": {"image": "postgres:17"}
          },
          "networks": {
            "default": {"name": "shop_default", "ipam": {}},
            "edge": {"name": "edge", "external": true}
          },
          "volumes": {"data": {"name": "shop_data"}}
        }"#;
        let desired = read_desired(config, "web aaa\napi bbb\ndb ccc\n").unwrap();
        assert_eq!(
            desired.services,
            [
                Service {
                    name: "api".into(),
                    hash: "bbb".into(),
                    image: String::new(),
                    builds: true,
                    scale: 2,
                },
                service("db", "ccc").with_image("postgres:17"),
                Service {
                    scale: 3,
                    ..service("web", "aaa").with_image("nginx:1.27")
                },
            ]
        );
        // The external network is not Compose's to create.
        assert_eq!(
            desired.networks,
            [("default".into(), "shop_default".into())]
        );
        assert_eq!(desired.volumes, [("data".into(), "shop_data".into())]);
        assert!(read_desired("not json", "").is_err());
    }

    impl Service {
        fn with_image(mut self, image: &str) -> Service {
            image.clone_into(&mut self.image);
            self
        }
    }

    #[test]
    fn a_container_listing_leaves_out_what_compose_run_started() {
        let listing = "web|aaa|running|shop-web-1|False\n\
                       db|ccc|exited|shop-db-1|False\n\
                       web|aaa|running|shop-web-run-1f2e|True\n\
                       \n";
        assert_eq!(
            read_containers(listing),
            [
                container("web", "aaa", "running"),
                container("db", "ccc", "exited")
            ]
        );
    }

    #[test]
    fn a_project_that_is_not_running_is_all_creates() {
        let desired = Desired {
            services: vec![service("db", "ccc"), service("web", "aaa")],
            networks: vec![("default".into(), "shop_default".into())],
            volumes: vec![("data".into(), "shop_data".into())],
        };
        let plan = diff(&desired, &Actual::default());
        assert_eq!(
            shown(&plan),
            [
                "create default: network shop_default",
                "create data: volume shop_data",
                "create db: container, image db:1",
                "create web: container, image web:1",
            ]
        );
        assert!(!plan.unknown);
    }

    #[test]
    fn a_project_that_matches_the_file_is_in_sync() {
        let desired = Desired {
            services: vec![service("db", "ccc"), service("web", "aaa")],
            networks: vec![("default".into(), "shop_default".into())],
            volumes: vec![("data".into(), "shop_data".into())],
        };
        let actual = Actual {
            containers: vec![
                container("db", "ccc", "running"),
                container("web", "aaa", "running"),
            ],
            networks: ["shop_default".to_owned()].into(),
            volumes: ["shop_data".to_owned()].into(),
        };
        assert!(diff(&desired, &actual).in_sync());
    }

    #[test]
    fn each_kind_of_difference_is_its_own_change() {
        let desired = Desired {
            services: vec![
                service("changed", "new"),
                Service {
                    scale: 3,
                    ..service("grown", "g")
                },
                Service {
                    scale: 0,
                    ..service("idle", "i")
                },
                Service {
                    scale: 0,
                    ..service("never", "n")
                },
                service("stopped", "s"),
                service("steady", "t"),
            ],
            ..Desired::default()
        };
        let actual = Actual {
            containers: vec![
                container("changed", "old", "running"),
                container("grown", "g", "running"),
                container("idle", "i", "running"),
                container("stopped", "s", "exited"),
                container("steady", "t", "running"),
                container("gone", "x", "running"),
                Container {
                    name: "shop-gone-2".into(),
                    ..container("gone", "x", "running")
                },
            ],
            ..Actual::default()
        };
        assert_eq!(
            shown(&diff(&desired, &actual)),
            [
                "replace changed: its configuration changed, so its container is recreated",
                "update grown: scaled from 1 to 3",
                "delete idle: scaled to no containers",
                "update stopped: started; container shop-stopped-1 is exited",
                // Two containers of one orphaned service are one change.
                "delete gone: no longer in the document, so its containers are removed",
            ]
        );
    }

    #[test]
    fn a_service_built_from_source_makes_the_plan_uncertain() {
        let desired = Desired {
            services: vec![Service {
                builds: true,
                ..service("api", "b")
            }],
            ..Desired::default()
        };
        let running = Actual {
            containers: vec![container("api", "b", "running")],
            ..Actual::default()
        };
        let plan = diff(&desired, &running);
        assert!(plan.changes.is_empty());
        assert!(plan.unknown && !plan.in_sync());
        assert_eq!(
            shown(&diff(&desired, &Actual::default())),
            ["create api: container, built from source"]
        );
    }

    #[test]
    fn a_path_walks_fields_and_indexes() {
        let value =
            decode_value(br#"{"State":{"Status":"running"},"Mounts":[{"Source":"/data"}]}"#)
                .unwrap();
        assert_eq!(
            walk(&value, ".State.Status"),
            Some(&Value::String("running".into()))
        );
        assert_eq!(
            walk(&value, ".Mounts[0].Source"),
            Some(&Value::String("/data".into()))
        );
        assert_eq!(walk(&value, ".Mounts[1].Source"), None);
        assert_eq!(walk(&value, ".State.Missing"), None);
    }
}
