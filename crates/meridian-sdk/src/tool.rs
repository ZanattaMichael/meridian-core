//! What nearly every [`crate::Runner`] needs: put the artifact on disk, run
//! the real tool, relay what it prints, and turn a failure into a
//! [`RunError`] that says what was run.
//!
//! Nothing here is required. A runner may drive its tool any way it likes; this
//! is the first-party plugins' common ground, published so a third-party one
//! does not have to rewrite it.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::{Artifact, Log, RunError};

/// How much of a tool's output a failure carries.
const TAIL_BYTES: usize = 8 * 1024;

/// The file in a work directory that lists what the last artifact wrote.
const MANIFEST: &str = ".meridian-files";

/// Writes an artifact into a directory, creating it if needed.
///
/// Files the previous artifact wrote and this one does not are removed, so a
/// resource deleted from a document does not linger as a file the tool still
/// reads. Anything else in the directory, such as the tool's own state, is
/// left alone. A path that would land outside the directory is refused.
pub fn write_artifact(dir: &Path, artifact: &Artifact) -> Result<(), RunError> {
    let fail = |what: &str, path: &Path, err: &dyn std::fmt::Display| {
        RunError::new(format!("could not {what} {}: {err}", path.display()))
    };
    for name in artifact.files.keys() {
        if !is_contained(name) {
            return Err(RunError::new(format!(
                "the artifact names the file {name:?}, which is not a path inside the work directory"
            )));
        }
    }
    fs::create_dir_all(dir).map_err(|e| fail("create", dir, &e))?;

    let manifest = dir.join(MANIFEST);
    if let Ok(previous) = fs::read_to_string(&manifest) {
        for old in previous.lines() {
            if is_contained(old) && !artifact.files.contains_key(old) {
                let _ = fs::remove_file(dir.join(old));
            }
        }
    }
    for (name, content) in &artifact.files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| fail("create", parent, &e))?;
        }
        fs::write(&path, content).map_err(|e| fail("write", &path, &e))?;
    }
    let mut listing = String::new();
    for name in artifact.files.keys() {
        listing.push_str(name);
        listing.push('\n');
    }
    fs::write(&manifest, listing).map_err(|e| fail("write", &manifest, &e))
}

/// Whether a relative path stays inside the directory it is joined to.
fn is_contained(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\n')
        && Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// The absolute form of a path, as a tool can be given it.
///
/// Links are resolved when the path exists; when it does not, the path is
/// only made absolute. On Windows the result never carries the `\\?\` prefix
/// that resolving a path adds there: Windows itself accepts that form, but
/// most command-line tools do not, and a `file://` URL built from it is not
/// one.
pub fn absolute_path(path: &Path) -> PathBuf {
    let resolved = path
        .canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_owned());
    without_verbatim_prefix(resolved)
}

/// `\\?\C:\dir` as `C:\dir`, and `\\?\UNC\server\share` as `\\server\share`.
/// Any other path, including every path on another system, is returned as is.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        // Only a drive path has a plain form; `\\?\Volume{...}` does not.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// One invocation of a command-line tool.
#[derive(Clone, Debug)]
pub struct Tool {
    program: String,
    args: Vec<String>,
    /// The arguments as a person is shown them, with secrets replaced.
    shown: Vec<String>,
    dir: Option<PathBuf>,
    env: Vec<(String, String)>,
    stdin: Option<String>,
    relay_stdout: bool,
}

impl Tool {
    /// A tool found on `PATH` by name, or run from a path.
    pub fn new(program: impl Into<String>) -> Self {
        Tool {
            program: program.into(),
            args: Vec::new(),
            shown: Vec::new(),
            dir: None,
            env: Vec::new(),
            stdin: None,
            relay_stdout: true,
        }
    }

    /// A tool whose program an operator may replace through an environment
    /// variable, for a binary that is not on `PATH` or goes by another name.
    pub fn from_env(variable: &str, default: &str) -> Self {
        match std::env::var(variable) {
            Ok(value) if !value.trim().is_empty() => Tool::new(value),
            _ => Tool::new(default),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        let arg = arg.into();
        self.shown.push(arg.clone());
        self.args.push(arg);
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for arg in args {
            self = self.arg(arg);
        }
        self
    }

    /// An argument that must not appear in a log or an error.
    pub fn secret_arg(mut self, arg: impl Into<String>) -> Self {
        self.shown.push("***".to_owned());
        self.args.push(arg.into());
        self
    }

    /// The directory the tool runs in.
    pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Text written to the tool's standard input, which is otherwise closed.
    pub fn stdin(mut self, text: impl Into<String>) -> Self {
        self.stdin = Some(text.into());
        self
    }

    /// Treats standard output as data and not as something to show: it is kept
    /// whole in [`Finished::stdout`] and not relayed to the log. For a tool
    /// asked to print JSON.
    pub fn capture_stdout(mut self) -> Self {
        self.relay_stdout = false;
        self
    }

    /// The command as a person would type it, with secret arguments hidden.
    pub fn command_line(&self) -> String {
        let mut line = display_word(&self.program);
        for arg in &self.shown {
            line.push(' ');
            line.push_str(&display_word(arg));
        }
        line
    }

    /// Runs the tool to the end, relaying its output line by line.
    ///
    /// Any exit status is a result; only failing to start the tool at all is
    /// an error. Use [`Tool::run_ok`] when a non-zero status is a failure.
    pub fn run(&self, log: &dyn Log) -> Result<Finished, RunError> {
        let mut command = Command::new(resolve_program(&self.program));
        command
            .args(&self.args)
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.dir {
            // Spawning in a directory that is not there fails with "not
            // found", which would read as the program being missing.
            if !dir.is_dir() {
                return Err(RunError {
                    msg: format!(
                        "could not run {}: its working directory {} does not exist",
                        self.program,
                        dir.display()
                    ),
                    command: self.command_line(),
                    ..RunError::default()
                });
            }
            command.current_dir(dir);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }

        let mut child = spawn_when_ready(&mut command).map_err(|err| RunError {
            msg: if err.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "could not run {}: it is not installed, or not on the PATH of the machine running Meridian",
                    self.program
                )
            } else {
                format!("could not run {}: {err}", self.program)
            },
            command: self.command_line(),
            ..RunError::default()
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let tail = Mutex::new(Tail::default());
        let relay_stdout = self.relay_stdout;

        let (out, err) = std::thread::scope(|scope| {
            if let (Some(mut pipe), Some(text)) = (stdin, self.stdin.as_deref()) {
                scope.spawn(move || {
                    // A tool that exits without reading its input is not this
                    // call's failure to report; its exit status is.
                    let _ = pipe.write_all(text.as_bytes());
                });
            }
            let out = scope.spawn(|| {
                read_lines(stdout, |line| {
                    if relay_stdout {
                        log.out(line);
                        tail.lock().expect("no panic holds the tail").push(line);
                    }
                })
            });
            let err = scope.spawn(|| {
                read_lines(stderr, |line| {
                    log.err(line);
                    tail.lock().expect("no panic holds the tail").push(line);
                })
            });
            (
                out.join().unwrap_or_default(),
                err.join().unwrap_or_default(),
            )
        });

        let status = child.wait().map_err(|err| RunError {
            msg: format!("could not wait for {}: {err}", self.program),
            command: self.command_line(),
            ..RunError::default()
        })?;

        Ok(Finished {
            code: status.code(),
            stdout: out,
            stderr: err,
            tail: tail.into_inner().expect("no panic holds the tail").text(),
            command: self.command_line(),
        })
    }

    /// Runs the tool and treats a non-zero exit status as a failure described
    /// by `what`, for example `"terraform init failed"`.
    pub fn run_ok(&self, what: &str, log: &dyn Log) -> Result<Finished, RunError> {
        let finished = self.run(log)?;
        if finished.success() {
            Ok(finished)
        } else {
            Err(finished.into_error(what))
        }
    }
}

/// A tool that ran to the end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// The exit status, or `None` when a signal ended the tool.
    pub code: Option<i32>,
    /// Everything written to standard output.
    pub stdout: String,
    /// Everything written to standard error.
    pub stderr: String,
    tail: String,
    command: String,
}

impl Finished {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The end of what the tool printed, in the order it appeared.
    pub fn tail(&self) -> &str {
        &self.tail
    }

    /// The failure this run amounts to, carrying the command, the exit status
    /// and the end of the output.
    pub fn into_error(self, msg: impl Into<String>) -> RunError {
        RunError {
            msg: msg.into(),
            command: self.command,
            exit_code: self.code,
            output: self.tail,
            changes: Vec::new(),
        }
    }
}

/// The last lines of a tool's output, bounded in size.
#[derive(Default)]
struct Tail {
    lines: VecDeque<String>,
    bytes: usize,
}

impl Tail {
    fn push(&mut self, line: &str) {
        self.bytes += line.len() + 1;
        self.lines.push_back(line.to_owned());
        while self.bytes > TAIL_BYTES && self.lines.len() > 1 {
            if let Some(dropped) = self.lines.pop_front() {
                self.bytes -= dropped.len() + 1;
            }
        }
    }

    fn text(self) -> String {
        let mut text = String::with_capacity(self.bytes);
        for line in self.lines {
            text.push_str(&line);
            text.push('\n');
        }
        text
    }
}

/// Reads a stream to its end, handing over each line as it completes, and
/// returns everything read. Bytes that are not UTF-8 are replaced, not fatal.
fn read_lines(stream: impl Read, mut each: impl FnMut(&str)) -> String {
    let mut reader = BufReader::new(stream);
    let mut all = String::new();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buffer);
                all.push_str(&text);
                each(text.trim_end_matches(['\n', '\r']));
            }
        }
    }
    all
}

/// One word of a command line, quoted only when a shell would need it.
fn display_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The file to start for a program name.
///
/// On Unix a name is what it is. On Windows a tool is as often a script as a
/// program: `az` is `az.cmd`, `puppet` is `puppet.bat`, and starting a process
/// by bare name finds only `.exe` files. So the name is looked up the way a
/// Windows shell would look it up, through `PATH` and the executable
/// extensions, and the file that is found is what gets started.
#[cfg(not(windows))]
fn resolve_program(program: &str) -> PathBuf {
    PathBuf::from(program)
}

#[cfg(windows)]
fn resolve_program(program: &str) -> PathBuf {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    find_program(program, &path_dirs, |candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(program))
}

/// The extensions Windows runs, in the order a shell tries them. Scripts for
/// an interpreter (`.ps1`, `.vbs`, `.js`) are left out: a process cannot be
/// started from one directly.
#[cfg(any(windows, test))]
const EXECUTABLE_EXTENSIONS: [&str; 4] = ["exe", "cmd", "bat", "com"];

/// Finds the file a program name stands for among `path_dirs`, or beside the
/// path when the name is one. `exists` answers whether a candidate is a file.
#[cfg(any(windows, test))]
fn find_program(
    program: &str,
    path_dirs: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let given = Path::new(program);
    let has_known_extension = given.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        EXECUTABLE_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(e))
    });
    let candidates = |base: &Path| -> Option<PathBuf> {
        if has_known_extension {
            return exists(base).then(|| base.to_path_buf());
        }
        // The extension is added to the whole name: `kubectl.docker` is a
        // name, not a file called `kubectl` with the extension `docker`.
        EXECUTABLE_EXTENSIONS
            .iter()
            .map(|ext| {
                let mut name = base.as_os_str().to_owned();
                name.push(".");
                name.push(ext);
                PathBuf::from(name)
            })
            .find(|candidate| exists(candidate))
            // A file with no extension cannot be started on Windows, but
            // leaving it as given keeps the error about the name asked for.
            .or_else(|| exists(base).then(|| base.to_path_buf()))
    };
    let is_path = given.is_absolute() || program.contains(['/', '\\']);
    if is_path {
        return candidates(given);
    }
    path_dirs
        .iter()
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| candidates(&dir.join(given)))
}

/// Starts a process, trying again when the program is briefly "busy".
///
/// A program that was written a moment ago cannot be run while any process
/// still holds it open for writing, and a process another thread is in the
/// middle of starting holds every open file for an instant. The condition
/// clears by itself, so it is waited out instead of reported.
fn spawn_when_ready(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    // "Text file busy" on Unix, and a sharing violation on Windows.
    const BUSY: i32 = if cfg!(windows) { 32 } else { 26 };
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(err) if err.raw_os_error() == Some(BUSY) && attempt < 50 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoLog;
    // The tests that run a real command use `sh` and `cat`, which a Windows
    // machine need not have. Running a tool there is covered by the plugins'
    // runner tests, whose stand-ins the test kit makes portable.
    #[cfg(unix)]
    use crate::{LogLine, LogStream};

    #[cfg(unix)]
    #[derive(Default)]
    struct Kept(Mutex<Vec<LogLine>>);

    #[cfg(unix)]
    impl Log for Kept {
        fn line(&self, line: LogLine) {
            self.0.lock().unwrap().push(line);
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meridian-sdk-tool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_command_line_hides_secrets_and_quotes_what_needs_it() {
        let tool = Tool::new("tool")
            .arg("apply")
            .arg("-var=name=a b")
            .secret_arg("hunter2")
            .arg("");
        assert_eq!(tool.command_line(), "tool apply '-var=name=a b' '***' ''");
    }

    #[cfg(unix)]
    #[test]
    fn output_is_relayed_by_stream_and_kept() {
        let log = Kept::default();
        let finished = Tool::new("sh")
            .args(["-c", "echo one; echo two >&2; exit 3"])
            .run(&log)
            .unwrap();
        assert_eq!(finished.code, Some(3));
        assert_eq!(finished.stdout, "one\n");
        assert_eq!(finished.stderr, "two\n");
        let lines = log.0.lock().unwrap();
        assert!(lines.contains(&LogLine {
            stream: LogStream::Stdout,
            text: "one".into()
        }));
        assert!(lines.contains(&LogLine {
            stream: LogStream::Stderr,
            text: "two".into()
        }));
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_says_what_ran_how_it_ended_and_what_it_printed() {
        let err = Tool::new("sh")
            .args(["-c", "echo 'no such host' >&2; exit 4"])
            .run_ok("the tool failed", &NoLog)
            .unwrap_err();
        assert_eq!(err.msg, "the tool failed");
        assert_eq!(err.exit_code, Some(4));
        assert_eq!(err.output, "no such host\n");
        assert_eq!(
            err.command,
            "sh -c 'echo '\\''no such host'\\'' >&2; exit 4'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_stdout_is_data_and_stdin_reaches_the_tool() {
        let log = Kept::default();
        let finished = Tool::new("cat")
            .stdin("{\"a\": 1}\n")
            .capture_stdout()
            .run(&log)
            .unwrap();
        assert_eq!(finished.stdout, "{\"a\": 1}\n");
        assert!(log.0.lock().unwrap().is_empty());
    }

    #[test]
    fn an_absolute_path_never_carries_the_verbatim_prefix() {
        for (given, want) in [
            (r"\\?\C:\work\dir", r"C:\work\dir"),
            (r"\\?\UNC\server\share\dir", r"\\server\share\dir"),
            (r"\\?\Volume{b75e2c83}\dir", r"\\?\Volume{b75e2c83}\dir"),
            (r"C:\work", r"C:\work"),
            ("/var/lib/meridian", "/var/lib/meridian"),
        ] {
            assert_eq!(
                without_verbatim_prefix(PathBuf::from(given)),
                PathBuf::from(want)
            );
        }
        let here = absolute_path(Path::new("."));
        assert!(here.is_absolute());
        assert!(!here.to_string_lossy().starts_with(r"\\?\"));
        // A path that does not exist is still made absolute.
        assert!(absolute_path(Path::new("no-such-directory-here")).is_absolute());
    }

    #[test]
    fn a_program_name_is_looked_up_as_a_windows_shell_would() {
        let files = [
            Path::new("tools").join("az.cmd"),
            Path::new("tools").join("terraform.exe"),
            Path::new("tools").join("terraform.cmd"),
            Path::new("tools").join("kubectl.docker.exe"),
            Path::new("other").join("az.exe"),
            Path::new("stand-ins").join("helm.exe"),
            Path::new("stand-ins").join("plain"),
        ];
        let exists = |candidate: &Path| files.iter().any(|f| f == candidate);
        let dirs = [PathBuf::from(""), "tools".into(), "other".into()];
        let find = |program: &str| find_program(program, &dirs, exists);

        // The first directory on the path wins, whatever the extension.
        assert_eq!(find("az"), Some(Path::new("tools").join("az.cmd")));
        // Within a directory a program is preferred to a script.
        assert_eq!(
            find("terraform"),
            Some(Path::new("tools").join("terraform.exe"))
        );
        assert_eq!(
            find("terraform.cmd"),
            Some(Path::new("tools").join("terraform.cmd"))
        );
        assert_eq!(
            find("kubectl.docker"),
            Some(Path::new("tools").join("kubectl.docker.exe"))
        );
        assert_eq!(find("pulumi"), None);
        // A path is completed where it is, and not searched for.
        let given = Path::new("stand-ins").join("helm");
        assert_eq!(
            find(given.to_str().unwrap()),
            Some(Path::new("stand-ins").join("helm.exe"))
        );
        let plain = Path::new("stand-ins").join("plain");
        assert_eq!(find(plain.to_str().unwrap()), Some(plain.clone()));
        assert_eq!(
            find(Path::new("stand-ins").join("az").to_str().unwrap()),
            None
        );
    }

    #[test]
    fn a_tool_that_is_not_installed_is_named() {
        let err = Tool::new("meridian-no-such-tool")
            .arg("x")
            .run(&NoLog)
            .unwrap_err();
        assert!(
            err.msg
                .starts_with("could not run meridian-no-such-tool: it is not installed"),
            "{}",
            err.msg
        );
        assert_eq!(err.command, "meridian-no-such-tool x");
    }

    #[test]
    fn an_artifact_replaces_the_last_one_and_leaves_the_tools_files() {
        let dir = scratch("artifact");
        let mut first = Artifact::default();
        first.files.insert("main.tf".into(), "one".into());
        first.files.insert("modules/a.tf".into(), "a".into());
        write_artifact(&dir, &first).unwrap();
        fs::write(dir.join("terraform.tfstate"), "state").unwrap();

        let mut second = Artifact::default();
        second.files.insert("main.tf".into(), "two".into());
        write_artifact(&dir, &second).unwrap();

        assert_eq!(fs::read_to_string(dir.join("main.tf")).unwrap(), "two");
        assert!(!dir.join("modules/a.tf").exists());
        assert_eq!(
            fs::read_to_string(dir.join("terraform.tfstate")).unwrap(),
            "state"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_artifact_cannot_write_outside_its_directory() {
        let dir = scratch("escape");
        for name in ["../x", "/etc/x", "a/../../x", ""] {
            let mut artifact = Artifact::default();
            artifact.files.insert(name.into(), "x".into());
            let err = write_artifact(&dir, &artifact).unwrap_err();
            assert!(err.msg.contains("not a path inside"), "{}", err.msg);
        }
        assert!(!dir.exists());
    }
}
