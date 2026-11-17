//! The three providers that need nothing installed: the environment, a
//! file, and a command.
//!
//! Each reads straight into protected memory. None of them puts the value,
//! or anything read from the place the value lives, into an error: an error
//! names the provider and where it looked, and stops there.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use crate::{SecretValue, DEFAULT_MAX_LEN};

/// Why a secret could not be fetched. It never holds the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The place the secret was to be read from has none.
    Missing(String),
    /// The place exists and was refused, or reading it failed.
    Failed(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Missing(why) | FetchError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for FetchError {}

fn failed(what: impl fmt::Display, e: std::io::Error) -> FetchError {
    FetchError::Failed(format!("{what}: {e}"))
}

/// What [`from_env`] has taken out of the environment, by variable, so that
/// a second document reading the same variable still finds it. Held in
/// protected memory until the process ends.
static TAKEN: Mutex<BTreeMap<String, SecretValue>> = Mutex::new(BTreeMap::new());

/// Reads a secret from an environment variable of this process, then
/// removes the variable, so that no program Meridian starts inherits it.
///
/// The value is kept, in protected memory, for a later read of the same
/// variable: two documents of one run may declare the same one, and the
/// second would otherwise find it gone.
///
/// This is the weakest provider and is documented as such: the kernel's own
/// record of the environment the process started with is not Meridian's to
/// wipe.
pub fn from_env(key: &str) -> Result<SecretValue, FetchError> {
    let about = |e| failed(format_args!("the environment variable {key}"), e);
    let mut taken = TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(value) = std::env::var_os(key) else {
        return match taken.get(key) {
            Some(kept) => kept.duplicate().map_err(about),
            None => Err(FetchError::Missing(format!(
                "the environment variable {key} is not set"
            ))),
        };
    };
    std::env::remove_var(key);
    // The copy the standard library made is ordinary memory: move the bytes
    // to their home and wipe the copy.
    let mut bytes = value.into_encoded_bytes();
    let secret = SecretValue::take_from(&mut bytes).map_err(about)?;
    taken.insert(key.to_owned(), secret.duplicate().map_err(about)?);
    Ok(secret)
}

/// Whether [`from_env`] would find `key`: it is set, or was and was taken.
fn env_is_known(key: &str) -> bool {
    std::env::var_os(key).is_some()
        || TAKEN
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(key)
}

/// How [`from_file`] reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileOptions {
    /// Read the file even when its mode lets others than its owner read it.
    pub allow_wide_mode: bool,
    /// Keep a line ending at the end of the file as part of the secret.
    pub keep_trailing_newline: bool,
}

/// Reads a secret from a file.
///
/// A file others than its owner can read is refused unless the options say
/// otherwise: a secret anyone on the machine can read is rarely what was
/// meant, and saying so beats using it quietly.
pub fn from_file(path: &Path, options: FileOptions) -> Result<SecretValue, FetchError> {
    let shown = path.display();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FetchError::Missing(format!(
                "the file {shown} does not exist"
            )))
        }
        Err(e) => return Err(failed(format_args!("the file {shown}"), e)),
    };
    let meta = file
        .metadata()
        .map_err(|e| failed(format_args!("the file {shown}"), e))?;
    if !meta.is_file() {
        return Err(FetchError::Failed(format!("{shown} is not a file")));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 && !options.allow_wide_mode {
            return Err(FetchError::Failed(format!(
                "the file {shown} has mode {mode:04o}, which lets others than its owner read \
                 it; make it 0600, or set allowWideMode: true on the secret if that is \
                 intended"
            )));
        }
    }
    let secret = SecretValue::from_reader(file, DEFAULT_MAX_LEN)
        .map_err(|e| failed(format_args!("the file {shown}"), e))?;
    if options.keep_trailing_newline {
        Ok(secret)
    } else {
        secret
            .without_trailing_newline()
            .map_err(|e| failed(format_args!("the file {shown}"), e))
    }
}

/// Reads a secret from the standard output of a command.
///
/// The command is run as given, without a shell. Its standard input is
/// closed, its standard error is discarded (a command that fails may print
/// the very thing it was asked for), and a line ending at the end of what it
/// prints is not part of the secret.
pub fn from_exec(command: &[String]) -> Result<SecretValue, FetchError> {
    let Some((program, args)) = command.split_first() else {
        return Err(FetchError::Failed(
            "the command is empty: give the program and its arguments as a list".to_owned(),
        ));
    };
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FetchError::Failed(format!(
                "the command {program} was not found"
            )))
        }
        Err(e) => return Err(failed(format_args!("the command {program}"), e)),
    };
    let read = match child.stdout.take() {
        Some(stdout) => SecretValue::from_reader(stdout.take(u64::MAX), DEFAULT_MAX_LEN),
        None => Err(std::io::Error::other("its output could not be read")),
    };
    let status = child
        .wait()
        .map_err(|e| failed(format_args!("the command {program}"), e))?;
    // The status first: what a failed command printed is not a secret to
    // keep, and is dropped (and wiped) here.
    if !status.success() {
        let how = match status.code() {
            Some(code) => format!("exited with status {code}"),
            None => "was stopped by a signal".to_owned(),
        };
        return Err(FetchError::Failed(format!("the command {program} {how}")));
    }
    let secret = read.map_err(|e| failed(format_args!("the command {program}"), e))?;
    if secret.is_empty() {
        return Err(FetchError::Missing(format!(
            "the command {program} printed nothing"
        )));
    }
    secret
        .without_trailing_newline()
        .map_err(|e| failed(format_args!("the command {program}"), e))
}

/// The providers a secret can come from.
pub const PROVIDERS: &[&str] = &["env", "exec", "file"];

/// Where one secret comes from: a provider and what it needs. This is what
/// a document or a project file declares, and holds nothing of the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    /// The provider: one of [`PROVIDERS`].
    pub from: String,
    /// `env`: the environment variable.
    pub key: String,
    /// `file`: the file.
    pub path: String,
    /// `exec`: the program and its arguments. No shell reads them.
    pub command: Vec<String>,
    /// `file`: read a file that others than its owner can read.
    pub allow_wide_mode: bool,
    /// `file`: keep the value's last line ending.
    pub keep_trailing_newline: bool,
}

impl Source {
    /// What is wrong with the declaration, as the field at fault and why.
    /// Empty when the provider is known, has what it needs and is given
    /// nothing that means nothing to it.
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        let has = |field: &str| match field {
            "key" => !self.key.is_empty(),
            "path" => !self.path.is_empty(),
            "command" => !self.command.is_empty(),
            "allowWideMode" => self.allow_wide_mode,
            "keepTrailingNewline" => self.keep_trailing_newline,
            _ => false,
        };
        let (needs, takes): (&'static str, &[&str]) = match self.from.as_str() {
            "env" => ("key", &[]),
            "file" => ("path", &["allowWideMode", "keepTrailingNewline"]),
            "exec" => ("command", &[]),
            "" => {
                found.push((
                    "from",
                    format!(
                        "a secret needs a provider: from is one of {}",
                        PROVIDERS.join(", ")
                    ),
                ));
                return found;
            }
            other => {
                found.push((
                    "from",
                    format!(
                        "{other:?} is not a provider this version has; from is one of {}",
                        PROVIDERS.join(", ")
                    ),
                ));
                return found;
            }
        };
        let what = match needs {
            "key" => "key, the environment variable to read",
            "path" => "path, the file to read",
            _ => "command, the program to run and its arguments, as a list",
        };
        if !has(needs) {
            found.push((needs, format!("from: {} needs {what}", self.from)));
        } else if needs == "command" && self.command[0].is_empty() {
            found.push((
                needs,
                "the first item of command is the program to run".to_owned(),
            ));
        }
        for field in [
            "key",
            "path",
            "command",
            "allowWideMode",
            "keepTrailingNewline",
        ] {
            if field != needs && !takes.contains(&field) && has(field) {
                found.push((
                    field,
                    format!("{field} means nothing to from: {}; remove it", self.from),
                ));
            }
        }
        found
    }

    /// Where the secret is looked for, in words: for a report that says
    /// which place would answer. Never anything read from that place, and
    /// for a command only the program, not its arguments.
    pub fn place(&self) -> String {
        match self.from.as_str() {
            "env" => format!("the environment variable {}", self.key),
            "file" => format!("the file {}", self.path),
            "exec" => format!(
                "the command {}",
                self.command.first().map_or("", String::as_str)
            ),
            other => format!("the provider {other:?}"),
        }
    }

    /// Whether the place would answer, found out without reading the value.
    pub fn probe(&self) -> Probe {
        match self.from.as_str() {
            "env" => {
                if env_is_known(&self.key) {
                    Probe::Present(format!("the environment variable {} is set", self.key))
                } else {
                    Probe::Absent(format!("the environment variable {} is not set", self.key))
                }
            }
            "file" => probe_file(Path::new(&self.path), self.allow_wide_mode),
            "exec" => {
                let program = self.command.first().map_or("", String::as_str);
                if on_path(program) {
                    Probe::Unknown(format!(
                        "the command {program} is there, and is only run by plan and apply"
                    ))
                } else {
                    Probe::Refused(format!("the command {program} was not found"))
                }
            }
            other => Probe::Refused(format!(
                "{other:?} is not a provider this version has; from is one of {}",
                PROVIDERS.join(", ")
            )),
        }
    }

    /// Fetches the secret, into protected memory.
    pub fn fetch(&self) -> Result<SecretValue, FetchError> {
        match self.from.as_str() {
            "env" => from_env(&self.key),
            "file" => from_file(
                Path::new(&self.path),
                FileOptions {
                    allow_wide_mode: self.allow_wide_mode,
                    keep_trailing_newline: self.keep_trailing_newline,
                },
            ),
            "exec" => from_exec(&self.command),
            other => Err(FetchError::Failed(format!(
                "{other:?} is not a provider this version has; from is one of {}",
                PROVIDERS.join(", ")
            ))),
        }
    }
}

/// What [`Source::probe`] found, in words. It never holds any part of a
/// value: a probe does not read one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The place holds something a fetch would read.
    Present(String),
    /// The place holds nothing.
    Absent(String),
    /// The place is there and would be refused, or cannot be reached.
    Refused(String),
    /// Only a fetch can tell: a command is not run to find out.
    Unknown(String),
}

impl Probe {
    /// What was found, as a sentence without its full stop.
    pub fn words(&self) -> &str {
        match self {
            Probe::Present(w) | Probe::Absent(w) | Probe::Refused(w) | Probe::Unknown(w) => w,
        }
    }
}

fn probe_file(path: &Path, allow_wide_mode: bool) -> Probe {
    let shown = path.display();
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Probe::Absent(format!("the file {shown} does not exist"))
        }
        Err(e) => return Probe::Refused(format!("the file {shown}: {e}")),
    };
    if !meta.is_file() {
        return Probe::Refused(format!("{shown} is not a file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 && !allow_wide_mode {
            return Probe::Refused(format!(
                "the file {shown} has mode {mode:04o}, which lets others than its owner read \
                 it; make it 0600, or set allowWideMode: true on the secret if that is \
                 intended"
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = allow_wide_mode;
    if meta.len() > DEFAULT_MAX_LEN as u64 {
        return Probe::Refused(format!(
            "the file {shown} is larger than a secret can be ({} KiB)",
            DEFAULT_MAX_LEN / 1024
        ));
    }
    Probe::Present(format!("the file {shown} is there"))
}

/// Whether `program` names something to run: a path that is a file, or a
/// name found in a directory of `PATH`.
fn on_path(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    let direct = Path::new(program);
    if direct.components().count() > 1 {
        return direct.is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file() || (cfg!(windows) && candidate.with_extension("exe").is_file())
        })
    })
}
