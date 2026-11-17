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

use crate::{stores, SecretValue, DEFAULT_MAX_LEN};

/// Why a secret could not be fetched. It never holds the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The place the secret was to be read from has none.
    Missing(String),
    /// The place exists and was refused, or reading it failed.
    Failed(String),
    /// The provider cannot be used on this machine at all: another
    /// operating system, no session bus, its tool not installed.
    Unavailable(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Missing(why) | FetchError::Failed(why) | FetchError::Unavailable(why) => {
                f.write_str(why)
            }
        }
    }
}

impl std::error::Error for FetchError {}

pub(crate) fn failed(what: impl fmt::Display, e: std::io::Error) -> FetchError {
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
pub const PROVIDERS: &[&str] = &[
    "env",
    "exec",
    "file",
    "keychain",
    "keyring",
    "secret-service",
    "systemd-creds",
];

/// Every field a source can have, as a document writes them.
const FIELDS: &[&str] = &[
    "key",
    "path",
    "command",
    "service",
    "account",
    "attributes",
    "allowWideMode",
    "keepTrailingNewline",
];

/// What a provider must be given and what else it takes, each field with
/// what it is for.
type Needs = &'static [(&'static str, &'static str)];

fn shape(from: &str) -> Option<(Needs, &'static [&'static str])> {
    Some(match from {
        "env" => (&[("key", "the environment variable to read")], &[]),
        "file" => (
            &[("path", "the file to read")],
            &["allowWideMode", "keepTrailingNewline"],
        ),
        "exec" => (
            &[("command", "the program to run and its arguments, as a list")],
            &[],
        ),
        "keyring" => (
            &[("key", "the description of the key in the kernel keyring")],
            &[],
        ),
        "systemd-creds" => (
            &[("key", "the name of the credential the unit was given")],
            &["keepTrailingNewline"],
        ),
        "secret-service" => (
            &[("attributes", "the attributes the item was stored with")],
            &[],
        ),
        "keychain" => (
            &[
                ("service", "the keychain item's service"),
                ("account", "the keychain item's account"),
            ],
            &[],
        ),
        _ => return None,
    })
}

/// Where one secret comes from: a provider and what it needs. This is what
/// a document or a project file declares, and holds nothing of the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    /// The provider: one of [`PROVIDERS`].
    pub from: String,
    /// `env`: the environment variable. `keyring`: the key's description.
    /// `systemd-creds`: the credential's name.
    pub key: String,
    /// `file`: the file.
    pub path: String,
    /// `exec`: the program and its arguments. No shell reads them.
    pub command: Vec<String>,
    /// `keychain`: the item's service.
    pub service: String,
    /// `keychain`: the item's account.
    pub account: String,
    /// `secret-service`: the attributes the item is found by.
    pub attributes: BTreeMap<String, String>,
    /// `file`: read a file that others than its owner can read.
    pub allow_wide_mode: bool,
    /// `file`, `systemd-creds`: keep the value's last line ending.
    pub keep_trailing_newline: bool,
}

impl Source {
    fn has(&self, field: &str) -> bool {
        match field {
            "key" => !self.key.is_empty(),
            "path" => !self.path.is_empty(),
            "command" => !self.command.is_empty(),
            "service" => !self.service.is_empty(),
            "account" => !self.account.is_empty(),
            "attributes" => !self.attributes.is_empty(),
            "allowWideMode" => self.allow_wide_mode,
            "keepTrailingNewline" => self.keep_trailing_newline,
            _ => false,
        }
    }

    /// What is wrong with the declaration, as the field at fault and why.
    /// Empty when the provider is known, has what it needs and is given
    /// nothing that means nothing to it.
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        let Some((needs, takes)) = shape(&self.from) else {
            let why = if self.from.is_empty() {
                format!(
                    "a secret needs a provider: from is one of {}",
                    PROVIDERS.join(", ")
                )
            } else {
                format!(
                    "{:?} is not a provider this version has; from is one of {}",
                    self.from,
                    PROVIDERS.join(", ")
                )
            };
            found.push(("from", why));
            return found;
        };
        for (field, what) in needs {
            if !self.has(field) {
                found.push((*field, format!("from: {} needs {field}, {what}", self.from)));
            } else if *field == "command" && self.command[0].is_empty() {
                found.push((
                    *field,
                    "the first item of command is the program to run".to_owned(),
                ));
            }
        }
        for field in FIELDS {
            let used = needs.iter().any(|(name, _)| name == field) || takes.contains(field);
            if !used && self.has(field) {
                found.push((
                    *field,
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
            "keyring" => format!("the kernel keyring, key {}", self.key),
            "systemd-creds" => format!("the unit's credential {}", self.key),
            "secret-service" => format!("the Secret Service, item {}", self.item()),
            "keychain" => format!(
                "the keychain, service {} and account {}",
                self.service, self.account
            ),
            other => format!("the provider {other:?}"),
        }
    }

    /// The attributes a Secret Service item is found by, as `name=value`.
    fn item(&self) -> String {
        self.attributes
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
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
            "keyring" => match stores::keyring_holds(&self.key) {
                Ok(true) => {
                    Probe::Present(format!("the kernel keyring holds the key {}", self.key))
                }
                Ok(false) => Probe::Absent(format!("the kernel keyring holds no key {}", self.key)),
                Err(e) => Probe::from_error(e),
            },
            "systemd-creds" => match stores::credential_path(&self.key) {
                Ok(path) if path.is_file() => {
                    Probe::Present(format!("this unit was given the credential {}", self.key))
                }
                Ok(_) => Probe::Absent(format!("this unit was given no credential {}", self.key)),
                Err(e) => Probe::from_error(e),
            },
            "secret-service" => match stores::secret_service_unavailable() {
                Some(e) => Probe::from_error(e),
                None if on_path(stores::SECRET_TOOL) => Probe::Unknown(format!(
                    "the Secret Service is only asked by plan and apply: asking can open a \
                     prompt to unlock it (item {})",
                    self.item()
                )),
                None => Probe::Unavailable(format!(
                    "the Secret Service is read with {}, which was not found",
                    stores::SECRET_TOOL
                )),
            },
            "keychain" if cfg!(target_os = "macos") => Probe::Unknown(format!(
                "the keychain is only asked by plan and apply: asking can open a prompt \
                 (service {}, account {})",
                self.service, self.account
            )),
            "keychain" => Probe::Unavailable("the keychain is only on macOS".to_owned()),
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
            "keyring" => stores::from_keyring(&self.key),
            "systemd-creds" => {
                stores::from_systemd_credential(&self.key, self.keep_trailing_newline)
            }
            "secret-service" => stores::from_secret_service(&self.attributes),
            "keychain" => stores::from_keychain(&self.service, &self.account),
            other => Err(FetchError::Failed(format!(
                "{other:?} is not a provider this version has; from is one of {}",
                PROVIDERS.join(", ")
            ))),
        }
    }

    /// Whether Meridian can put a value in this place itself. The others
    /// are written by their own tools.
    pub fn writable(&self) -> bool {
        matches!(self.from.as_str(), "keyring" | "secret-service")
    }

    /// Stores `value` in this place, under `label` where the store shows
    /// one. Only a [writable](Source::writable) place takes a value. The
    /// error says why not, and never holds any part of the value: it is
    /// [`FetchError::Unavailable`] when the store cannot be used on this
    /// machine, so that the next place can be tried.
    pub fn store(&self, label: &str, value: &SecretValue) -> Result<(), FetchError> {
        match self.from.as_str() {
            "keyring" => stores::into_keyring(&self.key, value),
            "secret-service" => stores::into_secret_service(label, &self.attributes, value),
            other => Err(FetchError::Unavailable(format!(
                "from: {other} is not a place meridian writes; it is written with its own tool"
            ))),
        }
    }
}

/// Stores `value` in the first of `sources` that meridian can write and
/// that can be used on this machine, and says which it was.
///
/// A store that cannot be used here is passed over for the next; one that
/// refuses stops the search. When none takes it, the error gives every
/// writable place's reason, or says that no place is writable.
pub fn store_first(
    sources: &[Source],
    label: &str,
    value: &SecretValue,
) -> Result<usize, FetchError> {
    let mut reasons = Vec::new();
    for (at, source) in sources.iter().enumerate() {
        if !source.writable() {
            continue;
        }
        match source.store(label, value) {
            Ok(()) => return Ok(at),
            Err(FetchError::Unavailable(why)) => reasons.push(why),
            Err(refused) => return Err(refused),
        }
    }
    Err(FetchError::Unavailable(if reasons.is_empty() {
        "none of its places is one meridian writes".to_owned()
    } else {
        reasons.join("; ")
    }))
}

/// Fetches from the first of `sources` that has the secret, and says which
/// it was.
///
/// A place that has nothing, or that cannot be used on this machine, is
/// passed over for the next. A place that is there and refused stops the
/// search: that is a mistake to fix, not one to hide behind the next place.
/// When none answers, the error gives every place's reason, and is
/// [`FetchError::Missing`] when at least one place could have held it.
pub fn fetch_first(sources: &[Source]) -> Result<(usize, SecretValue), FetchError> {
    let mut reasons = Vec::new();
    let mut could_hold = false;
    for (at, source) in sources.iter().enumerate() {
        match source.fetch() {
            Ok(value) => return Ok((at, value)),
            Err(FetchError::Missing(why)) => {
                could_hold = true;
                reasons.push(why);
            }
            Err(FetchError::Unavailable(why)) => reasons.push(why),
            Err(failed) => return Err(failed),
        }
    }
    let why = reasons.join("; ");
    Err(if could_hold {
        FetchError::Missing(why)
    } else {
        FetchError::Unavailable(why)
    })
}

/// What [`fetch_first`] would find, without reading a value: the first
/// place that holds something or can only be asked by a fetch, or the place
/// that would be refused, or every place's reason for having nothing.
pub fn probe_first(sources: &[Source]) -> (usize, Probe) {
    let mut reasons = Vec::new();
    let mut could_hold = false;
    for (at, source) in sources.iter().enumerate() {
        match source.probe() {
            Probe::Absent(why) => {
                could_hold = true;
                reasons.push(why);
            }
            Probe::Unavailable(why) => reasons.push(why),
            answer => return (at, answer),
        }
    }
    let why = reasons.join("; ");
    let last = sources.len().saturating_sub(1);
    if could_hold {
        (last, Probe::Absent(why))
    } else {
        (last, Probe::Unavailable(why))
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
    /// The provider cannot be used on this machine at all.
    Unavailable(String),
}

impl Probe {
    /// What a failed look at a place amounts to.
    fn from_error(error: FetchError) -> Probe {
        match error {
            FetchError::Missing(why) => Probe::Absent(why),
            FetchError::Failed(why) => Probe::Refused(why),
            FetchError::Unavailable(why) => Probe::Unavailable(why),
        }
    }

    /// What was found, as a sentence without its full stop.
    pub fn words(&self) -> &str {
        match self {
            Probe::Present(w)
            | Probe::Absent(w)
            | Probe::Refused(w)
            | Probe::Unknown(w)
            | Probe::Unavailable(w) => w,
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
