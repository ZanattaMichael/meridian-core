//! The half that hands a template to `packer`. It sits outside the compile
//! chain: nothing here is reached by a compile.

use std::fs;
use std::path::Path;

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{
    cloud_for, decode_value, Applied, Artifact, Change, ChangeAction, CloudHandover, Log,
    Operations, Output, Plan, RunContext, RunError, Runner, SecretFile, Value,
};
use meridian_sdk::{settings, Setting};

use crate::outputs::{self, Built};
use crate::{BUILD_PATH, CONFIG_PATH, MANIFEST_PATH};

/// Names the program to run, when it is not `packer` on `PATH`.
pub const PROGRAM_VARIABLE: &str = "MERIDIAN_PACKER";
/// Extra arguments for `packer validate` and `packer build`, split like a
/// shell command line: `-only=amazon-ebs.web`, `-parallel-builds=1`.
pub const ARGS_VARIABLE: &str = "MERIDIAN_PACKER_ARGS";

/// What an operator can configure about this runner, in the order the
/// settings are documented.
pub fn settings() -> Vec<Setting> {
    vec![
        Setting::new(
            "program",
            PROGRAM_VARIABLE,
            "packer",
            "The `packer` program to run, when it is not on `PATH`.",
        ),
        Setting::new(
            "args",
            ARGS_VARIABLE,
            "",
            "Extra arguments for `packer validate` and `packer build`, split like a shell command line: `-only=amazon-ebs.web`, `-parallel-builds=1`.",
        ),
    ]
}

/// Runs a compiled template.
///
/// The work directory is Packer's working directory: the template is written
/// there, and Packer's manifest of what it built is kept beside it.
pub struct PackerRunner {
    /// The program to run.
    pub program: String,
    /// Arguments added to `validate` and `build`, after Packer's own.
    pub extra_args: Vec<String>,
}

/// What the compile recorded about the build, read back from the artifact or
/// from the work directory.
#[derive(Debug, Default, PartialEq)]
struct Described {
    /// Each source's id and builder type, in the document's order.
    sources: Vec<(String, String)>,
    /// Each output's name and its text.
    outputs: Vec<(String, String)>,
}

impl PackerRunner {
    /// The runner as the environment configures it.
    pub fn from_env() -> Self {
        let declared = settings();
        let setting = |name: &str| settings::find(&declared, name);
        PackerRunner {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
        }
    }

    fn tool(&self, context: &RunContext, cloud: Option<&CloudHandover>) -> Tool {
        let mut tool = Tool::new(&self.program)
            .dir(&context.work_dir)
            .env("PACKER_NO_COLOR", "1")
            .env("CHECKPOINT_DISABLE", "1");
        // Values handed over from earlier stages reach the template as input
        // variables, for a document that declares them. Packer ignores one it
        // has no variable for.
        for (name, value) in &context.inputs {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.canonical(),
            };
            tool = tool.env(format!("PKR_VAR_{name}"), text);
        }
        CloudHandover::tell_if_any(cloud, tool)
    }

    /// The secrets of the run as a variables file only the user can read,
    /// held in memory where the system allows, for `-var-file`; nothing when
    /// the run has none. The command line names the file's path and nothing
    /// more, and the variables are declared sensitive, so Packer masks their
    /// values in everything it prints.
    fn secrets(context: &RunContext) -> Result<Option<SecretFile>, RunError> {
        if context.secrets.is_empty() {
            return Ok(None);
        }
        context
            .secrets
            .json_file("secrets.pkrvars.json", crate::secret_variable)
            .map(Some)
            .map_err(|e| RunError::new(format!("writing the variables file: {e}")))
    }

    /// A call of `command` that reads the template, given the variables file
    /// when there is one; the template's directory comes last.
    fn reading(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        command: &[&str],
        secrets: Option<&SecretFile>,
    ) -> Tool {
        let tool = self
            .tool(context, cloud)
            .args(command.iter().copied())
            .args(self.extra_args.iter().cloned());
        match secrets {
            Some(file) => tool.arg(format!("-var-file={}", file.path().display())),
            None => tool,
        }
        .arg(".")
    }

    /// Writes the template, installs the plugins it requires and has Packer
    /// check it. Nothing is built.
    fn prepare(
        &self,
        context: &RunContext,
        cloud: Option<&CloudHandover>,
        artifact: &Artifact,
        secrets: Option<&SecretFile>,
        log: &dyn Log,
    ) -> Result<Described, RunError> {
        let (Some(_), Some(described)) = (
            artifact.files.get(CONFIG_PATH),
            artifact.files.get(BUILD_PATH),
        ) else {
            return Err(RunError::new(format!(
                "the artifact has no {CONFIG_PATH} and {BUILD_PATH}; it was not compiled by \
                 this plugin"
            )));
        };
        let described = read_described(described)?;
        write_artifact(&context.work_dir, artifact)?;

        self.tool(context, cloud)
            .args(["init", "."])
            .run_ok("packer init failed", log)?;
        self.reading(context, cloud, &["validate"], secrets)
            .run_ok("packer validate failed", log)?;
        Ok(described)
    }
}

impl Runner for PackerRunner {
    fn settings(&self) -> Vec<Setting> {
        settings()
    }

    fn operations(&self) -> Operations {
        Operations::ALL
    }

    fn accepts_inputs(&self) -> bool {
        true
    }

    /// Packer has no plan, and does not need one to be honest: a build never
    /// changes an image that exists, it always makes a new one. So the
    /// preview is one creation per source, after Packer itself has checked
    /// the template.
    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        // A cloud credential reaches the tool in the environment of every
        // call of the stage; the files a provider reads live to the end of it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        let secrets = Self::secrets(context)?;
        let described = self.prepare(context, cloud, artifact, secrets.as_ref(), log)?;
        Ok(Plan {
            changes: described
                .sources
                .iter()
                .map(|(id, builder)| created(id, builder))
                .collect(),
            unknown: false,
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        // A cloud credential reaches the tool in the environment of every
        // call of the stage; the files a provider reads live to the end of it.
        let cloud = cloud_for(context.credential.as_ref(), &crate::CLOUD_TAKES)?;
        let cloud = cloud.as_ref();
        let secrets = Self::secrets(context)?;
        let described = self.prepare(context, cloud, artifact, secrets.as_ref(), log)?;
        let built = self
            .reading(context, cloud, &["build", "-color=false"], secrets.as_ref())
            .run_ok("packer build failed", log);
        drop(secrets);
        built?;
        let built = read_manifest(&context.work_dir)?;
        Ok(Applied {
            changes: described
                .sources
                .iter()
                .filter(|(id, _)| built.iter().any(|b| &b.name == id))
                .map(|(id, builder)| created(id, builder))
                .collect(),
            outputs: fill_outputs(&described, &built)?,
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let nothing =
            || RunError::new("nothing has been built for this document yet, so it has no outputs");
        let described =
            fs::read_to_string(context.work_dir.join(BUILD_PATH)).map_err(|_| nothing())?;
        if !context.work_dir.join(MANIFEST_PATH).exists() {
            return Err(nothing());
        }
        let described = read_described(&described)?;
        fill_outputs(&described, &read_manifest(&context.work_dir)?)
    }
}

fn created(id: &str, builder: &str) -> Change {
    Change {
        resource: id.to_owned(),
        action: ChangeAction::Create,
        summary: format!("{builder} image"),
    }
}

fn unreadable(what: &str, detail: impl std::fmt::Display) -> RunError {
    RunError::new(format!("{what} could not be read: {detail}"))
}

/// Reads what the compile recorded about the build.
fn read_described(json: &str) -> Result<Described, RunError> {
    let what = BUILD_PATH;
    let value = decode_value(json.as_bytes()).map_err(|e| unreadable(what, e))?;
    let Some(root) = value.as_map() else {
        return Err(unreadable(what, "it is not a JSON object"));
    };
    let mut described = Described::default();
    for source in root.get("sources").and_then(Value::as_list).unwrap_or(&[]) {
        let field = |key: &str| {
            source
                .as_map()
                .and_then(|s| s.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        described.sources.push((field("id"), field("type")));
    }
    if let Some(outputs) = root.get("outputs").and_then(Value::as_map) {
        for (name, template) in outputs {
            let Some(template) = template.as_str() else {
                return Err(unreadable(what, format!("output {name:?} is not text")));
            };
            described.outputs.push((name.clone(), template.to_owned()));
        }
    }
    Ok(described)
}

/// Reads what the last run built from the manifest in a work directory.
fn read_manifest(dir: &Path) -> Result<Vec<Built>, RunError> {
    let path = dir.join(MANIFEST_PATH);
    let json = fs::read(&path).map_err(|e| {
        RunError::new(format!(
            "packer did not leave the manifest {} that every build ends by writing: {e}",
            path.display()
        ))
    })?;
    parse_manifest(&json)
}

/// Reads a manifest into what its last run built.
///
/// Packer appends to a manifest, so it holds every run there has been. Only
/// the builds of the last run count: an image from an earlier run is not what
/// this document's outputs promise.
fn parse_manifest(json: &[u8]) -> Result<Vec<Built>, RunError> {
    let what = "packer's manifest";
    let value = decode_value(json).map_err(|e| unreadable(what, e))?;
    let Some(root) = value.as_map() else {
        return Err(unreadable(what, "it is not a JSON object"));
    };
    let last_run = root.get("last_run_uuid").and_then(Value::as_str);
    let mut built = Vec::new();
    for entry in root.get("builds").and_then(Value::as_list).unwrap_or(&[]) {
        let Some(fields) = entry.as_map() else {
            continue;
        };
        if last_run.is_some() && fields.get("packer_run_uuid").and_then(Value::as_str) != last_run {
            continue;
        }
        let name = fields.get("name").and_then(Value::as_str).unwrap_or("");
        built.push(Built {
            // A source is recorded under its own name. Should a release of
            // Packer ever qualify it, as it does on screen, the name is still
            // the last part.
            name: name.rsplit('.').next().unwrap_or(name).to_owned(),
            fields: fields.clone(),
        });
    }
    Ok(built)
}

fn fill_outputs(described: &Described, built: &[Built]) -> Result<Vec<Output>, RunError> {
    described
        .outputs
        .iter()
        .map(|(name, template)| {
            Ok(Output {
                name: name.clone(),
                value: outputs::fill(template, built)
                    .map_err(|msg| RunError::new(format!("output {name:?}: {msg}")))?,
                sensitive: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
      "builds": [
        {"name": "ubuntu", "builder_type": "amazon-ebs", "build_time": 1794000000,
         "files": null, "artifact_id": "us-east-1:ami-0old", "packer_run_uuid": "run-1",
         "custom_data": null},
        {"name": "ubuntu", "builder_type": "amazon-ebs", "build_time": 1794003600,
         "files": null, "artifact_id": "us-east-1:ami-0new", "packer_run_uuid": "run-2",
         "custom_data": null},
        {"name": "web-image.docker.app", "builder_type": "docker", "build_time": 1794003601,
         "files": null, "artifact_id": "sha256:9f86d081", "packer_run_uuid": "run-2",
         "custom_data": null}
      ],
      "last_run_uuid": "run-2"
    }"#;

    #[test]
    fn only_the_last_run_of_a_manifest_counts() {
        let built = parse_manifest(MANIFEST.as_bytes()).unwrap();
        let names: Vec<&str> = built.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["ubuntu", "app"]);
        assert_eq!(
            outputs::fill("${ubuntu.id}", &built).unwrap(),
            Value::String("ami-0new".into())
        );
        assert_eq!(
            outputs::fill("${app.id}", &built).unwrap(),
            Value::String("sha256:9f86d081".into())
        );
    }

    #[test]
    fn a_manifest_that_is_not_one_is_an_error_and_an_empty_one_built_nothing() {
        assert!(parse_manifest(b"not json").is_err());
        assert!(parse_manifest(b"[]").is_err());
        assert!(parse_manifest(b"{}").unwrap().is_empty());
    }

    #[test]
    fn the_build_description_is_read_back() {
        let described = read_described(
            r#"{"build":"web-image","outputs":{"image_id":"${ubuntu.id}"},
                "sources":[{"id":"ubuntu","type":"amazon-ebs"}]}"#,
        )
        .unwrap();
        assert_eq!(
            described,
            Described {
                sources: vec![("ubuntu".into(), "amazon-ebs".into())],
                outputs: vec![("image_id".into(), "${ubuntu.id}".into())],
            }
        );
        assert!(read_described("[]").is_err());
        assert!(read_described(r#"{"outputs":{"x":3}}"#).is_err());
    }
}
