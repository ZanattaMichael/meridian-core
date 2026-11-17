//! What the first-party plugins' tests share.
//!
//! A plugin's library depends on the SDK alone. Its tests want more: a
//! document read the way the core reads one, golden files, the real plugin
//! binary, and a stand-in for the tool so a runner can be tested on a machine
//! with nothing installed. Those live here, in a crate no plugin ships with.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use meridian_import::Rendered;
use meridian_ir::{Bundle, Document};
use meridian_sdk::{
    Artifact, Data, Emitted, Emitter, ImportSource, Imported, Importer, LogLine, Position,
    ResourceGraph, Value,
};

/// The repository root.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

/// Parses and validates a document held in a string, and builds the tree of
/// the one document in it, with `target` in place of whatever it names.
///
/// A resource set must give its host inline. `shown` is the file name
/// positions carry.
pub fn graph_from_text(shown: &str, text: &str, target: &str) -> ResourceGraph {
    let mut documents = meridian_ir::parse(shown, text.as_bytes())
        .unwrap_or_else(|e| panic!("parsing {shown}: {e}"));
    assert_eq!(documents.len(), 1, "{shown} should hold one document");
    match &mut documents[0] {
        Document::ResourceSet(rs) => rs.spec.target = target.to_owned(),
        Document::Infrastructure(infra) => infra.spec.target = target.to_owned(),
    }
    let bundle = Bundle::new(documents.clone());
    bundle
        .validate()
        .unwrap_or_else(|e| panic!("validating {shown}: {e}"));
    let graph = match &documents[0] {
        Document::ResourceSet(rs) => {
            let host = &rs
                .spec
                .target_host
                .as_ref()
                .expect("the document names its host inline")
                .host;
            meridian_ast::ResourceGraph::build(rs, host)
        }
        Document::Infrastructure(infra) => meridian_ast::ResourceGraph::build_infrastructure(infra),
    }
    .unwrap_or_else(|e| panic!("building {shown}: {e}"));
    meridian_plugin_host::to_sdk(&graph)
}

/// The tree of a document under the repository, by its path from the root.
pub fn graph_from_file(path: &str, target: &str) -> ResourceGraph {
    let full = repo_root().join(path);
    let text =
        fs::read_to_string(&full).unwrap_or_else(|e| panic!("reading {}: {e}", full.display()));
    graph_from_text(path, &text, target)
}

/// One of the shared fixtures in `fixtures/`, by name.
pub fn fixture(name: &str, target: &str) -> ResourceGraph {
    graph_from_file(&format!("fixtures/{name}.yaml"), target)
}

/// The same tree with every source position removed, as a tree built in
/// memory would be.
pub fn without_positions(mut graph: ResourceGraph) -> ResourceGraph {
    for resource in &mut graph.resources {
        resource.pos = Position::default();
    }
    for edge in &mut graph.edges {
        edge.pos = Position::default();
    }
    for output in &mut graph.outputs {
        output.pos = Position::default();
    }
    graph
}

/// The name a golden file has for one artifact path: `playbook.yml` of the
/// fixture `web` is `web.playbook_yml`.
pub fn golden_name(case: &str, artifact_path: &str) -> String {
    format!("{case}.{}", artifact_path.replace(['/', '.'], "_"))
}

/// Compares what a target compiled with the golden files recorded for a case:
/// one file per artifact path, plus `<case>.warnings` when there are any.
///
/// Run with `UPDATE_GOLDEN=1` to record them instead.
pub fn assert_golden(dir: &Path, case: &str, emitted: &Emitted) {
    let mut got: BTreeMap<String, String> = emitted
        .artifact
        .files
        .iter()
        .map(|(path, text)| (golden_name(case, path), text.clone()))
        .collect();
    if !emitted.warnings.is_empty() {
        got.insert(
            format!("{case}.warnings"),
            emitted.warnings.iter().map(|w| format!("{w}\n")).collect(),
        );
    }
    assert_golden_files(dir, case, &got);
}

/// Compares a refusal with `<case>.refused`.
pub fn assert_golden_refusal(dir: &Path, case: &str, refusal: &meridian_sdk::CompileError) {
    let got = BTreeMap::from([(format!("{case}.refused"), format!("{refusal}\n"))]);
    assert_golden_files(dir, case, &got);
}

fn assert_golden_files(dir: &Path, case: &str, got: &BTreeMap<String, String>) {
    let prefix = format!("{case}.");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(dir).expect("creating the golden directory");
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().into_string().unwrap_or_default();
                if name.starts_with(&prefix) && !got.contains_key(&name) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        for (name, text) in got {
            fs::write(dir.join(name), text).expect("recording a golden file");
        }
        return;
    }
    let want: BTreeMap<String, String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("listing {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(&prefix))
        .map(|name| {
            let text = fs::read_to_string(dir.join(&name)).unwrap();
            (name, text)
        })
        .collect();
    // The names first, so a missing or extra file reads as that and not as a
    // wall of differing text.
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>(),
        "{case}: the set of golden files differs"
    );
    for (name, text) in got {
        assert_eq!(text, &want[name], "{name}");
    }
}

/// Compares one piece of text with the golden file `<dir>/<name>`. Run with
/// `UPDATE_GOLDEN=1` to record it instead.
pub fn assert_golden_text(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(dir).expect("creating the golden directory");
        fs::write(&path, text).expect("recording a golden file");
        return;
    }
    let want =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert_eq!(text, want, "{name}");
}

/// The files directly inside a directory, as the source of an import named
/// `name`.
pub fn import_source(dir: &Path, name: &str) -> ImportSource {
    let mut files = BTreeMap::new();
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("listing {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_file() {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            files.insert(
                path.file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned(),
                text,
            );
        }
    }
    ImportSource {
        files,
        name: name.to_owned(),
        root: String::new(),
    }
}

/// An import source from path and content pairs.
pub fn import_text(name: &str, files: &[(&str, &str)]) -> ImportSource {
    ImportSource {
        files: files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect(),
        name: name.to_owned(),
        root: String::new(),
    }
}

/// One imported document, taken all the way round: as the importer returned
/// it, as the file `meridian import` would write, and as the target compiles
/// that file.
pub struct RoundTrip {
    pub rendered: Rendered,
    pub emitted: Emitted,
}

/// Imports a source, writes each document the way `meridian import` does,
/// reads it back the way the core reads any document, and compiles it with
/// the target.
///
/// That the written document parses, validates and compiles is itself half
/// of what a round-trip test asserts. The other half is comparing what was
/// compiled with the source, which only the test knows how to do.
pub fn round_trip(
    importer: &dyn Importer,
    emitter: &dyn Emitter,
    source: &ImportSource,
) -> (Imported, Vec<RoundTrip>) {
    let mut imported = importer
        .import(source)
        .unwrap_or_else(|e| panic!("importing: {e}"));
    for document in &mut imported.documents {
        if document.target.is_empty() {
            document.target = emitter.name().to_owned();
        }
    }
    let trips = meridian_import::render_all(&imported, "the test source")
        .into_iter()
        .map(|rendered| {
            let graph = graph_from_text(&rendered.file_name, &rendered.text, emitter.name());
            let emitted = emitter.emit(&graph, &Data::new()).unwrap_or_else(|e| {
                panic!(
                    "the imported document {} does not compile: {e}\n{}",
                    rendered.file_name, rendered.text
                )
            });
            RoundTrip { rendered, emitted }
        })
        .collect();
    (imported, trips)
}

/// Reads JSON text as a value, for comparing with what a target compiled.
pub fn json(text: &str) -> Value {
    meridian_sdk::decode_value(text.as_bytes()).unwrap_or_else(|e| panic!("reading JSON: {e}"))
}

/// An importer's notes as the lines a report shows, for a golden file.
pub fn notes_text(imported: &Imported) -> String {
    imported
        .notes
        .iter()
        .map(|n| {
            let about = if n.resource.is_empty() {
                String::new()
            } else {
                format!(" [{}]", n.resource)
            };
            format!("{}{about}: {n}\n", n.level)
        })
        .collect()
}

/// An artifact from path and content pairs.
pub fn artifact(files: &[(&str, &str)]) -> Artifact {
    Artifact {
        files: files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect(),
    }
}

/// A new, empty directory for one test, under Cargo's target directory when
/// the caller passes `env!("CARGO_TARGET_TMPDIR")`.
pub fn scratch(base: &str, name: &str) -> PathBuf {
    let dir = Path::new(base).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("creating a scratch directory");
    dir
}

/// Writes an executable shell script that stands in for a tool, and returns
/// its path. The script sees the tool's real arguments and working directory.
#[cfg(unix)]
pub fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).expect("creating the stand-in directory");
    let path = dir.join(name);
    // Written under another name and renamed, so the file is never executed
    // while it is still open for writing.
    let staged = dir.join(format!("{name}.staged"));
    fs::write(&staged, format!("#!/bin/sh\n{body}\n")).expect("writing the stand-in");
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).expect("marking it executable");
    fs::rename(&staged, &path).expect("placing the stand-in");
    path
}

/// A log that keeps every line, for asserting on what a runner relayed.
#[derive(Default)]
pub struct KeptLog(Mutex<Vec<LogLine>>);

impl KeptLog {
    /// The text of every line so far, in order.
    pub fn text(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("the log is not poisoned")
            .iter()
            .map(|l| l.text.clone())
            .collect()
    }
}

impl meridian_sdk::Log for KeptLog {
    fn line(&self, line: LogLine) {
        self.0.lock().expect("the log is not poisoned").push(line);
    }
}

/// Builds a workspace plugin's binary and returns its path, for a test that
/// needs the real process. Each binary is placed in a directory of its own,
/// so discovering that directory finds exactly one plugin.
///
/// A test cannot ask Cargo for another package's binary through an
/// environment variable, so it is built here, and Cargo is asked where it put
/// the result: the layout of a target directory is Cargo's business and has
/// changed between releases.
pub fn plugin_binary(package: &str) -> PathBuf {
    static BUILT: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut built = BUILT
        .get_or_init(Mutex::default)
        .lock()
        .expect("no build panicked while holding the lock");
    if let Some(path) = built.get(package) {
        return path.clone();
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build.args([
        "build",
        "--locked",
        "--message-format=json",
        "-p",
        package,
        "--bin",
        package,
    ]);
    // Built the way the test itself was, so a release test run does not also
    // compile a debug plugin.
    if !cfg!(debug_assertions) {
        build.arg("--release");
    }
    let output = build.current_dir(repo_root()).output().expect("cargo runs");
    assert!(
        output.status.success(),
        "building {package}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = String::from_utf8_lossy(&output.stdout);
    let executable = messages
        .lines()
        .filter(|line| line.contains("\"reason\":\"compiler-artifact\""))
        .filter_map(executable_in)
        .rfind(|path| path.file_stem().is_some_and(|n| n == package))
        .unwrap_or_else(|| panic!("cargo did not report where it built {package}:\n{messages}"));

    let dir = executable
        .parent()
        .expect("the binary is in a directory")
        .join("plugins-under-test")
        .join(package);
    fs::create_dir_all(&dir).expect("creating the plugin directory");
    let name = executable.file_name().expect("the binary has a name");
    // Copied under another name and renamed, so a test process running in
    // parallel never executes a half-written file.
    let staged = dir.join(format!("{package}.{}", std::process::id()));
    fs::copy(&executable, &staged).expect("copying the plugin binary");
    let placed = dir.join(name);
    fs::rename(&staged, &placed).expect("placing the plugin binary");
    built.insert(package.to_owned(), placed.clone());
    placed
}

/// The `executable` path in one of Cargo's JSON build messages.
fn executable_in(message: &str) -> Option<PathBuf> {
    let marker = "\"executable\":\"";
    let start = message.find(marker)? + marker.len();
    let mut path = String::new();
    let mut chars = message[start..].chars();
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
