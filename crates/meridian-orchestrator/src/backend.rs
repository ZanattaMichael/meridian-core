//! Where Meridian's own record lives.
//!
//! The record is small and asks little of a store: named blobs that can be
//! read, written whole, removed and listed, and one operation that is atomic,
//! creating a blob only if it is not there. The lock is built from that last
//! one. Anything that can do those five things can hold a team's runs.
//!
//! `local` is files under the state directory. Any other backend is a
//! program named `meridian-backend-<name>`, run once per operation, so a
//! shell script around a cloud CLI can be one.
//!
//! A backend never holds a tool's working directory, and nothing written to
//! one holds a secret value: see `docs/reference/state.md`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// The naming convention a backend program follows, as a target's follows
/// `meridian-target-`.
pub const BACKEND_PREFIX: &str = "meridian-backend-";

/// The exit status with which a backend program says a key is not there.
pub const EXIT_NOT_FOUND: i32 = 3;
/// The exit status with which a backend program says `create` found the key
/// already there.
pub const EXIT_EXISTS: i32 = 4;

/// A store of named blobs.
///
/// Keys are relative paths with `/` between their parts, such as
/// `runs/2026-11-17T09-14-03Z-7f3a/run.json`. An error is text for the
/// operator: what was being done, and what the store said.
pub trait Backend: Send + Sync + fmt::Debug {
    /// What to call this store when talking to a person.
    fn describe(&self) -> String;
    /// The blob, or `None` when there is none.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    /// Stores the blob, replacing any other whole. A reader sees the old
    /// blob or the new one, never part of each.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String>;
    /// Stores the blob only if the key is not there, and says whether it
    /// did. This must be atomic: of two callers, exactly one is told `true`.
    fn create(&self, key: &str, bytes: &[u8]) -> Result<bool, String>;
    /// Removes the blob. Removing one that is not there is not an error.
    fn delete(&self, key: &str) -> Result<(), String>;
    /// Every key that starts with `prefix`, sorted.
    fn list(&self, prefix: &str) -> Result<Vec<String>, String>;
    /// Adds bytes to the end of the blob, making it if there is none. A
    /// crash may leave part of the added bytes; a reader of a log of lines
    /// skips a line it cannot read. A store with no append of its own reads
    /// the blob and puts it back whole.
    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let mut whole = self.get(key)?.unwrap_or_default();
        whole.extend_from_slice(bytes);
        self.put(key, &whole)
    }
}

/// Whether `key` is one a backend should accept: relative, with no part that
/// climbs out of the store.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.ends_with('/')
        && Path::new(key)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn check(key: &str) -> Result<(), String> {
    if valid_key(key) {
        Ok(())
    } else {
        Err(format!("{key:?} is not a key a state backend accepts"))
    }
}

/// Files under a directory. This is what a run uses unless told otherwise.
#[derive(Clone, Debug)]
pub struct LocalBackend {
    dir: PathBuf,
}

impl LocalBackend {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        LocalBackend { dir: dir.into() }
    }

    fn path(&self, key: &str) -> Result<PathBuf, String> {
        check(key)?;
        Ok(self.dir.join(key))
    }

    fn open_private(path: &Path, create_new: bool) -> std::io::Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true);
        if create_new {
            options.create_new(true);
        } else {
            options.create(true).truncate(true);
        }
        Self::private(options, path)
    }

    fn private(mut options: std::fs::OpenOptions, path: &Path) -> std::io::Result<std::fs::File> {
        // Nothing in the record is secret. Keeping it private anyway costs
        // nothing, and means a mistake elsewhere is not also a leak.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path)
    }
}

impl Backend for LocalBackend {
    fn describe(&self) -> String {
        format!("the state directory {}", self.dir.display())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("reading {}: {e}", path.display())),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.path(key)?;
        let wrote = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Written beside the file and renamed over it, so a crash never
            // leaves half of one.
            let mut staged = path.clone().into_os_string();
            staged.push(".tmp");
            let staged = PathBuf::from(staged);
            let mut file = Self::open_private(&staged, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&staged, &path)
        })();
        wrote.map_err(|e| format!("writing {}: {e}", path.display()))
    }

    fn create(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        let path = self.path(key)?;
        let created = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = match Self::open_private(&path, true) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
                Err(e) => return Err(e),
            };
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(true)
        })();
        created.map_err(|e: std::io::Error| format!("creating {}: {e}", path.display()))
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let path = self.path(key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("removing {}: {e}", path.display())),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        // Only the directory the prefix names is walked, so listing the
        // runs never wanders through a tool's working directory.
        let (dir, _) = prefix.rsplit_once('/').unwrap_or(("", prefix));
        if !dir.is_empty() {
            check(dir)?;
        }
        let mut keys = Vec::new();
        let mut pending = vec![dir.to_owned()];
        while let Some(relative) = pending.pop() {
            let entries = match std::fs::read_dir(self.dir.join(&relative)) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("listing {}: {e}", self.dir.display())),
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let key = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => pending.push(key),
                    Ok(kind) if kind.is_file() && !key.ends_with(".tmp") => keys.push(key),
                    _ => {}
                }
            }
        }
        keys.retain(|key| key.starts_with(prefix));
        keys.sort();
        Ok(keys)
    }

    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.path(key)?;
        let wrote = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.append(true).create(true);
            let mut file = Self::private(options, &path)?;
            file.write_all(bytes)?;
            file.sync_data()
        })();
        wrote.map_err(|e| format!("writing {}: {e}", path.display()))
    }
}

/// A store in memory, for tests and for a caller that wants a run recorded
/// nowhere.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    blobs: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        MemoryBackend::default()
    }

    fn blobs(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.blobs.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Backend for MemoryBackend {
    fn describe(&self) -> String {
        "memory".to_owned()
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        check(key)?;
        Ok(self.blobs().get(key).cloned())
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        check(key)?;
        self.blobs().insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn create(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        check(key)?;
        let mut blobs = self.blobs();
        if blobs.contains_key(key) {
            return Ok(false);
        }
        blobs.insert(key.to_owned(), bytes.to_vec());
        Ok(true)
    }

    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        check(key)?;
        self.blobs()
            .entry(key.to_owned())
            .or_default()
            .extend_from_slice(bytes);
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        check(key)?;
        self.blobs().remove(key);
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        Ok(self
            .blobs()
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect())
    }
}

/// A store behind a program named `meridian-backend-<name>`.
///
/// The program is run once per operation:
///
/// | Arguments | Standard input | Standard output | Exit status |
/// |---|---|---|---|
/// | `get <key>` | | the blob | 0, or 3 when there is none |
/// | `put <key>` | the blob | | 0 |
/// | `create <key>` | the blob | | 0, or 4 when the key is there |
/// | `delete <key>` | | | 0, also when there was none |
/// | `list <prefix>` | | one key per line | 0 |
///
/// Any other status is a failure, and what the program wrote to standard
/// error is the reason. Its settings arrive as `MERIDIAN_BACKEND_<SETTING>`
/// environment variables.
#[derive(Clone)]
pub struct ExecBackend {
    name: String,
    program: PathBuf,
    settings: BTreeMap<String, String>,
}

/// A setting's value can be a credential, so `{:?}` shows only its name.
impl fmt::Debug for ExecBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecBackend")
            .field("name", &self.name)
            .field("program", &self.program)
            .field("settings", &self.settings.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ExecBackend {
    /// A backend run from `program`.
    pub fn new(
        name: impl Into<String>,
        program: impl Into<PathBuf>,
        settings: BTreeMap<String, String>,
    ) -> Self {
        ExecBackend {
            name: name.into(),
            program: program.into(),
            settings,
        }
    }

    /// Finds `meridian-backend-<name>` in `dirs`, the directories plugins
    /// are looked for in.
    pub fn find(
        name: &str,
        dirs: &[PathBuf],
        settings: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let safe = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe {
            return Err(format!("{name:?} is not a state backend name"));
        }
        let file = format!("{BACKEND_PREFIX}{name}");
        for dir in dirs {
            for candidate in [dir.join(&file), dir.join(format!("{file}.exe"))] {
                if candidate.is_file() {
                    return Ok(ExecBackend::new(name, candidate, settings));
                }
            }
        }
        let searched: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        Err(format!(
            "no state backend {name:?}: there is no program named {file} in {}",
            if searched.is_empty() {
                "any plugin directory".to_owned()
            } else {
                searched.join(", ")
            }
        ))
    }

    /// The environment variable a setting arrives in.
    pub fn setting_variable(setting: &str) -> String {
        let name: String = setting
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("MERIDIAN_BACKEND_{name}")
    }

    /// Runs one operation, and returns the exit status and standard output.
    fn call(&self, verb: &str, key: &str, input: Option<&[u8]>) -> Result<(i32, Vec<u8>), String> {
        let failed = |what: &str| format!("state backend {:?}: {verb} {key}: {what}", self.name);
        let mut command = Command::new(&self.program);
        command
            .arg(verb)
            .arg(key)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (setting, value) in &self.settings {
            command.env(Self::setting_variable(setting), value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| failed(&format!("running {}: {e}", self.program.display())))?;
        if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
            // A program that exits without reading closes the pipe; its exit
            // status says why, so that is what gets reported.
            let _ = stdin.write_all(bytes);
        }
        let output = child
            .wait_with_output()
            .map_err(|e| failed(&e.to_string()))?;
        let Some(code) = output.status.code() else {
            return Err(failed("the program was killed"));
        };
        if code == 0 || code == EXIT_NOT_FOUND || code == EXIT_EXISTS {
            return Ok((code, output.stdout));
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        Err(failed(&if said.is_empty() {
            format!("exit status {code}")
        } else {
            format!("exit status {code}: {said}")
        }))
    }

    fn unexpected(&self, verb: &str, key: &str, code: i32) -> String {
        format!(
            "state backend {:?}: {verb} {key}: exit status {code} means nothing for {verb}",
            self.name
        )
    }
}

impl Backend for ExecBackend {
    fn describe(&self) -> String {
        format!("the state backend {:?}", self.name)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        check(key)?;
        match self.call("get", key, None)? {
            (0, bytes) => Ok(Some(bytes)),
            (EXIT_NOT_FOUND, _) => Ok(None),
            (code, _) => Err(self.unexpected("get", key, code)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        check(key)?;
        match self.call("put", key, Some(bytes))? {
            (0, _) => Ok(()),
            (code, _) => Err(self.unexpected("put", key, code)),
        }
    }

    fn create(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        check(key)?;
        match self.call("create", key, Some(bytes))? {
            (0, _) => Ok(true),
            (EXIT_EXISTS, _) => Ok(false),
            (code, _) => Err(self.unexpected("create", key, code)),
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        check(key)?;
        match self.call("delete", key, None)? {
            (0, _) | (EXIT_NOT_FOUND, _) => Ok(()),
            (code, _) => Err(self.unexpected("delete", key, code)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        match self.call("list", prefix, None)? {
            (0, bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                // What a program prints is not trusted to be a key.
                let mut keys: Vec<String> = text
                    .lines()
                    .map(str::trim)
                    .filter(|key| valid_key(key) && key.starts_with(prefix))
                    .map(str::to_owned)
                    .collect();
                keys.sort();
                keys.dedup();
                Ok(keys)
            }
            (EXIT_NOT_FOUND, _) => Ok(Vec::new()),
            (code, _) => Err(self.unexpected("list", prefix, code)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("meridian-backend-test-{}", std::process::id()))
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The contract every backend keeps, run against each of them.
    fn keeps_the_contract(backend: &dyn Backend) {
        assert_eq!(backend.get("state.json").unwrap(), None);
        backend.put("state.json", b"one").unwrap();
        backend.put("state.json", b"two").unwrap();
        assert_eq!(backend.get("state.json").unwrap().unwrap(), b"two");

        assert!(backend.create("lock", b"first").unwrap());
        assert!(!backend.create("lock", b"second").unwrap());
        assert_eq!(backend.get("lock").unwrap().unwrap(), b"first");
        backend.delete("lock").unwrap();
        backend.delete("lock").unwrap();
        assert!(backend.create("lock", b"third").unwrap());

        backend.put("runs/b/run.json", b"{}").unwrap();
        backend.put("runs/a/run.json", b"{}").unwrap();
        backend.put("runs/a/events.jsonl", b"").unwrap();
        assert_eq!(
            backend.list("runs/").unwrap(),
            ["runs/a/events.jsonl", "runs/a/run.json", "runs/b/run.json"]
        );
        assert_eq!(backend.list("runs/b/").unwrap(), ["runs/b/run.json"]);
        assert!(backend.list("nothing/").unwrap().is_empty());

        backend.append("runs/c/events.jsonl", b"one\n").unwrap();
        backend.append("runs/c/events.jsonl", b"two\n").unwrap();
        assert_eq!(
            backend.get("runs/c/events.jsonl").unwrap().unwrap(),
            b"one\ntwo\n"
        );

        for bad in ["", "../escape", "/etc/passwd", "runs/../../x", "runs/"] {
            assert!(backend.put(bad, b"x").is_err(), "{bad:?} was accepted");
            assert!(backend.get(bad).is_err(), "{bad:?} was accepted");
            assert!(backend.append(bad, b"x").is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn the_memory_backend_keeps_the_contract() {
        keeps_the_contract(&MemoryBackend::new());
    }

    #[test]
    fn the_local_backend_keeps_the_contract() {
        let dir = scratch("local");
        keeps_the_contract(&LocalBackend::new(&dir));
        // A tool's working directory beside the record is never listed.
        std::fs::create_dir_all(dir.join("infrastructure/net/fake")).unwrap();
        std::fs::write(dir.join("infrastructure/net/fake/state.json"), "x").unwrap();
        let backend = LocalBackend::new(&dir);
        assert!(backend
            .list("runs/")
            .unwrap()
            .iter()
            .all(|key| key.starts_with("runs/")));
    }

    #[cfg(unix)]
    #[test]
    fn the_local_backend_writes_private_files_and_leaves_no_staging_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("private");
        let backend = LocalBackend::new(&dir);
        backend.put("state.json", b"{}").unwrap();
        backend.create("lock", b"{}").unwrap();
        for file in ["state.json", "lock"] {
            let mode = std::fs::metadata(dir.join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
        assert!(!dir.join("state.json.tmp").exists());
    }

    #[test]
    fn a_setting_becomes_an_environment_variable() {
        assert_eq!(
            ExecBackend::setting_variable("bucket"),
            "MERIDIAN_BACKEND_BUCKET"
        );
        assert_eq!(
            ExecBackend::setting_variable("key-prefix"),
            "MERIDIAN_BACKEND_KEY_PREFIX"
        );
    }

    #[test]
    fn a_backend_that_is_not_installed_says_where_it_was_looked_for() {
        let dir = scratch("missing");
        std::fs::create_dir_all(&dir).unwrap();
        let err = ExecBackend::find("s3", std::slice::from_ref(&dir), BTreeMap::new()).unwrap_err();
        assert!(err.contains("meridian-backend-s3"), "{err}");
        assert!(err.contains(&dir.display().to_string()), "{err}");
        assert!(ExecBackend::find("../x", &[dir], BTreeMap::new()).is_err());
    }

    #[test]
    fn debug_never_shows_a_settings_value() {
        let backend = ExecBackend::new(
            "s3",
            "/bin/true",
            [("token".to_owned(), "hunter2".to_owned())].into(),
        );
        let shown = format!("{backend:?}");
        assert!(shown.contains("token"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}
