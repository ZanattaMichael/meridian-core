//! The half that applies manifests with `kubectl`. It sits outside the
//! compile chain: nothing here is reached by a compile.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog, Operations, Output, Plan,
    RunContext, RunError, Runner, Value, Written,
};
use meridian_sdk::{settings, Setting};

use crate::{secret_token, CLUSTER_PATH, MANIFESTS_PATH};

/// Names the program to run, when it is not `kubectl` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_KUBECTL";
/// Extra arguments for every call, split like a shell command line.
pub const ARGS_VARIABLE: &str = "MERIDIAN_KUBECTL_ARGS";
/// How many seconds an apply waits for an output the cluster has not filled
/// in yet, such as a load balancer's address.
pub const OUTPUT_WAIT_VARIABLE: &str = "MERIDIAN_KUBECTL_OUTPUT_WAIT";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "kubectl",
            "The `kubectl` program to run, when it is not on `PATH`. Shared with the helm target.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for every `kubectl` call, split like a shell command line: `--context prod`. Shared with the helm target.",
        ),
        Setting::new(
            "output-wait",
            OUTPUT_WAIT_VARIABLE,
            "120",
            "How many seconds an apply waits for an output the cluster has not filled in yet, such as a load balancer's address.",
        ),
    ]
}

/// Applies compiled manifests to a cluster.
///
/// A plan is `kubectl apply --dry-run=server`: the API server runs the whole
/// request, admission included, and stores nothing. An apply is
/// `kubectl apply`, and its own report is the list of changes.
pub struct KubernetesRunner {
    pub program: String,
    /// Arguments added to every call, after Meridian's own.
    pub extra_args: Vec<String>,
    /// How long an apply waits for an output that has no value yet.
    pub output_wait: Duration,
    /// How long it sleeps between two looks.
    pub output_poll: Duration,
}

/// One piece of an output, as `cluster.json` records it.
enum Part {
    Text(String),
    Read {
        get: String,
        name: String,
        namespace: String,
        path: String,
    },
}

/// What `cluster.json` says.
struct Cluster {
    context: String,
    namespace: String,
    kubeconfig: String,
    /// What `kubectl` calls an object (`deployment.apps/web`) to the id the
    /// author wrote.
    objects: BTreeMap<String, String>,
    outputs: Vec<(String, Vec<Part>)>,
}

impl Cluster {
    fn read(text: &str) -> Result<Cluster, RunError> {
        let bad = || RunError::new(format!("{CLUSTER_PATH} is not one this plugin compiled"));
        let value = decode_value(text.as_bytes()).map_err(|_| bad())?;
        let map = value.as_map().ok_or_else(bad)?;
        let string = |map: &BTreeMap<String, Value>, key: &str| {
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
                parts.push(match part.get("text").and_then(Value::as_str) {
                    Some(text) => Part::Text(text.to_owned()),
                    None => Part::Read {
                        get: string(part, "get"),
                        name: string(part, "name"),
                        namespace: string(part, "namespace"),
                        path: string(part, "path"),
                    },
                });
            }
            outputs.push((string(output, "name"), parts));
        }
        Ok(Cluster {
            context: string(map, "context"),
            namespace: string(map, "namespace"),
            kubeconfig: string(map, "kubeconfig"),
            objects: map
                .get("objects")
                .and_then(Value::as_map)
                .ok_or_else(bad)?
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect(),
            outputs,
        })
    }
}

impl KubernetesRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        KubernetesRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
            output_wait: Duration::from_secs(
                settings::text(setting("output-wait"))
                    .parse()
                    .unwrap_or(120),
            ),
            output_poll: Duration::from_secs(2),
        }
    }

    /// One `kubectl` call against the document's cluster. `namespace` is the
    /// object's own when it sets one, and the cluster's otherwise.
    fn kubectl(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        operation: &[&str],
        namespace: &str,
    ) -> Tool {
        let mut tool = Tool::new(&self.program)
            .args(operation.iter().copied())
            .dir(&context.work_dir)
            .capture_stdout();
        let namespace = if namespace.is_empty() {
            cluster.namespace.as_str()
        } else {
            namespace
        };
        for (flag, value) in [
            ("--context", cluster.context.as_str()),
            ("--namespace", namespace),
            ("--kubeconfig", cluster.kubeconfig.as_str()),
        ] {
            if !value.is_empty() {
                tool = tool.args([flag, value]);
            }
        }
        tool.args(self.extra_args.iter().cloned())
    }

    /// `kubectl apply`, given the manifests. With no secrets in the run it
    /// reads the file in the work directory. With secrets, the file holds
    /// a token where each value goes, and the manifests are put together
    /// with the values in protected memory and handed over on standard
    /// input, so that no file ever holds a value: the first choice of
    /// channel, and the only copy is the API server's.
    fn apply_manifests(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        artifact: &Artifact,
        before: &[&str],
    ) -> Result<Tool, RunError> {
        let mut operation = vec!["apply"];
        operation.extend_from_slice(before);
        if context.secrets.is_empty() {
            operation.extend(["--filename", MANIFESTS_PATH]);
            return Ok(self.kubectl(context, cluster, &operation, ""));
        }
        operation.extend(["--filename", "-"]);
        let text = artifact
            .files
            .get(MANIFESTS_PATH)
            .map(String::as_str)
            .unwrap_or("");
        let filled = context
            .secrets
            .filled_text(text, secret_token, Written::JsonString)
            .map_err(|e| {
                RunError::new(format!(
                    "the secrets could not be put into the manifests for kubectl: {e}"
                ))
            })?;
        Ok(self
            .kubectl(context, cluster, &operation, "")
            .stdin_secret(filled))
    }

    fn prepare(&self, context: &RunContext, artifact: &Artifact) -> Result<Cluster, RunError> {
        let (Some(_), Some(settings)) = (
            artifact.files.get(MANIFESTS_PATH),
            artifact.files.get(CLUSTER_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {MANIFESTS_PATH} and {CLUSTER_PATH}; it was not compiled by this plugin"
            )));
        };
        let cluster = Cluster::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;
        Ok(cluster)
    }

    /// Reads every output from the live objects. An output with no value is
    /// looked at again until `wait` has passed, then reported as missing.
    fn read_outputs(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        wait: Duration,
        log: &dyn Log,
    ) -> Result<Vec<Output>, RunError> {
        let started = Instant::now();
        let mut outputs = Vec::new();
        for (name, parts) in &cluster.outputs {
            let mut value = String::new();
            let mut sensitive = false;
            for part in parts {
                let (get, object, namespace, path) = match part {
                    Part::Text(text) => {
                        value.push_str(text);
                        continue;
                    }
                    Part::Read {
                        get,
                        name,
                        namespace,
                        path,
                    } => (get, name, namespace, path),
                };
                sensitive |= get == "secret";
                let jsonpath = format!("jsonpath={{{path}}}");
                let mut said_waiting = false;
                let read = loop {
                    let finished = self
                        .kubectl(
                            context,
                            cluster,
                            &["get", get, object, "--output", &jsonpath],
                            namespace,
                        )
                        .run(&NoLog)?;
                    // A list the cluster has not filled in yet is an index
                    // out of bounds to kubectl, and no different from a field
                    // that is not there yet.
                    if !finished.success() && !finished.stderr.contains("index out of bounds") {
                        return Err(
                            finished.into_error(format!("kubectl could not read output {name:?}"))
                        );
                    }
                    if !finished.stdout.trim().is_empty() {
                        break finished.stdout.trim().to_owned();
                    }
                    if started.elapsed() >= wait {
                        return Err(RunError::new(format!(
                            "output {name:?}: {get}/{object} has no value at {path} yet; the \
                             cluster may still be filling it in, so read the outputs again later"
                        )));
                    }
                    if !said_waiting {
                        log.out(&format!(
                            "output {name}: waiting for {get}/{object} to have {path}"
                        ));
                        said_waiting = true;
                    }
                    std::thread::sleep(self.output_poll);
                };
                value.push_str(&read);
            }
            outputs.push(Output {
                name: name.clone(),
                value: Value::String(value),
                sensitive,
            });
        }
        Ok(outputs)
    }
}

/// Reads what `kubectl apply` printed: one line an object, such as
/// `deployment.apps/web configured (server dry run)`.
fn read_report(stdout: &str, cluster: &Cluster) -> Vec<Change> {
    let mut changes = Vec::new();
    for line in stdout.lines() {
        let mut words = line.split_whitespace();
        let (Some(object), Some(verb)) = (words.next(), words.next()) else {
            continue;
        };
        let action = match verb {
            "created" => ChangeAction::Create,
            "configured" | "serverside-applied" => ChangeAction::Update,
            // "unchanged", and anything that is not a report line.
            _ => continue,
        };
        changes.push(Change {
            resource: cluster
                .objects
                .get(object)
                .cloned()
                .unwrap_or_else(|| object.to_owned()),
            action,
            summary: object.to_owned(),
        });
    }
    changes
}

/// Whether a failed dry run failed only because objects sit in a namespace
/// the same dry run would create. Nothing is stored in a dry run, so the
/// namespace is not there for them, though it will be in the apply.
fn only_missing_new_namespaces(stdout: &str, stderr: &str) -> bool {
    let created: Vec<&str> = stdout
        .lines()
        .filter_map(|line| {
            let (object, rest) = line.split_once(' ')?;
            let name = object.strip_prefix("namespace/")?;
            rest.starts_with("created").then_some(name)
        })
        .collect();
    let mut errors = stderr.lines().filter(|l| !l.trim().is_empty()).peekable();
    errors.peek().is_some()
        && errors.all(|line| {
            created
                .iter()
                .any(|name| line.contains(&format!("namespaces \"{name}\" not found")))
        })
}

impl Runner for KubernetesRunner {
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
        let cluster = self.prepare(context, artifact)?;
        log.out("asking the cluster what an apply would change");
        let finished = self
            .apply_manifests(context, &cluster, artifact, &["--dry-run=server"])?
            .run(log)?;
        let mut plan = Plan {
            changes: read_report(&finished.stdout, &cluster),
            unknown: false,
            saved: None,
        };
        if !finished.success() {
            if !only_missing_new_namespaces(&finished.stdout, &finished.stderr) {
                return Err(finished.into_error("kubectl could not preview the apply"));
            }
            log.out(
                "some objects go in a namespace this document creates, so the cluster cannot \
                 preview them until the namespace exists",
            );
            plan.unknown = true;
        }
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
        let cluster = self.prepare(context, artifact)?;
        let finished = self
            .apply_manifests(context, &cluster, artifact, &[])?
            .run_ok("kubectl could not apply the manifests", log)?;
        let changes = read_report(&finished.stdout, &cluster);
        for change in &changes {
            log.out(&change.to_string());
        }
        Ok(Applied {
            changes,
            outputs: self.read_outputs(context, &cluster, self.output_wait, log)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let settings =
            std::fs::read_to_string(context.work_dir.join(CLUSTER_PATH)).map_err(|_| {
                RunError::new(
                    "nothing has been applied for this document yet, so it has no outputs",
                )
            })?;
        let cluster = Cluster::read(&settings)?;
        self.read_outputs(context, &cluster, Duration::ZERO, &NoLog)
    }
}
