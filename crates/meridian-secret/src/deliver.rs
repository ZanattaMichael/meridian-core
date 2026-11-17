//! Handing a secret to a tool that can only read a file.
//!
//! The first choice is always the tool's standard input or a pipe, which
//! needs nothing from this module: `secret.expose(|bytes| pipe.write_all(bytes))`.
//! A [`SecretFile`] is for the tools that have no such way in. It is written
//! once, here, so that its mode, its directory, its wiping and its removal
//! are not rewritten, slightly differently, by every plugin.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::{sys, SecretValue};

/// The prefix of every directory this module makes, followed by the id of
/// the process that made it.
const PREFIX: &str = "meridian-secrets-";

/// A file holding secret material for one run of one tool.
///
/// It lives in a directory only the user can enter, on a filesystem held in
/// memory where the system has one, and it can be read only by the user.
/// When it is dropped it is overwritten, removed, and its directory with it.
#[derive(Debug)]
pub struct SecretFile {
    dir: PathBuf,
    path: PathBuf,
    in_memory: bool,
}

/// Where a secret file's directory can go, best first, with whether the
/// place is held in memory.
fn candidates() -> Vec<(PathBuf, bool)> {
    let mut found = Vec::new();
    if cfg!(unix) {
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            found.push((PathBuf::from(runtime), true));
        }
        found.push((PathBuf::from(format!("/run/user/{}", sys::user_id())), true));
        found.push((PathBuf::from("/dev/shm"), true));
    }
    found.push((std::env::temp_dir(), false));
    found
}

/// Sixteen random bytes as hex, for a name that cannot be guessed.
fn random_name() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    #[cfg(unix)]
    {
        use std::io::Read;
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    }
    #[cfg(not(unix))]
    {
        // Not a secret: only a name that will not collide. The directory is
        // still created exclusively.
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        for chunk in bytes.chunks_mut(8) {
            let n = RandomState::new().build_hasher().finish().to_le_bytes();
            chunk.copy_from_slice(&n[..chunk.len()]);
        }
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    // Only Unix has a mode to set on it.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    // `create_new` refuses a path that already exists, a symlink included,
    // so nothing planted there beforehand is ever written through.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

impl SecretFile {
    /// Writes `parts` one after another into a new private file named
    /// `name`. A part is literal text or a secret, so that a file with a
    /// secret inside it (a variables file, a kubeconfig) is put together in
    /// the file and nowhere else.
    pub fn create(name: &str, parts: &[Part<'_>]) -> std::io::Result<SecretFile> {
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a secret file's name is a file name, not a path",
            ));
        }
        let unique = format!("{PREFIX}{}-{}", std::process::id(), random_name()?);
        let mut last_error = None;
        for (root, in_memory) in candidates() {
            if !root.is_dir() {
                continue;
            }
            let dir = root.join(&unique);
            match private_dir(&dir) {
                Ok(()) => {}
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            }
            // From here the guard removes whatever was made, on any path out.
            let file = SecretFile {
                path: dir.join(name),
                dir,
                in_memory,
            };
            let mut handle = private_file(&file.path)?;
            for part in parts {
                match part {
                    Part::Text(text) => handle.write_all(text.as_bytes())?,
                    Part::Secret(secret) => secret.expose(|bytes| handle.write_all(bytes))?,
                }
            }
            handle.flush()?;
            return Ok(file);
        }
        Err(last_error.unwrap_or_else(|| {
            std::io::Error::other(
                "there is nowhere to write a secret file: no temporary directory exists",
            )
        }))
    }

    /// The file's path, to give to the tool.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file is on a filesystem held in memory. When it is not,
    /// the secret touched a disk for the length of the run, which is worth
    /// telling the user.
    pub fn in_memory(&self) -> bool {
        self.in_memory
    }
}

/// One piece of a [`SecretFile`].
#[derive(Debug)]
pub enum Part<'a> {
    Text(&'a str),
    Secret(&'a SecretValue),
}

/// Overwrites a file's contents with zeroes, as far as the filesystem lets
/// a write land on the same blocks.
fn overwrite(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let zeroes = [0u8; 4096];
    let mut left = len;
    while left > 0 {
        let n = usize::try_from(left)
            .unwrap_or(usize::MAX)
            .min(zeroes.len());
        file.write_all(&zeroes[..n])?;
        left -= n as u64;
    }
    file.sync_all()
}

fn remove(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Never follow a link out of the directory.
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = overwrite(&path);
            }
            let _ = fs::remove_file(&path);
        }
    }
    let _ = fs::remove_dir(dir);
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        remove(&self.dir);
    }
}

/// Removes what an earlier run left behind when it was killed before it
/// could clean up: every secret directory of this user whose process no
/// longer exists. Returns how many were removed.
pub fn sweep_stale() -> usize {
    let mut removed = 0;
    for (root, _) in candidates() {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(PREFIX)) else {
                continue;
            };
            let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
                continue;
            };
            if pid == std::process::id() || sys::process_exists(pid) {
                continue;
            }
            // Only a real directory that this user owns outright.
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.is_dir() || !owned_by_user(&meta) {
                continue;
            }
            remove(&entry.path());
            removed += 1;
        }
    }
    removed
}

#[cfg(unix)]
fn owned_by_user(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.uid() == sys::user_id()
}

#[cfg(not(unix))]
fn owned_by_user(_meta: &fs::Metadata) -> bool {
    true
}
