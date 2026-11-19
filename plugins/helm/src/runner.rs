//! The half that installs a chart with `helm`. It sits outside the compile
//! chain: nothing here is reached by a compile.
//!
//! A plan renders the chart and compares the result with the manifest of the
//! release Helm last installed. An apply installs or upgrades the release.
//! When the document has a Kustomize overlay, Helm renders the chart,
//! Kustomize patches what it rendered, and Helm installs the patched objects,
//! so the result is still one Helm release with a history and a rollback.

use std::collections::BTreeMap;
use std::fs;

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    decode_value, Applied, Artifact, Change, Log, NoLog, Operations, Output, Plan, RunContext,
    RunError, Runner, Value, Written,
};
use meridian_sdk::{settings, Setting};
use meridian_target_kubernetes::failures::{read_errors, same_kind};
use meridian_yaml::Yaml;

use crate::emit::RENDERED_FILE;
use crate::secret_token;

/// What `--tool-debug` adds to the tool, level 1 first. Helm has one debug level.
pub const DEBUG: &[meridian_sdk::debug::DebugLevel] =
    &[meridian_sdk::debug::DebugLevel::args(&["--debug"])];

/// Helm given the run's secrets, when it has any: the chart's `values.yaml`
/// with each token replaced by its value, put together in protected memory
/// and read by Helm on standard input as one more values file, which
/// overrides the one in the chart key by key. Nothing on disk holds a
/// value; what Helm keeps in the release is the cluster's.
fn with_values(
    tool: Tool,
    context: &RunContext,
    artifact: &Artifact,
    chart_dir: &str,
) -> Result<Tool, RunError> {
    if context.secrets.is_empty() {
        return Ok(tool);
    }
    let values = artifact
        .files
        .get(&format!("{chart_dir}/values.yaml"))
        .map(String::as_str)
        .unwrap_or("");
    let filled = context
        .secrets
        .filled_text(values, secret_token, Written::JsonString)
        .map_err(|e| {
            RunError::new(format!(
                "the secrets could not be put into the values for helm: {e}"
            ))
        })?;
    Ok(tool.args(["--values", "-"]).stdin_secret(filled))
}
use crate::manifest::{self, Key};
use crate::path::{self, Step};
use crate::{template, CHART_DIR, KUSTOMIZE_DIR, RELEASE_PATH};

/// Names the program to run, when it is not `helm` on `PATH`.
pub const HELM_VARIABLE: &str = "MERIDIAN_HELM";
/// Extra arguments for every `helm` call, split like a shell command line:
/// `--kube-context prod`.
pub const HELM_ARGS_VARIABLE: &str = "MERIDIAN_HELM_ARGS";
/// The command that builds an overlay, when it is not `kustomize build`:
/// `kubectl kustomize` uses the copy built into kubectl.
pub const KUSTOMIZE_VARIABLE: &str = "MERIDIAN_KUSTOMIZE";
/// Names the program that reads outputs, when it is not `kubectl` on `PATH`.
pub const KUBECTL_VARIABLE: &str = "MERIDIAN_KUBECTL";
/// Extra arguments for every `kubectl` call, split like a shell command line:
/// `--context prod`.
pub const KUBECTL_ARGS_VARIABLE: &str = "MERIDIAN_KUBECTL_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "helm",
            HELM_VARIABLE,
            "helm",
            "The `helm` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "helm-args",
            HELM_ARGS_VARIABLE,
            "",
            "Extra arguments for every `helm` call, split like a shell command line: `--kube-context prod`.",
        ),
        Setting::new(
            "kustomize",
            KUSTOMIZE_VARIABLE,
            "kustomize build",
            "The command that builds an overlay, when it is not `kustomize build`: `kubectl kustomize` uses the copy built into kubectl.",
        ),
        Setting::new(
            "kubectl",
            KUBECTL_VARIABLE,
            "kubectl",
            "The `kubectl` program that reads outputs, when it is not on `PATH`. Shared with the kubernetes target.",
        ),
        Setting::new(
            "kubectl-args",
            KUBECTL_ARGS_VARIABLE,
            "",
            "Extra arguments for every `kubectl` call, split like a shell command line: `--context prod`. Shared with the kubernetes target.",
        ),
    ]
}

/// The directory of the work directory that holds the patched objects as a
/// chart of their own, when the document has an overlay.
const PATCHED_DIR: &str = "patched";

/// Installs a compiled chart as a release.
pub struct HelmRunner {
    pub helm: String,
    /// Arguments added to every `helm` call, after Meridian's own.
    pub helm_args: Vec<String>,
    /// The command that builds an overlay: the program, then its leading
    /// arguments. The overlay's directory is added as the last one.
    pub kustomize: Vec<String>,
    pub kubectl: String,
    /// Arguments added to every `kubectl` call, after Meridian's own.
    pub kubectl_args: Vec<String>,
}

/// The object a resource of the document became.
struct Target {
    api_version: String,
    kind: String,
    name: String,
    namespace: String,
}

enum Part {
    Text(String),
    Lookup { resource: String, path: Vec<Step> },
}

struct OutputSpec {
    name: String,
    sensitive: bool,
    parts: Vec<Part>,
}

/// What `release.json` says about the installation.
struct Release {
    name: String,
    namespace: String,
    create_namespace: bool,
    wait: bool,
    atomic: bool,
    timeout: String,
    chart_name: String,
    chart_version: String,
    kustomize: bool,
    /// By the id the author wrote.
    resources: BTreeMap<String, Target>,
    outputs: Vec<OutputSpec>,
}

impl Release {
    fn read(text: &str) -> Result<Release, RunError> {
        Release::parse(text)
            .ok_or_else(|| RunError::new(format!("{RELEASE_PATH} is not one this plugin compiled")))
    }

    fn parse(text: &str) -> Option<Release> {
        let value = decode_value(text.as_bytes()).ok()?;
        let map = value.as_map()?;
        let string = |map: &BTreeMap<String, Value>, key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let flag = |key: &str| matches!(map.get(key), Some(Value::Bool(true)));
        let chart = map.get("chart")?.as_map()?;

        let mut resources = BTreeMap::new();
        for (id, target) in map.get("resources")?.as_map()? {
            let target = target.as_map()?;
            resources.insert(
                id.clone(),
                Target {
                    api_version: string(target, "apiVersion"),
                    kind: string(target, "kind"),
                    name: string(target, "name"),
                    namespace: string(target, "namespace"),
                },
            );
        }
        let mut outputs = Vec::new();
        for output in map.get("outputs")?.as_list()? {
            let output = output.as_map()?;
            let mut parts = Vec::new();
            for part in output.get("parts")?.as_list()? {
                let part = part.as_map()?;
                parts.push(match part.get("text") {
                    Some(text) => Part::Text(text.as_str()?.to_owned()),
                    None => Part::Lookup {
                        resource: string(part, "resource"),
                        path: path::from_value(part.get("path")?)?,
                    },
                });
            }
            outputs.push(OutputSpec {
                name: string(output, "name"),
                sensitive: matches!(output.get("sensitive"), Some(Value::Bool(true))),
                parts,
            });
        }

        let release = Release {
            name: string(map, "release"),
            namespace: string(map, "namespace"),
            create_namespace: flag("createNamespace"),
            wait: flag("wait"),
            atomic: flag("atomic"),
            timeout: string(map, "timeout"),
            chart_name: string(chart, "name"),
            chart_version: string(chart, "version"),
            kustomize: flag("kustomize"),
            resources,
            outputs,
        };
        (!release.name.is_empty() && !release.namespace.is_empty()).then_some(release)
    }

    /// The id of the resource an object was declared by, or the tool's own
    /// name for an object no resource accounts for.
    /// The id of an object Helm named by kind and name, or Helm's name for
    /// it when the document has no such object. With no name, the one
    /// object of that kind.
    fn resource_named(&self, kind: &str, name: &str) -> String {
        let mut matching = self
            .resources
            .iter()
            .filter(|(_, t)| same_kind(kind, &t.kind) && (name.is_empty() || t.name == name));
        match (matching.next(), matching.next()) {
            (Some((id, _)), None) => id.clone(),
            _ if name.is_empty() => kind.to_owned(),
            _ => format!("{kind}/{name}"),
        }
    }

    /// `helm` run to its end, its failure carrying the objects it named.
    fn run_ok(&self, tool: Tool, what: &str, log: &dyn Log) -> Result<(), RunError> {
        let finished = tool.run(log)?;
        if finished.success() {
            return Ok(());
        }
        let failures = read_errors(&finished.stderr)
            .into_iter()
            .map(|found| {
                let mut failure = found.failure;
                failure.resource = if found.kind.is_empty() {
                    String::new()
                } else {
                    self.resource_named(&found.kind, &found.name)
                };
                failure
            })
            .collect();
        let mut err = finished.into_error(what);
        err.failures = failures;
        Err(err)
    }

    fn resource_of(&self, key: &Key) -> String {
        let mut candidates = self.resources.iter().filter(|(_, t)| {
            t.api_version == key.api_version && t.kind == key.kind && t.name == key.name
        });
        let found = match (candidates.next(), candidates.next()) {
            (Some((id, _)), None) => Some(id),
            // The same object in two namespaces: the namespace decides.
            (Some(first), Some(second)) => [first, second]
                .into_iter()
                .chain(candidates)
                .find(|(_, t)| t.namespace == key.namespace)
                .map(|(id, _)| id),
            _ => None,
        };
        found
            .cloned()
            .unwrap_or_else(|| format!("{}/{}", key.kind, key.name))
    }
}

/// A chart rendered to the objects an apply would install.
struct Rendered {
    release: Release,
    manifests: String,
    /// The chart to install, relative to the work directory: the compiled
    /// one, or the patched objects when there is an overlay.
    chart: &'static str,
}

/// A rendered chart compared with the release in the cluster.
struct Preview {
    rendered: Rendered,
    changes: Vec<Change>,
    /// The release's status, or `None` when there is no such release.
    status: Option<String>,
}

impl Preview {
    /// Whether the release is installed, healthy and already what the chart
    /// describes.
    fn in_sync(&self) -> bool {
        self.changes.is_empty() && self.status.as_deref() == Some("deployed")
    }
}

impl HelmRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        HelmRunner {
            helm: settings::text(setting("helm")),
            helm_args: settings::args(setting("helm-args")),
            kustomize: settings::args(setting("kustomize")),
            kubectl: settings::text(setting("kubectl")),
            kubectl_args: settings::args(setting("kubectl-args")),
        }
    }

    /// One `helm` call about the release.
    fn helm(&self, context: &RunContext, release: &Release, operation: &[&str]) -> Tool {
        Tool::new(&self.helm)
            .args(operation.iter().copied())
            .args(["--namespace", &release.namespace])
            .args(self.helm_args.iter().cloned())
            .debug(meridian_sdk::debug::pick(DEBUG, context.debug_level))
            .dir(&context.work_dir)
    }

    /// Renders the artifact to the manifests an apply would install, without
    /// reading or changing a cluster: Helm renders the chart, and Kustomize
    /// patches the result when the document has an overlay.
    pub fn render(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<String, RunError> {
        Ok(self.rendered(context, artifact, log)?.manifests)
    }

    fn rendered(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Rendered, RunError> {
        let chart_file = format!("{CHART_DIR}/Chart.yaml");
        let (Some(_), Some(settings)) = (
            artifact.files.get(&chart_file),
            artifact.files.get(RELEASE_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {chart_file} and {RELEASE_PATH}; it was not compiled by this plugin"
            )));
        };
        let release = Release::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;

        log.out(&format!("release {}: rendering the chart", release.name));
        let template = self.helm(context, &release, &["template", &release.name, CHART_DIR]);
        let templated = with_values(template, context, artifact, CHART_DIR)?
            .capture_stdout()
            .run_ok("helm could not render the chart", log)?
            .stdout;
        if !release.kustomize {
            return Ok(Rendered {
                release,
                manifests: templated,
                chart: CHART_DIR,
            });
        }

        // Kustomize only reads files under the overlay's own directory.
        let write = |path: std::path::PathBuf, text: &str| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    RunError::new(format!("could not create {}: {e}", parent.display()))
                })?;
            }
            fs::write(&path, text)
                .map_err(|e| RunError::new(format!("could not write {}: {e}", path.display())))
        };
        write(
            context.work_dir.join(KUSTOMIZE_DIR).join(RENDERED_FILE),
            &templated,
        )?;
        log.out(&format!(
            "release {}: applying the Kustomize overlay",
            release.name
        ));
        let (program, leading) = self.kustomize.split_first().ok_or_else(|| {
            RunError::new("no command is configured to build a Kustomize overlay")
        })?;
        let patched = Tool::new(program)
            .args(leading.iter().cloned())
            .arg(KUSTOMIZE_DIR)
            .dir(&context.work_dir)
            .capture_stdout()
            .run_ok("kustomize could not apply the overlay", log)?
            .stdout;

        // The patched objects become a chart of their own, so Helm installs
        // exactly them. They are final: nothing in them is a template, and a
        // `{{` is escaped to stay one.
        let dir = context.work_dir.join(PATCHED_DIR);
        let _ = fs::remove_dir_all(&dir);
        let about = Yaml::Map(vec![
            ("apiVersion".to_owned(), Yaml::str("v2")),
            ("name".to_owned(), Yaml::str(&release.chart_name)),
            (
                "description".to_owned(),
                Yaml::str("What the compiled chart renders, patched by its Kustomize overlay."),
            ),
            ("type".to_owned(), Yaml::str("application")),
            ("version".to_owned(), Yaml::str(&release.chart_version)),
        ]);
        write(dir.join("Chart.yaml"), &meridian_yaml::write(&about))?;
        write(
            dir.join("templates/manifests.yaml"),
            &template::literal(&patched),
        )?;
        Ok(Rendered {
            release,
            manifests: patched,
            chart: PATCHED_DIR,
        })
    }

    /// The release's status as Helm names it, or `None` when it has never
    /// been installed.
    fn status(&self, context: &RunContext, release: &Release) -> Result<Option<String>, RunError> {
        let finished = self
            .helm(
                context,
                release,
                &["status", &release.name, "--output", "json"],
            )
            .capture_stdout()
            .run(&NoLog)?;
        if !finished.success() {
            if finished.stderr.contains("release: not found") {
                return Ok(None);
            }
            return Err(finished.into_error("helm could not read the release"));
        }
        let described = decode_value(finished.stdout.trim().as_bytes())
            .map_err(|e| RunError::new(format!("the release's status could not be read: {e}")))?;
        let status = described
            .as_map()
            .and_then(|m| m.get("info"))
            .and_then(Value::as_map)
            .and_then(|info| info.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        Ok(Some(status.to_owned()))
    }

    fn preview(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Preview, RunError> {
        let rendered = self.rendered(context, artifact, log)?;
        let release = &rendered.release;
        let status = self.status(context, release)?;
        let installed = match status {
            None => String::new(),
            Some(_) => {
                self.helm(context, release, &["get", "manifest", &release.name])
                    .capture_stdout()
                    .run_ok("helm could not read what the release holds", &NoLog)?
                    .stdout
            }
        };
        let changes = changes_between(release, &installed, &rendered.manifests);
        Ok(Preview {
            rendered,
            changes,
            status,
        })
    }

    /// The release an artifact installs, with the artifact written out.
    fn release_of(context: &RunContext, artifact: &Artifact) -> Result<Release, RunError> {
        let Some(settings) = artifact.files.get(RELEASE_PATH) else {
            return Err(RunError::new(format!(
                "the artifact has no {RELEASE_PATH}; it was not compiled by this plugin"
            )));
        };
        let release = Release::read(settings)?;
        write_artifact(&context.work_dir, artifact)?;
        Ok(release)
    }

    /// What uninstalling the release would remove: every object of the
    /// manifest Helm last installed. `None` when it is not installed.
    fn removals(
        &self,
        context: &RunContext,
        release: &Release,
    ) -> Result<Option<Vec<Change>>, RunError> {
        if self.status(context, release)?.is_none() {
            return Ok(None);
        }
        let installed = self
            .helm(context, release, &["get", "manifest", &release.name])
            .capture_stdout()
            .run_ok("helm could not read what the release holds", &NoLog)?
            .stdout;
        Ok(Some(changes_between(release, &installed, "")))
    }

    /// Reads one object from the cluster.
    fn object(
        &self,
        context: &RunContext,
        release: &Release,
        target: &Target,
    ) -> Result<Value, RunError> {
        // A kind alone can name two things once a cluster has custom
        // resources, so a grouped kind is asked for in full.
        let resource = match target.api_version.split_once('/') {
            Some((group, version)) => format!("{}.{version}.{group}", target.kind),
            None => target.kind.clone(),
        };
        let namespace = if target.namespace.is_empty() {
            &release.namespace
        } else {
            &target.namespace
        };
        let finished = Tool::new(&self.kubectl)
            .args(["get", &resource, &target.name])
            .args(["--namespace", namespace, "--output", "json"])
            .args(self.kubectl_args.iter().cloned())
            .dir(&context.work_dir)
            .capture_stdout()
            .run_ok(
                &format!("kubectl could not read the {} {}", target.kind, target.name),
                &NoLog,
            )?;
        decode_value(finished.stdout.trim().as_bytes()).map_err(|e| {
            RunError::new(format!(
                "the {} {} could not be read: {e}",
                target.kind, target.name
            ))
        })
    }

    fn read_outputs(
        &self,
        context: &RunContext,
        release: &Release,
    ) -> Result<Vec<Output>, RunError> {
        let mut objects: BTreeMap<&str, Value> = BTreeMap::new();
        let mut outputs = Vec::with_capacity(release.outputs.len());
        for spec in &release.outputs {
            let mut values = Vec::with_capacity(spec.parts.len());
            for part in &spec.parts {
                let (resource, path) = match part {
                    Part::Text(text) => {
                        values.push(Value::String(text.clone()));
                        continue;
                    }
                    Part::Lookup { resource, path } => (resource, path),
                };
                let target = release.resources.get(resource).ok_or_else(|| {
                    RunError::new(format!(
                        "output {:?} reads the resource {resource:?}, which {RELEASE_PATH} does not list",
                        spec.name
                    ))
                })?;
                if !objects.contains_key(resource.as_str()) {
                    objects.insert(resource, self.object(context, release, target)?);
                }
                let found = path::lookup(&objects[resource.as_str()], path).map_err(|missing| {
                    RunError::new(format!(
                        "output {:?}: the {} {} has no {missing} yet. A value the cluster fills \
                         in later, such as a load balancer's address, is there once the release \
                         is ready; set wait: true on the release to have Helm wait for that",
                        spec.name, target.kind, target.name
                    ))
                })?;
                values.push(found.clone());
            }
            // One reference alone keeps its type; anything longer is text.
            let value = match (spec.parts.as_slice(), values.as_slice()) {
                ([Part::Lookup { .. }], [value]) => value.clone(),
                _ => Value::String(
                    values
                        .iter()
                        .map(|v| template::as_text(v).unwrap_or_else(|| v.canonical()))
                        .collect(),
                ),
            };
            outputs.push(Output {
                name: spec.name.clone(),
                value,
                sensitive: spec.sensitive,
            });
        }
        Ok(outputs)
    }
}

impl Runner for HelmRunner {
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
        let preview = self.preview(context, artifact, log)?;
        // A release that is there but not healthy is upgraded again by an
        // apply even when its objects are the ones the chart describes, and
        // what that will change in the cluster is not something the manifests
        // show.
        let unknown = preview.changes.is_empty() && !preview.in_sync();
        if let (true, Some(status)) = (unknown, &preview.status) {
            log.out(&format!(
                "release {}: its status is {status}, so an apply will upgrade it again",
                preview.rendered.release.name
            ));
        }
        Ok(Plan {
            changes: preview.changes,
            unknown,
            saved: None,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let preview = self.preview(context, artifact, log)?;
        let release = &preview.rendered.release;
        if preview.in_sync() {
            log.out(&format!("release {}: nothing to change", release.name));
        } else {
            log.out(&format!(
                "release {}: {}",
                release.name,
                if preview.status.is_none() {
                    "installing"
                } else {
                    "upgrading"
                }
            ));
            let mut upgrade = self.helm(
                context,
                release,
                &[
                    "upgrade",
                    "--install",
                    &release.name,
                    preview.rendered.chart,
                ],
            );
            if release.create_namespace {
                upgrade = upgrade.arg("--create-namespace");
            }
            if release.wait {
                upgrade = upgrade.arg("--wait");
            }
            if release.atomic {
                upgrade = upgrade.arg("--atomic");
            }
            if !release.timeout.is_empty() {
                upgrade = upgrade.args(["--timeout", &release.timeout]);
            }
            let upgrade = with_values(upgrade, context, artifact, CHART_DIR)?;
            release.run_ok(
                upgrade,
                "helm could not install the release; `helm status` and `helm history` say what \
                 state it was left in",
                log,
            )?;
        }
        Ok(Applied {
            outputs: self.read_outputs(context, release)?,
            changes: preview.changes,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let settings = fs::read_to_string(context.work_dir.join(RELEASE_PATH)).map_err(|_| {
            RunError::new("nothing has been applied for this document yet, so it has no outputs")
        })?;
        self.read_outputs(context, &Release::read(&settings)?)
    }

    fn destroys(&self) -> bool {
        true
    }

    fn plan_destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        _log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let release = Self::release_of(context, artifact)?;
        Ok(Plan {
            changes: self.removals(context, &release)?.unwrap_or_default(),
            unknown: false,
            saved: None,
        })
    }

    /// `helm uninstall`, waiting until the release's objects are gone. A
    /// release that is not installed has nothing to remove.
    fn destroy(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Vec<Change>, RunError> {
        let release = Self::release_of(context, artifact)?;
        let Some(removals) = self.removals(context, &release)? else {
            log.out(&format!("release {}: not installed", release.name));
            return Ok(Vec::new());
        };
        log.out(&format!("release {}: uninstalling", release.name));
        let mut uninstall = self.helm(context, &release, &["uninstall", &release.name, "--wait"]);
        if !release.timeout.is_empty() {
            uninstall = uninstall.args(["--timeout", &release.timeout]);
        }
        release.run_ok(uninstall, "helm could not uninstall the release", log)?;
        Ok(removals)
    }
}

/// The changes that take the objects of one manifest to those of another,
/// by the ids the author wrote.
fn changes_between(release: &Release, old: &str, new: &str) -> Vec<Change> {
    manifest::diff(&manifest::objects(old), &manifest::objects(new))
        .into_iter()
        .map(|(action, key)| Change {
            resource: release.resource_of(&key),
            action,
            summary: if key.namespace.is_empty() {
                format!("{} {}", key.kind, key.name)
            } else {
                format!("{} {} in namespace {}", key.kind, key.name, key.namespace)
            },
        })
        .collect()
}
