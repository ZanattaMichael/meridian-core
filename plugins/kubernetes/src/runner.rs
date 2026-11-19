//! The half that applies manifests with `kubectl`. It sits outside the
//! compile chain: nothing here is reached by a compile.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::tool::{write_artifact, Finished, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, ChangeAction, Log, NoLog, Operations, Output, Plan,
    RunContext, RunError, Runner, Value, Written,
};
use meridian_sdk::{settings, Setting};

use crate::failures::{read_errors, same_kind};
use crate::{secret_token, CLUSTER_PATH, MANIFESTS_PATH};

/// What `--tool-debug` adds to the tool, level 1 first. kubectl logs to standard error.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] = &[
    meridian_sdk::debug::DebugLevel::args(&["-v=4"]),
    meridian_sdk::debug::DebugLevel::args(&["-v=6"]),
    meridian_sdk::debug::DebugLevel::args(&["-v=8"]),
];

/// Names the program to run, when it is not `kubectl` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_KUBECTL";
/// Extra arguments for every call, split like a shell command line.
pub const ARGS_VARIABLE: &str = "MERIDIAN_KUBECTL_ARGS";
/// How many seconds an apply waits for an output the cluster has not filled
/// in yet, such as a load balancer's address.
pub const OUTPUT_WAIT_VARIABLE: &str = "MERIDIAN_KUBECTL_OUTPUT_WAIT";
/// How many seconds an apply waits for one wave's objects to be ready.
pub const WAIT_TIMEOUT_VARIABLE: &str = "MERIDIAN_KUBECTL_WAIT_TIMEOUT";

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
        Setting::new(
            "wait-timeout",
            WAIT_TIMEOUT_VARIABLE,
            "300",
            "How many seconds an apply waits for each object of a `dependsOn` wave to be ready (a rollout, a Job, a CustomResourceDefinition) before it fails with `timed-out`.",
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
    /// How long an apply waits for one object of a wave to be ready.
    pub wait_timeout: Duration,
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
    /// The waves `dependsOn` splits the objects into; empty for one.
    waves: Vec<Wave>,
}

/// One wave: its file, and what to wait for before the next.
struct Wave {
    file: String,
    wait: Vec<Wait>,
}

/// An object to wait for: `rollout`, `complete` or `established`.
struct Wait {
    object: String,
    namespace: String,
    until: String,
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
        let mut waves = Vec::new();
        for wave in map.get("waves").and_then(Value::as_list).unwrap_or(&[]) {
            let wave = wave.as_map().ok_or_else(bad)?;
            let mut wait = Vec::new();
            for object in wave.get("wait").and_then(Value::as_list).unwrap_or(&[]) {
                let object = object.as_map().ok_or_else(bad)?;
                wait.push(Wait {
                    object: string(object, "object"),
                    namespace: string(object, "namespace"),
                    until: string(object, "for"),
                });
            }
            waves.push(Wave {
                file: string(wave, "file"),
                wait,
            });
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
            waves,
        })
    }
}

impl Cluster {
    /// The id of the object kubectl named, or kubectl's name for it when
    /// the document has no such object. With no name, the one object of
    /// that kind.
    fn resource_of(&self, kind: &str, name: &str) -> String {
        let mut matching = self.objects.iter().filter(|(object, _)| {
            let Some((declared, declared_name)) = object.split_once('/') else {
                return false;
            };
            let declared = declared.split('.').next().unwrap_or_default();
            same_kind(kind, declared) && (name.is_empty() || declared_name == name)
        });
        match (matching.next(), matching.next()) {
            (Some((_, id)), None) => id.clone(),
            _ if name.is_empty() => kind.to_owned(),
            _ => format!("{kind}/{name}"),
        }
    }

    /// The failure a finished `kubectl` call amounts to, with the objects
    /// its errors name.
    fn error(&self, finished: Finished, what: &str) -> RunError {
        let failures = read_errors(&finished.stderr)
            .into_iter()
            .map(|found| {
                let mut failure = found.failure;
                if !found.kind.is_empty() {
                    failure.resource = self.resource_of(&found.kind, &found.name);
                }
                failure
            })
            .collect();
        let mut err = finished.into_error(what);
        err.failures = failures;
        err
    }

    /// `kubectl` run to its end, its failure carrying what it named.
    fn run_ok(&self, tool: Tool, what: &str, log: &dyn Log) -> Result<Finished, RunError> {
        let finished = tool.run(log)?;
        if finished.success() {
            Ok(finished)
        } else {
            Err(self.error(finished, what))
        }
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
            wait_timeout: Duration::from_secs(
                settings::text(setting("wait-timeout"))
                    .parse()
                    .unwrap_or(300),
            ),
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
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
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
        self.with_file(context, cluster, artifact, MANIFESTS_PATH, "apply", before)
    }

    /// `kubectl <verb>`, given the manifests as [`Self::apply_manifests`]
    /// gives them.
    fn with_manifests(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        artifact: &Artifact,
        verb: &str,
        before: &[&str],
    ) -> Result<Tool, RunError> {
        self.with_file(context, cluster, artifact, MANIFESTS_PATH, verb, before)
    }

    /// `kubectl <verb>` of the manifests in `file`, given as
    /// [`Self::apply_manifests`] gives them.
    fn with_file(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        artifact: &Artifact,
        file: &str,
        verb: &str,
        before: &[&str],
    ) -> Result<Tool, RunError> {
        let mut operation = vec![verb];
        operation.extend_from_slice(before);
        if context.secrets.is_empty() {
            operation.extend(["--filename", file]);
            return Ok(self.kubectl(context, cluster, &operation, ""));
        }
        operation.extend(["--filename", "-"]);
        let text = artifact.files.get(file).map(String::as_str).unwrap_or("");
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

    /// Applies one wave at a time, waiting for each wave's objects before
    /// the next. A failure carries the changes made before it.
    fn apply_waves(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let mut changes = Vec::new();
        let count = cluster.waves.len();
        for (n, wave) in cluster.waves.iter().enumerate() {
            log.out(&format!("applying wave {} of {count}", n + 1));
            let applied = self
                .with_file(context, cluster, artifact, &wave.file, "apply", &[])
                .and_then(|tool| {
                    cluster.run_ok(tool, "kubectl could not apply the manifests", log)
                });
            let finished = match applied {
                Ok(finished) => finished,
                Err(mut err) => {
                    err.changes = changes;
                    return Err(err);
                }
            };
            for change in read_report(&finished.stdout, cluster) {
                log.out(&change.to_string());
                changes.push(change);
            }
            for wait in &wave.wait {
                if let Err(mut err) = self.wait(context, cluster, wait, log) {
                    err.changes = changes;
                    return Err(err);
                }
            }
        }
        Ok(changes)
    }

    /// Waits until `wait`'s object is ready: a rollout done, a Job complete,
    /// a CustomResourceDefinition established.
    fn wait(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        wait: &Wait,
        log: &dyn Log,
    ) -> Result<(), RunError> {
        let resource = cluster
            .objects
            .get(&wait.object)
            .cloned()
            .unwrap_or_else(|| wait.object.clone());
        let timeout = format!("--timeout={}s", self.wait_timeout.as_secs());
        let timed_out = |what: &str| {
            let mut err = RunError::new(format!(
                "{} did not {what} within {} s",
                wait.object,
                self.wait_timeout.as_secs()
            ));
            err.failures.push(Failure::new(
                Cause::TimedOut,
                resource.clone(),
                format!("{} did not {what} in time", wait.object),
            ));
            err
        };
        let (operation, what): (Vec<&str>, &str) = match wait.until.as_str() {
            "rollout" => (
                vec!["rollout", "status", &wait.object, &timeout],
                "finish its rollout",
            ),
            "established" => (
                vec![
                    "wait",
                    "--for=condition=Established",
                    &wait.object,
                    &timeout,
                ],
                "become established",
            ),
            "complete" => return self.wait_for_job(context, cluster, wait, &resource, log),
            _ => return Ok(()),
        };
        log.out(&format!("waiting for {} to {what}", wait.object));
        let finished = self
            .kubectl(context, cluster, &operation, &wait.namespace)
            .run(log)?;
        if finished.success() {
            return Ok(());
        }
        if finished.stderr.contains("timed out") {
            let mut err = timed_out(what);
            err.output = finished.stderr;
            return Err(err);
        }
        let mut err = cluster.error(finished, &format!("kubectl could not wait for {resource}"));
        if err.failures.is_empty() {
            err.failures.push(Failure::new(
                Cause::ResourceFailed,
                resource,
                format!("{} did not {what}", wait.object),
            ));
        }
        Err(err)
    }

    /// Looks at a Job until it is complete or failed. `kubectl wait` cannot
    /// wait for either, so a failed Job would otherwise wait the whole time.
    fn wait_for_job(
        &self,
        context: &RunContext,
        cluster: &Cluster,
        wait: &Wait,
        resource: &str,
        log: &dyn Log,
    ) -> Result<(), RunError> {
        log.out(&format!("waiting for {} to complete", wait.object));
        let started = Instant::now();
        let conditions = "jsonpath={.status.conditions[?(@.status==\"True\")].type}";
        loop {
            let finished = self
                .kubectl(
                    context,
                    cluster,
                    &["get", &wait.object, "--output", conditions],
                    &wait.namespace,
                )
                .run(&NoLog)?;
            if !finished.success() {
                return Err(cluster.error(finished, &format!("kubectl could not read {resource}")));
            }
            let conditions: Vec<&str> = finished.stdout.split_whitespace().collect();
            if conditions.contains(&"Complete") {
                return Ok(());
            }
            if conditions.contains(&"Failed") {
                let mut err = RunError::new(format!("{} failed", wait.object));
                err.failures.push(Failure::new(
                    Cause::ResourceFailed,
                    resource,
                    format!("{} failed", wait.object),
                ));
                return Err(err);
            }
            if started.elapsed() >= self.wait_timeout {
                let mut err = RunError::new(format!(
                    "{} did not complete within {} s",
                    wait.object,
                    self.wait_timeout.as_secs()
                ));
                err.failures.push(Failure::new(
                    Cause::TimedOut,
                    resource,
                    format!("{} did not complete in time", wait.object),
                ));
                return Err(err);
            }
            std::thread::sleep(self.output_poll);
        }
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

/// Reads what `kubectl delete` printed: one line an object, such as
/// `deployment.apps "web" deleted (server dry run)`.
fn read_deleted(stdout: &str, cluster: &Cluster) -> Vec<Change> {
    stdout
        .lines()
        .filter_map(|line| {
            let (kind, rest) = line.trim().split_once(' ')?;
            let (name, rest) = rest.strip_prefix('"')?.split_once('"')?;
            if !rest.trim_start().starts_with("deleted") {
                return None;
            }
            let object = format!("{kind}/{name}");
            Some(Change {
                resource: cluster
                    .objects
                    .get(&object)
                    .cloned()
                    .unwrap_or_else(|| object.clone()),
                action: ChangeAction::Delete,
                summary: object,
            })
        })
        .collect()
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
    fn debug_levels(&self) -> &'static [meridian_sdk::debug::DebugLevel] {
        DEBUG
    }

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
                return Err(cluster.error(finished, "kubectl could not preview the apply"));
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
        let changes = if cluster.waves.is_empty() {
            let finished = cluster.run_ok(
                self.apply_manifests(context, &cluster, artifact, &[])?,
                "kubectl could not apply the manifests",
                log,
            )?;
            let changes = read_report(&finished.stdout, &cluster);
            for change in &changes {
                log.out(&change.to_string());
            }
            changes
        } else {
            self.apply_waves(context, &cluster, artifact, log)?
        };
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

    fn inspects(&self) -> bool {
        true
    }

    /// `kubectl get` reads the object; a server-side dry run of the
    /// manifests, which stores nothing, says whether an apply would change it.
    fn inspect(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        resource: &str,
        _endpoint: &str,
    ) -> Result<meridian_sdk::Inspection, RunError> {
        let cluster = self.prepare(context, artifact)?;
        let names: Vec<&str> = cluster
            .objects
            .iter()
            .filter(|(_, id)| id.as_str() == resource)
            .map(|(object, _)| object.as_str())
            .collect();
        let manifests = &artifact.files[MANIFESTS_PATH];
        let declared = crate::inspect::declared(manifests, &names)
            .map_err(|e| RunError::new(format!("{MANIFESTS_PATH} could not be read: {e}")))?;
        let declared = match declared.as_slice() {
            [one] => one,
            [] => {
                return Err(RunError::new(format!(
                    "{MANIFESTS_PATH} has no object for {resource:?}"
                )))
            }
            _ => {
                return Err(RunError::new(format!(
                    "{resource} compiles to more than one object ({})",
                    meridian_sdk::rules::UNSUPPORTED_INSPECT.name
                )))
            }
        };
        let log = &NoLog;
        let got = self
            .kubectl(
                context,
                &cluster,
                &["get", &declared.applied_as, "--output", "json"],
                &declared.namespace,
            )
            .run(log)?;
        let command = format!(
            "kubectl get {} --output json; kubectl apply --dry-run=server",
            declared.applied_as
        );
        if !got.success() {
            if got.stderr.contains("NotFound") {
                return Ok(meridian_sdk::Inspection {
                    found: false,
                    in_desired_state: Some(false),
                    command,
                    ..Default::default()
                });
            }
            return Err(cluster.error(got, "kubectl could not read the object"));
        }
        let actual = crate::inspect::actual(&got.stdout, declared)
            .map_err(|e| RunError::new(format!("kubectl printed what is not JSON: {e}")))?;
        let dry = self
            .apply_manifests(context, &cluster, artifact, &["--dry-run=server"])?
            .run(log)?;
        let in_desired_state = dry.success().then(|| {
            !read_report(&dry.stdout, &cluster)
                .iter()
                .any(|c| c.summary == declared.applied_as)
        });
        Ok(meridian_sdk::Inspection {
            found: true,
            actual,
            in_desired_state,
            command,
        })
    }

    fn destroys(&self) -> bool {
        true
    }

    fn plan_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let cluster = self.prepare(context, artifact)?;
        log.out("asking the cluster what a delete would remove");
        let finished = cluster.run_ok(
            self.with_manifests(
                context,
                &cluster,
                artifact,
                "delete",
                &["--ignore-not-found", "--dry-run=server"],
            )?,
            "kubectl could not preview the delete",
            log,
        )?;
        Ok(Plan {
            changes: read_deleted(&finished.stdout, &cluster),
            unknown: false,
            saved: None,
        })
    }

    /// `kubectl delete` of every object in the manifests, waiting until
    /// each is gone. An object that is already gone is not an error.
    fn destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let cluster = self.prepare(context, artifact)?;
        let finished = cluster.run_ok(
            self.with_manifests(
                context,
                &cluster,
                artifact,
                "delete",
                &["--ignore-not-found", "--wait"],
            )?,
            "kubectl could not delete the objects",
            log,
        )?;
        let removed = read_deleted(&finished.stdout, &cluster);
        for change in &removed {
            log.out(&change.to_string());
        }
        Ok(removed)
    }
}
