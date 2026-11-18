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
    AgentPrograms, Artifact, Credential, Data, Emitted, Emitter, HeldCredential, ImportSource,
    Imported, Importer, LogLine, Position, ResourceGraph, SecretName, SecretRef, SecretValue,
    Secrets, Value,
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

/// Whether a file in a golden directory is a golden file of `case`.
///
/// A golden name is `<case>.<suffix>` where the suffix is an artifact path
/// with its slashes and dots turned into underscores, or `warnings` or
/// `refused`: so it never holds a second dot. A test's own input beside its
/// golden files, `web.yaml` for the case `web`, is therefore not a golden
/// file, and recording the case again neither compares nor deletes it.
pub fn is_golden_of(case: &str, name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix(case)
        .and_then(|rest| rest.strip_prefix('.'))
    else {
        return false;
    };
    const SOURCE_EXTENSIONS: &[&str] = &[
        "yaml", "yml", "json", "md", "toml", "rs", "sh", "ps1", "py", "txt", "hcl", "tf",
    ];
    !suffix.is_empty() && !suffix.contains('.') && !SOURCE_EXTENSIONS.contains(&suffix)
}

fn assert_golden_files(dir: &Path, case: &str, got: &BTreeMap<String, String>) {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(dir).expect("creating the golden directory");
        // Only a golden file of this case that the compile no longer
        // produces is removed; anything else in the directory is not the
        // kit's to delete.
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().into_string().unwrap_or_default();
                if is_golden_of(case, &name) && !got.contains_key(&name) {
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
        .filter(|name| is_golden_of(case, name))
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
        "{case}: the set of golden files differs; run the tests with UPDATE_GOLDEN=1 to record them again"
    );
    for (name, text) in got {
        assert_eq!(
            text, &want[name],
            "{name}: run the tests with UPDATE_GOLDEN=1 to record it again"
        );
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

/// Puts a secret called `name` in the param `param` of resource `id`, as
/// the host does for a target that takes secrets: the param becomes a
/// [`Value::Secret`] and the graph names it. Returns the secret's slot, for
/// a run context made with [`secrets`].
pub fn secret_in_param(graph: &mut ResourceGraph, id: &str, param: &str, name: &str) -> SecretRef {
    let slot = SecretRef::new(graph.secrets.len() as u64 + 40);
    let resource = graph
        .resources
        .iter_mut()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no resource {id:?} in the graph"));
    resource
        .params
        .insert(param.to_owned(), Value::Secret(slot));
    graph.secrets.push(SecretName {
        secret: slot,
        name: name.to_owned(),
    });
    slot
}

/// As [`secret_in_param`], for a place inside a param: `path` is the param
/// and then the keys of the maps below it, and the last key's value becomes
/// the secret. Every map on the way is made when it is missing.
pub fn secret_at(graph: &mut ResourceGraph, id: &str, path: &[&str], name: &str) -> SecretRef {
    let slot = SecretRef::new(graph.secrets.len() as u64 + 40);
    let resource = graph
        .resources
        .iter_mut()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no resource {id:?} in the graph"));
    let (last, inner) = path.split_last().expect("a path has at least one key");
    let mut map = &mut resource.params;
    for key in inner {
        let entry = map
            .entry((*key).to_owned())
            .or_insert_with(|| Value::Map(Default::default()));
        match entry {
            Value::Map(next) => map = next,
            other => panic!("{key} of {id} is {}, not a map", other.describe()),
        }
    }
    map.insert((*last).to_owned(), Value::Secret(slot));
    graph.secrets.push(SecretName {
        secret: slot,
        name: name.to_owned(),
    });
    slot
}

/// The secrets a run context hands a runner, from slot, name and value.
pub fn secrets(values: &[(SecretRef, &str, &str)]) -> Secrets {
    Secrets::from_values(
        values
            .iter()
            .map(|(slot, name, value)| {
                let value = SecretValue::from_reader(value.as_bytes(), 64 * 1024)
                    .expect("a secret fits in memory");
                (*slot, (*name).to_owned(), value)
            })
            .collect(),
    )
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

/// Whether a test against the real tool was asked for, by the one rule every
/// real-tool test follows: `MERIDIAN_TEST_REAL_<TOOL>` is set and not empty.
///
/// Returns `None`, with a line saying what to set, when it was not. A test
/// then returns early; it is also marked `#[ignore]`, so a plain `cargo test`
/// counts it as ignored and never as passed. With
/// `MERIDIAN_TEST_REAL_REQUIRED` set as well, a switch that is missing is a
/// failure, so a CI job cannot go green because its variable was misspelt.
///
/// `tool` is the upper-case part of the variable, such as `"TERRAFORM"`.
pub fn real_tool_asked(tool: &str) -> Option<String> {
    let variable = format!("MERIDIAN_TEST_REAL_{tool}");
    let value = std::env::var(&variable)
        .ok()
        .filter(|v| !v.trim().is_empty() && v.trim() != "0");
    if value.is_none() {
        assert!(
            std::env::var_os("MERIDIAN_TEST_REAL_REQUIRED").is_none(),
            "MERIDIAN_TEST_REAL_REQUIRED is set, and {variable} is not: this run was meant to \
             test the real tool and would have checked nothing"
        );
        eprintln!("skipped: set {variable}=1 to run against the real tool");
    }
    value
}

/// The program a real-tool test runs, when the test was asked for: the
/// value of `MERIDIAN_<TOOL>` (how the plugin itself is told where its tool
/// is), or `default`.
pub fn real_tool_program(tool: &str, default: &str) -> Option<String> {
    real_tool_asked(tool)?;
    Some(
        std::env::var(format!("MERIDIAN_{tool}"))
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| default.to_owned()),
    )
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
///
/// On Windows a script cannot be started as a program, so the stand-in is a
/// small program, `<name>.exe`, that runs the script `<name>.sh` beside it
/// with the `sh` that Git for Windows installs (see `src/bin/meridian-stand-in.rs`).
/// The path returned is the program's.
pub fn stand_in(dir: &Path, name: &str, body: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("creating the stand-in directory");
    place_stand_in(dir, name, &format!("#!/bin/sh\n{body}\n"))
}

#[cfg(unix)]
fn place_stand_in(dir: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    // Written under another name and renamed, so the file is never executed
    // while it is still open for writing. The name is this writer's own, so
    // two tests placing a stand-in of the same name at once do not rename
    // each other's half-written file.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let staged = dir.join(format!(
        "{name}.staged.{}.{}",
        std::process::id(),
        WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::write(&staged, script).expect("writing the stand-in");
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).expect("marking it executable");
    fs::rename(&staged, &path).expect("placing the stand-in");
    path
}

#[cfg(not(unix))]
fn place_stand_in(dir: &Path, name: &str, script: &str) -> PathBuf {
    let launcher = built_binary("meridian-plugin-testkit", "meridian-stand-in");
    // The name is extended, not given a new extension: `docker.real` must
    // become `docker.real.exe` and its script `docker.real.sh`.
    let with = |extension: &str| dir.join(format!("{name}.{extension}"));
    // The launcher passes the tool's arguments in the environment; these
    // lines make them the script's own. `pwd` answers with the directory as
    // Windows names it, which is how the code under test names it too.
    let script = script.replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\n\
         set --\n\
         meridian_arg=1\n\
         while [ \"$meridian_arg\" -le \"${MERIDIAN_STAND_IN_ARGC:-0}\" ]; do\n\
         \x20 eval \"set -- \\\"\\$@\\\" \\\"\\$MERIDIAN_STAND_IN_ARG_$meridian_arg\\\"\"\n\
         \x20 meridian_arg=$((meridian_arg + 1))\n\
         done\n\
         unset meridian_arg\n\
         pwd() { cygpath -w \"$(command pwd)\"; }\n",
        1,
    );
    fs::write(with("sh"), script).expect("writing the stand-in's script");
    let path = with("exe");
    // A stand-in a test replaces may still be running; one that is already
    // there is the same launcher, so only its script needed replacing.
    if let Err(err) = fs::copy(&launcher, &path) {
        assert!(
            path.is_file(),
            "placing the stand-in {}: {err}",
            path.display()
        );
    }
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
pub fn plugin_binary(package: &str) -> PathBuf {
    built_binary(package, package)
}

/// Builds one binary of a workspace package and returns the path of a copy
/// that is safe to run, in a directory of its own.
///
/// A test cannot ask Cargo for another package's binary through an
/// environment variable, so it is built here, and Cargo is asked where it put
/// the result: the layout of a target directory is Cargo's business and has
/// changed between releases.
pub fn built_binary(package: &str, binary: &str) -> PathBuf {
    static BUILT: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut built = BUILT
        .get_or_init(Mutex::default)
        .lock()
        .expect("no build panicked while holding the lock");
    if let Some(path) = built.get(binary) {
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
        binary,
    ]);
    // Built the way the test itself was, so a release test run does not also
    // compile a debug plugin.
    if !cfg!(debug_assertions) {
        build.arg("--release");
    }
    let output = build.current_dir(repo_root()).output().expect("cargo runs");
    assert!(
        output.status.success(),
        "building {binary}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = String::from_utf8_lossy(&output.stdout);
    let executable = messages
        .lines()
        .filter(|line| line.contains("\"reason\":\"compiler-artifact\""))
        .filter_map(executable_in)
        .rfind(|path| path.file_stem().is_some_and(|n| n == binary))
        .unwrap_or_else(|| panic!("cargo did not report where it built {binary}:\n{messages}"));

    let dir = executable
        .parent()
        .expect("the binary is in a directory")
        .join("plugins-under-test")
        .join(binary);
    fs::create_dir_all(&dir).expect("creating the plugin directory");
    let name = executable.file_name().expect("the binary has a name");
    // Copied under another name and renamed, so a test process running in
    // parallel never executes a half-written file.
    let staged = dir.join(format!("{binary}.{}", std::process::id()));
    fs::copy(&executable, &staged).expect("copying the plugin binary");
    let placed = dir.join(name);
    if let Err(err) = fs::rename(&staged, &placed) {
        // Windows will not replace a program that is running. One that is
        // already in place was built from the same sources moments ago.
        let _ = fs::remove_file(&staged);
        assert!(placed.is_file(), "placing {}: {err}", placed.display());
    }
    built.insert(binary.to_owned(), placed.clone());
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

/// A credential of `kind` for `user`, with the value of each secret field
/// given, as a runner receives one at plan and apply.
pub fn credential(kind: &str, user: &str, fields: &[(&str, &str)]) -> HeldCredential {
    let mut text = BTreeMap::new();
    if !user.is_empty() {
        text.insert("user".to_owned(), user.to_owned());
    }
    HeldCredential::from_values(
        Credential {
            name: "deploy".to_owned(),
            kind: kind.to_owned(),
            text,
            ..Credential::default()
        },
        fields
            .iter()
            .map(|(field, value)| {
                (
                    (*field).to_owned(),
                    SecretValue::from_reader(value.as_bytes(), 1 << 16)
                        .expect("a test value fits in a secret"),
                )
            })
            .collect(),
    )
}

/// Stand-ins for `ssh-agent` and `ssh-add` that record what they are given.
/// The agent creates a plain file where its socket is asked for and waits
/// until it is stopped or that file is gone (on Windows a stopped stand-in
/// is its launcher only, and the shell under it would otherwise live on,
/// holding the test run's output open); `ssh-add` keeps the key it reads on standard input in
/// `added-key`, its arguments in `ssh-add.args`, its environment in
/// `ssh-add.env`, and what its askpass program answers in `added-passphrase`.
pub fn agent_stand_ins(dir: &Path) -> AgentPrograms {
    let d = dir.display();
    let ssh_agent = stand_in(
        dir,
        "ssh-agent",
        &format!(
            r#"echo "$@" >> "{d}/ssh-agent.args"
sock=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-a" ]; then sock="$a"; fi
  prev="$a"
done
: > "$sock"
trap 'exit 0' TERM INT
while [ -e "$sock" ]; do sleep 1; done"#
        ),
    );
    let ssh_add = stand_in(
        dir,
        "ssh-add",
        &format!(
            r#"echo "$@" >> "{d}/ssh-add.args"
env > "{d}/ssh-add.env"
cat > "{d}/added-key"
if [ -n "$SSH_ASKPASS" ]; then "$SSH_ASKPASS" > "{d}/added-passphrase"; fi"#
        ),
    );
    AgentPrograms {
        ssh_agent: ssh_agent.to_string_lossy().into_owned(),
        ssh_add: ssh_add.to_string_lossy().into_owned(),
    }
}
