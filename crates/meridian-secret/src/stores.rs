//! The stores an operating system already protects, as providers: the Linux
//! kernel keyring, the credentials systemd passes to a unit, the freedesktop
//! Secret Service and the macOS keychain.
//!
//! Each reads straight into protected memory, as the built-in providers do,
//! and none puts anything it read into an error. A store that cannot be used
//! on this machine at all (another operating system, no session bus, its
//! tool not installed) says so as [`FetchError::Unavailable`], which is what
//! lets a secret name a second place to look.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use crate::provider::failed;
use crate::{FetchError, SecretValue, DEFAULT_MAX_LEN};

/// The environment variable systemd sets for a unit that was given
/// credentials: the directory they are in.
pub(crate) const CREDENTIALS_DIRECTORY: &str = "CREDENTIALS_DIRECTORY";
/// The environment variable that says where the session bus is.
pub(crate) const SESSION_BUS: &str = "DBUS_SESSION_BUS_ADDRESS";
/// The program that reads and writes the Secret Service.
pub(crate) const SECRET_TOOL: &str = "secret-tool";
/// The program that reads the macOS keychain.
pub(crate) const SECURITY: &str = "/usr/bin/security";

/// Runs a program without a shell and reads what it prints into protected
/// memory. Its standard error is discarded: a program that fails may print
/// the very thing it was asked for. `Err` is the program not starting.
pub(crate) fn run(
    program: &str,
    args: &[String],
) -> std::io::Result<(ExitStatus, std::io::Result<SecretValue>)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let read = match child.stdout.take() {
        Some(stdout) => SecretValue::from_reader(stdout, DEFAULT_MAX_LEN),
        None => Err(std::io::Error::other("its output could not be read")),
    };
    Ok((child.wait()?, read))
}

/// Reads a key of type `user` from the Linux kernel keyring: the session
/// keyring first, then the user keyring.
///
/// A key there never touches a disk and can be given a lifetime, which makes
/// it the right home for a value that should live for one login.
pub fn from_keyring(description: &str) -> Result<SecretValue, FetchError> {
    #[cfg(target_os = "linux")]
    {
        use crate::sys;
        let about = |e| failed(format_args!("the kernel keyring, key {description}"), e);
        let name = std::ffi::CString::new(description).map_err(|_| {
            FetchError::Failed("the key's description cannot hold a NUL character".to_owned())
        })?;
        let id = match sys::keyring_find(&name) {
            Ok(Some(id)) => id,
            Ok(None) => {
                return Err(FetchError::Missing(format!(
                    "the kernel keyring holds no key {description}"
                )))
            }
            Err(e) if keyring_is_shut(&e) => return Err(keyring_unavailable(&e)),
            Err(e) => return Err(about(e)),
        };
        // A key in the user keyring can be found by anyone running as the
        // user, and read only by a session that holds that keyring.
        let len = sys::keyring_len(id).map_err(|e| {
            if e.raw_os_error() == Some(libc::EACCES) {
                FetchError::Failed(format!(
                    "the kernel keyring holds the key {description}, and this session may \
                     not read it: the key is readable only by a possessor, and this \
                     session's keyring does not link the user keyring. Let the owner read \
                     it with: keyctl setperm $(keyctl search @u user {description}) \
                     0x3f3f0000, or store it with meridian secret set"
                ))
            } else {
                about(e)
            }
        })?;
        if len > DEFAULT_MAX_LEN {
            return Err(FetchError::Failed(format!(
                "the key {description} is larger than a secret can be ({} KiB)",
                DEFAULT_MAX_LEN / 1024
            )));
        }
        SecretValue::filled(len, |home| {
            if sys::keyring_read(id, home)? == home.len() {
                Ok(())
            } else {
                Err(std::io::Error::other("the key changed while it was read"))
            }
        })
        .map_err(about)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = description;
        Err(FetchError::Unavailable(
            "the kernel keyring is only on Linux".to_owned(),
        ))
    }
}

/// Whether the kernel refused the keyring itself, as a container's
/// sandbox does, and not this one key.
#[cfg(target_os = "linux")]
fn keyring_is_shut(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EPERM | libc::ENOSYS | libc::EACCES)
    )
}

#[cfg(target_os = "linux")]
fn keyring_unavailable(error: &std::io::Error) -> FetchError {
    FetchError::Unavailable(format!(
        "the kernel keyring cannot be used here ({error}); a container often shuts it off"
    ))
}

/// Whether the kernel keyring holds the key, without reading it.
pub(crate) fn keyring_holds(description: &str) -> Result<bool, FetchError> {
    #[cfg(target_os = "linux")]
    {
        let name = std::ffi::CString::new(description).map_err(|_| {
            FetchError::Failed("the key's description cannot hold a NUL character".to_owned())
        })?;
        match crate::sys::keyring_find(&name) {
            Ok(found) => Ok(found.is_some()),
            Err(e) if keyring_is_shut(&e) => Err(keyring_unavailable(&e)),
            Err(e) => Err(failed(
                format_args!("the kernel keyring, key {description}"),
                e,
            )),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = description;
        Err(FetchError::Unavailable(
            "the kernel keyring is only on Linux".to_owned(),
        ))
    }
}

/// Puts a secret in the user keyring of the Linux kernel keyring, replacing
/// a key of the same description.
pub(crate) fn into_keyring(description: &str, value: &SecretValue) -> Result<(), FetchError> {
    #[cfg(target_os = "linux")]
    {
        let name = std::ffi::CString::new(description).map_err(|_| {
            FetchError::Failed("the key's description cannot hold a NUL character".to_owned())
        })?;
        value
            .expose(|bytes| crate::sys::keyring_store(&name, bytes))
            .map_err(|e| {
                if keyring_is_shut(&e) {
                    keyring_unavailable(&e)
                } else {
                    failed(format_args!("the kernel keyring, key {description}"), e)
                }
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (description, value);
        Err(FetchError::Unavailable(
            "the kernel keyring is only on Linux".to_owned(),
        ))
    }
}

/// Where the credential `name` of this unit is, when the process was given
/// credentials at all.
pub(crate) fn credential_path(name: &str) -> Result<std::path::PathBuf, FetchError> {
    let Some(dir) = std::env::var_os(CREDENTIALS_DIRECTORY).filter(|d| !d.is_empty()) else {
        return Err(FetchError::Unavailable(format!(
            "no credentials were passed to this process: {CREDENTIALS_DIRECTORY} is not set. \
             systemd sets it for a unit with LoadCredential= or LoadCredentialEncrypted="
        )));
    };
    // A credential is a file directly in the directory, never a path.
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(FetchError::Failed(format!(
            "{name:?} is not the name of a credential: it is one name, not a path"
        )));
    }
    Ok(Path::new(&dir).join(name))
}

/// Reads a credential systemd passed to this unit (`LoadCredential=`,
/// `LoadCredentialEncrypted=`), from `$CREDENTIALS_DIRECTORY`.
///
/// systemd makes the directory readable by the unit alone and, for an
/// encrypted credential, decrypts it into memory that is never swapped, so
/// the file's mode is not checked here.
pub fn from_systemd_credential(
    name: &str,
    keep_trailing_newline: bool,
) -> Result<SecretValue, FetchError> {
    let path = credential_path(name)?;
    let about = |e| failed(format_args!("the credential {name}"), e);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FetchError::Missing(format!(
                "this unit was given no credential {name}"
            )))
        }
        Err(e) => return Err(about(e)),
    };
    let secret = SecretValue::from_reader(file, DEFAULT_MAX_LEN).map_err(about)?;
    if keep_trailing_newline {
        Ok(secret)
    } else {
        secret.without_trailing_newline().map_err(about)
    }
}

/// The arguments that name an item of the Secret Service: each attribute
/// and its value, in the attributes' order.
fn attribute_args(attributes: &BTreeMap<String, String>) -> Vec<String> {
    attributes
        .iter()
        .flat_map(|(name, value)| [name.clone(), value.clone()])
        .collect()
}

/// Why the Secret Service cannot be asked on this machine, when it cannot.
pub(crate) fn secret_service_unavailable() -> Option<FetchError> {
    // Without a session bus the tool waits for one to appear. Saying so
    // beats a run that hangs.
    if std::env::var_os(SESSION_BUS).is_none_or(|bus| bus.is_empty()) {
        return Some(FetchError::Unavailable(format!(
            "the Secret Service needs a session bus, and {SESSION_BUS} is not set; a server \
             or a pipeline usually has none"
        )));
    }
    None
}

fn secret_tool_missing() -> FetchError {
    FetchError::Unavailable(format!(
        "the Secret Service is read with {SECRET_TOOL}, which was not found; it is in the \
         package libsecret-tools (or libsecret)"
    ))
}

/// Reads an item from the freedesktop Secret Service (GNOME Keyring,
/// KWallet), found by its attributes, with `secret-tool`.
///
/// The attributes are how an item is found and are not secret: they are
/// stored unencrypted by the service itself.
pub fn from_secret_service(
    attributes: &BTreeMap<String, String>,
) -> Result<SecretValue, FetchError> {
    if let Some(unavailable) = secret_service_unavailable() {
        return Err(unavailable);
    }
    let mut args = vec!["lookup".to_owned()];
    args.extend(attribute_args(attributes));
    let (status, read) = match run(SECRET_TOOL, &args) {
        Ok(ran) => ran,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(secret_tool_missing()),
        Err(e) => return Err(failed("the Secret Service", e)),
    };
    let secret = read.map_err(|e| failed("the Secret Service", e))?;
    // The tool exits with a failure and prints nothing when no item has
    // these attributes.
    if !status.success() || secret.is_empty() {
        return Err(FetchError::Missing(
            "the Secret Service holds no item with these attributes".to_owned(),
        ));
    }
    Ok(secret)
}

/// Stores a secret in the Secret Service under these attributes, handing
/// the value to `secret-tool` on its standard input.
pub(crate) fn into_secret_service(
    label: &str,
    attributes: &BTreeMap<String, String>,
    value: &SecretValue,
) -> Result<(), FetchError> {
    if let Some(unavailable) = secret_service_unavailable() {
        return Err(unavailable);
    }
    let mut child = match Command::new(SECRET_TOOL)
        .arg("store")
        .arg(format!("--label={label}"))
        .args(attribute_args(attributes))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(secret_tool_missing()),
        Err(e) => return Err(failed("the Secret Service", e)),
    };
    let written = match child.stdin.take() {
        // The pipe is closed when `stdin` is dropped, which is what tells
        // the tool the value has ended.
        Some(mut stdin) => value.expose(|bytes| stdin.write_all(bytes)),
        None => Err(std::io::Error::other("its input could not be written")),
    };
    let status = child.wait().map_err(|e| failed("the Secret Service", e))?;
    written.map_err(|e| failed("the Secret Service", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(FetchError::Failed(
            "the Secret Service refused to store the item".to_owned(),
        ))
    }
}

/// Reads a generic password from the macOS keychain, by its service and
/// account, with `security`.
///
/// The keychain asks the user the first time a program reads an item it
/// did not make, and can be told to always allow it.
pub fn from_keychain(service: &str, account: &str) -> Result<SecretValue, FetchError> {
    if !cfg!(target_os = "macos") {
        return Err(FetchError::Unavailable(
            "the keychain is only on macOS".to_owned(),
        ));
    }
    let args = ["find-generic-password", "-s", service, "-a", account, "-w"].map(str::to_owned);
    let (status, read) = run(SECURITY, &args).map_err(|e| failed("the keychain", e))?;
    // 44 is the tool's "the specified item could not be found".
    if status.code() == Some(44) {
        return Err(FetchError::Missing(format!(
            "the keychain holds no item for service {service} and account {account}"
        )));
    }
    if !status.success() {
        let how = match status.code() {
            Some(code) => format!("status {code}"),
            None => "a signal".to_owned(),
        };
        return Err(FetchError::Failed(format!(
            "the keychain did not give the item for service {service} and account {account} \
             ({SECURITY} ended with {how}); it may be locked, or the read was not allowed"
        )));
    }
    read.and_then(SecretValue::without_trailing_newline)
        .map_err(|e| failed("the keychain", e))
}
