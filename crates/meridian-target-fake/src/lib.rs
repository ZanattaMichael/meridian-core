//! A small, complete plugin, for tests and as a worked example.
//!
//! Its "tool" is a directory. Compiling turns each `file` resource into one
//! artifact file; applying copies the artifact into the work directory and
//! remembers what it wrote, so a plan is a real comparison against real
//! state. That is enough to exercise every part of the protocol without
//! installing anything.
//!
//! A few magic words in a file's content make it misbehave on purpose:
//! `FAIL` makes the tool fail at that file, `PANIC` panics the runner, and
//! `EXIT` kills the whole plugin process mid-run.
//!
//! It imports as well. Its tool's "existing configuration" is a `*.fake`
//! file of `name = content` lines, each of which becomes a `file` resource.
//! A line that starts with `?` is something the importer does not
//! understand, which it reports instead of dropping, and a line that is only
//! `BROKEN` is a syntax error.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use meridian_sdk::{
    canonical_map, decode_map, serve, Applied, Artifact, Capabilities, Category, Change,
    ChangeAction, CompileError, Data, DocumentKind, Emitted, Emitter, ImportError, ImportFormat,
    ImportNote, ImportSource, Imported, ImportedDocument, ImportedResource, Importer, Log,
    NoteLevel, Operations, Output, Plan, Plugin, ResourceGraph, RunContext, RunError, Runner,
    ServeError, Value, Warning,
};

/// The compiling half: each `file` resource becomes one artifact file.
pub struct Fake;

impl Emitter for Fake {
    fn name(&self) -> &str {
        "fake"
    }

    fn supported_resource_types(&self) -> Vec<String> {
        vec!["file".to_owned()]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
        }
    }

    // The fake stands in for a provisioning tool in one stage of a test and
    // for a configuration tool in the next. Containerization is the one
    // category that takes both kinds of document, so that is what it states.
    fn category(&self) -> Category {
        Category::Containerization
    }

    fn target_versions(&self) -> Vec<String> {
        vec!["1".to_owned(), "2.4".to_owned()]
    }

    fn emit(&self, graph: &ResourceGraph, data: &Data) -> Result<Emitted, CompileError> {
        let mut emitted = Emitted::default();
        for resource in &graph.resources {
            if resource.r#type != "file" {
                return Err(CompileError::for_resource(
                    resource,
                    "unsupported-resource-type",
                    format!(
                        "the fake target has no mapping for type {:?}",
                        resource.r#type
                    ),
                ));
            }
            let content = match resource.params.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.canonical(),
                None => String::new(),
            };
            if resource.state == "absent" {
                emitted.warnings.push(Warning {
                    target: String::new(),
                    resource: resource.id.clone(),
                    msg: "an absent file is simply left out".to_owned(),
                });
                continue;
            }
            emitted
                .artifact
                .files
                .insert(format!("files/{}", resource.id), content);
        }
        // A declared output is compiled into the artifact, as a real target
        // compiles one into its tool's own output block.
        for output in &graph.outputs {
            emitted
                .artifact
                .files
                .insert(format!("outputs/{}", output.name), output.value.clone());
        }
        // The resolved data is echoed so a test can see it arrive intact.
        if !data.is_empty() {
            emitted
                .artifact
                .files
                .insert("data.json".to_owned(), canonical_map(data));
        }
        Ok(emitted)
    }
}

/// The running half. Its state is `state.json` in the work directory.
pub struct FakeRunner {
    pub operations: Operations,
    /// Whether the tool takes inputs, which it then echoes when it applies.
    pub accepts_inputs: bool,
}

/// What the last apply wrote, by artifact path.
fn read_state(work_dir: &Path) -> Result<BTreeMap<String, Value>, RunError> {
    match fs::read(work_dir.join("state.json")) {
        Ok(bytes) => decode_map(&bytes).map_err(RunError::new),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(RunError::new(format!("reading the state file: {e}"))),
    }
}

fn diff(state: &BTreeMap<String, Value>, artifact: &Artifact) -> Vec<Change> {
    let mut changes = Vec::new();
    for (path, content) in &artifact.files {
        let action = match state.get(path) {
            None => ChangeAction::Create,
            Some(Value::String(old)) if old == content => continue,
            Some(_) => ChangeAction::Update,
        };
        changes.push(Change {
            resource: path.clone(),
            action,
            summary: format!("{} bytes", content.len()),
        });
    }
    for path in state.keys() {
        if !artifact.files.contains_key(path) {
            changes.push(Change {
                resource: path.clone(),
                action: ChangeAction::Delete,
                summary: String::new(),
            });
        }
    }
    changes
}

fn outputs_of(state: &BTreeMap<String, Value>, context: &RunContext) -> Vec<Output> {
    let mut outputs = vec![
        Output {
            name: "file_count".to_owned(),
            value: Value::Int(state.len() as i64),
            sensitive: false,
        },
        Output {
            name: "token".to_owned(),
            value: Value::from("s3cret-for-the-next-stage"),
            sensitive: true,
        },
    ];
    // Declared outputs. `${id.content}` is the content of that file, which is
    // this tool's one attribute; anything else is taken as written. A name
    // starting `secret_` is reported as sensitive.
    for (path, expression) in state {
        let Some(name) = path.strip_prefix("outputs/") else {
            continue;
        };
        let Value::String(expression) = expression else {
            continue;
        };
        let value = expression
            .strip_prefix("${")
            .and_then(|e| e.strip_suffix(".content}"))
            .and_then(|id| state.get(&format!("files/{id}")))
            .cloned()
            .unwrap_or_else(|| Value::from(expression.as_str()));
        outputs.push(Output {
            name: name.to_owned(),
            value,
            sensitive: name.starts_with("secret_"),
        });
    }
    if let Some(greeting) = context.inputs.get("greeting") {
        outputs.push(Output {
            name: "greeting".to_owned(),
            value: greeting.clone(),
            sensitive: false,
        });
    }
    outputs
}

impl Runner for FakeRunner {
    fn operations(&self) -> Operations {
        self.operations
    }

    fn accepts_inputs(&self) -> bool {
        self.accepts_inputs
    }

    fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Plan, RunError> {
        let state = read_state(&context.work_dir)?;
        log.out(&format!(
            "comparing {} files for {} on {}",
            artifact.files.len(),
            context.document,
            context.host
        ));
        let changes = diff(&state, artifact);
        log.err(&format!("{} to change", changes.len()));
        Ok(Plan {
            changes,
            unknown: artifact.files.values().any(|c| c.contains("UNKNOWN")),
        })
    }

    fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &dyn Log,
    ) -> Result<Applied, RunError> {
        let mut state = read_state(&context.work_dir)?;
        // Tools echo their variables more often than anyone would like. This
        // one does it on purpose, so a host's masking has something to catch.
        if !context.inputs.is_empty() {
            log.err(&format!("inputs: {}", canonical_map(&context.inputs)));
        }
        let planned = diff(&state, artifact);
        let mut done = Vec::new();
        let save = |state: &BTreeMap<String, Value>| {
            fs::write(context.work_dir.join("state.json"), canonical_map(state))
                .map_err(|e| RunError::new(format!("writing the state file: {e}")))
        };

        for change in planned {
            let path = context.work_dir.join("current").join(&change.resource);
            log.out(&format!("{} {}", change.action, change.resource));
            if change.action == ChangeAction::Delete {
                let _ = fs::remove_file(&path);
                state.remove(&change.resource);
                done.push(change);
                continue;
            }
            let content = &artifact.files[&change.resource];
            if content.contains("PANIC") {
                panic!("the fake runner was told to panic");
            }
            if content.contains("EXIT") {
                eprintln!("the fake plugin was told to die");
                std::process::exit(7);
            }
            if content.contains("FAIL") {
                save(&state)?;
                return Err(RunError {
                    msg: format!("could not write {}", change.resource),
                    command: "fake-tool apply --password=***".to_owned(),
                    exit_code: Some(3),
                    output: "disk on fire\n".to_owned(),
                    changes: done,
                });
            }
            let written = path
                .parent()
                .map(fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|()| fs::write(&path, content));
            if let Err(e) = written {
                save(&state)?;
                return Err(RunError {
                    msg: format!("writing {}: {e}", path.display()),
                    changes: done,
                    ..RunError::default()
                });
            }
            state.insert(change.resource.clone(), Value::from(content.as_str()));
            done.push(change);
        }
        save(&state)?;
        Ok(Applied {
            changes: done,
            outputs: outputs_of(&state, context),
        })
    }

    fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunError> {
        let state = read_state(&context.work_dir)?;
        if state.is_empty() {
            return Err(RunError::new("nothing has been applied yet"));
        }
        Ok(outputs_of(&state, context))
    }
}

/// The importing half: each `name = content` line of a `*.fake` file becomes
/// a `file` resource.
pub struct FakeImporter;

impl Importer for FakeImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        vec![ImportFormat::new("fake", "Fake files", &["*.fake"])]
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        let name = if source.name.is_empty() {
            "imported".to_owned()
        } else {
            source.name.clone()
        };
        let mut resources = Vec::new();
        let mut notes = Vec::new();
        for (file, text) in &source.files {
            for (index, line) in text.lines().enumerate() {
                let origin = format!("{file}:{}", index + 1);
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if line == "BROKEN" {
                    return Err(ImportError::at(file, index + 1, "this line is broken"));
                }
                if let Some(what) = line.strip_prefix('?') {
                    notes.push(ImportNote {
                        level: NoteLevel::Untranslated,
                        origin,
                        subject: what.trim().to_owned(),
                        msg: "the fake tool has nothing like this, so it was left out".to_owned(),
                        document: name.clone(),
                        resource: String::new(),
                    });
                    continue;
                }
                let Some((id, content)) = line.split_once('=') else {
                    return Err(ImportError::at(
                        file,
                        index + 1,
                        "expected `name = content`",
                    ));
                };
                resources.push(ImportedResource {
                    id: id.trim().to_owned(),
                    r#type: "file".to_owned(),
                    params: BTreeMap::from([(
                        "content".to_owned(),
                        Value::String(content.trim().to_owned()),
                    )]),
                    origin,
                    ..ImportedResource::default()
                });
            }
        }
        Ok(Imported {
            documents: vec![ImportedDocument {
                kind: DocumentKind::Infrastructure,
                name,
                resources,
                ..ImportedDocument::default()
            }],
            notes,
        })
    }
}

/// Serves the fake plugin, offering the operations `FAKE_OPERATIONS` names
/// (`none`, `plan`, or anything else for all of them), so a host can be
/// checked against a plugin that offers only some. `FAKE_INPUTS=none` makes
/// it a plugin that does not take inputs.
pub fn serve_from_env() -> Result<(), ServeError> {
    let operations = match std::env::var("FAKE_OPERATIONS").as_deref() {
        Ok("none") => None,
        Ok("plan") => Some(Operations {
            plan: true,
            apply: false,
            outputs: false,
        }),
        _ => Some(Operations::ALL),
    };
    let mut plugin = Plugin::new(Fake);
    if std::env::var("FAKE_IMPORT").as_deref() != Ok("none") {
        plugin = plugin.with_importer(FakeImporter);
    }
    match operations {
        Some(operations) => serve(plugin.with_runner(FakeRunner {
            operations,
            accepts_inputs: std::env::var("FAKE_INPUTS").as_deref() != Ok("none"),
        })),
        None => serve(plugin),
    }
}

/// Builds the plugin binary and returns the directory holding it, for a test
/// that needs the real process. The build is a no-op when it is up to date.
///
/// A test cannot ask Cargo for another package's binary through an
/// environment variable, so it is built here, and Cargo is asked where it put
/// the result: the layout of a target directory is Cargo's business and has
/// changed between releases.
pub fn plugin_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let name = "meridian-target-fake";
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = Command::new(cargo);
        build.args([
            "build",
            "--locked",
            "--message-format=json",
            "-p",
            name,
            "--bin",
            name,
        ]);
        // Built the way the test itself was, so a release test run does not
        // also compile a debug plugin.
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        let output = build
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("cargo runs");
        assert!(
            output.status.success(),
            "building the fake plugin:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let messages = String::from_utf8_lossy(&output.stdout);
        let built = messages
            .lines()
            .filter(|line| line.contains("\"reason\":\"compiler-artifact\""))
            .filter_map(executable_in)
            .rfind(|path| path.file_name().is_some_and(|n| n == name))
            .unwrap_or_else(|| panic!("cargo did not report where it built {name}:\n{messages}"));

        // Discovery loads everything named like a plugin, so the binary gets
        // a directory to itself.
        let dir = built
            .parent()
            .expect("the binary is in a directory")
            .join("fake-plugin");
        std::fs::create_dir_all(&dir).expect("creating the plugin directory");
        // Copied under another name and renamed, so a test process running in
        // parallel never executes a half-written file.
        let staged = dir.join(format!("{name}.{}", std::process::id()));
        std::fs::copy(&built, &staged).expect("copying the plugin binary");
        std::fs::rename(&staged, dir.join(name)).expect("placing the plugin binary");
        dir
    })
    .clone()
}

/// The `executable` path in one of Cargo's JSON build messages.
fn executable_in(message: &str) -> Option<PathBuf> {
    let marker = "\"executable\":\"";
    let start = message.find(marker)? + marker.len();
    let rest = &message[start..];
    let mut path = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(PathBuf::from(path)),
            // JSON escapes a backslash or a quote in a path with a backslash.
            '\\' => path.push(chars.next()?),
            c => path.push(c),
        }
    }
    None
}

/// The plugin binary itself.
pub fn plugin_path() -> PathBuf {
    plugin_dir().join("meridian-target-fake")
}
