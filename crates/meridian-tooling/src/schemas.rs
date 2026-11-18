//! The cache of tool schemas that `meridian schema sync` writes and the
//! editor tools read.
//!
//! A target's plugin can ask its installed tool for the schemas of the
//! tool's own resource types (`aws_instance`, `apps/v1/Deployment`), but an
//! editor must not run tools, and the tool may need the network or a cloud
//! account. So the sync runs once, by hand or in CI, and writes what it got
//! under the project's state directory, one file per target:
//!
//! ```text
//! .meridian/schemas/terraform.json
//! .meridian/schemas/kubernetes.json
//! ```
//!
//! Each file says which tool and which versions it was read from, and
//! `meridian.lock` records the same, with the file's hash. The editor reads
//! the files and tells the author when they are missing or no longer match
//! the lock. Nothing in a cache is a rule: it is the tool's own description
//! of its types, offered as advice until the tool itself judges a compile.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use meridian_module::{Lock, LockedSchemas};
use meridian_sdk::{ResourceSchema, SchemaExport};
use sha2::{Digest, Sha256};

/// The directory under the state directory that holds the cache.
pub const CACHE_DIR: &str = "schemas";
/// Where a target's tool may keep what it downloads between syncs.
pub const WORK_DIR: &str = "work";
/// The format this build writes. A newer build reads an older file; an
/// older build ignores a newer one rather than misread it.
pub const CACHE_VERSION: u32 = 1;

/// What is cached for one target.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cached {
    /// The cache format, [`CACHE_VERSION`].
    pub cache_version: u32,
    /// The target the schemas belong to.
    pub target: String,
    /// The tool that produced them, and its version.
    pub tool: String,
    pub tool_version: String,
    /// The exact versions of the parts the schemas were read from.
    pub pins: BTreeMap<String, String>,
    /// When the sync ran, as `YYYY-MM-DDTHH:MM:SSZ`.
    pub synced_at: String,
    /// What the plugin wanted the operator to know.
    pub notes: Vec<String>,
    /// One per resource type, sorted by type.
    pub schemas: Vec<ResourceSchema>,
    /// The schema bundles the cache was made from, as
    /// `<target>/<name>:<version>`; empty when the tool wrote it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bundles: Vec<String>,
}

impl Cached {
    /// What a sync produced, as the cache holds it.
    pub fn from_export(target: &str, mut export: SchemaExport) -> Cached {
        export.tidy();
        Cached {
            cache_version: CACHE_VERSION,
            target: target.to_owned(),
            tool: export.tool,
            tool_version: export.tool_version,
            pins: export.pins,
            synced_at: now(),
            notes: export.notes,
            schemas: export.schemas,
            bundles: Vec::new(),
        }
    }

    /// The versions, as one line: `terraform 1.13.1; hashicorp/aws 5.60.0`.
    pub fn versions(&self) -> String {
        let mut parts = Vec::new();
        if !self.tool.is_empty() {
            parts.push(
                format!("{} {}", self.tool, self.tool_version)
                    .trim()
                    .to_owned(),
            );
        }
        for (name, version) in &self.pins {
            parts.push(format!("{name} {version}"));
        }
        parts.join("; ")
    }

    /// The lock entry that stands for this cache, given the file's hash.
    pub fn lock_entry(&self, integrity: &str) -> LockedSchemas {
        LockedSchemas {
            target: self.target.clone(),
            tool: self.tool.clone(),
            tool_version: self.tool_version.clone(),
            pins: self.pins.clone(),
            integrity: integrity.to_owned(),
            bundles: Vec::new(),
        }
    }
}

/// The cache under one state directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The cache under `state_dir`, usually `.meridian`.
    pub fn new(state_dir: &Path) -> Store {
        Store {
            dir: state_dir.join(CACHE_DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file one target's schemas are kept in.
    pub fn path(&self, target: &str) -> PathBuf {
        self.dir.join(format!("{}.json", safe_name(target)))
    }

    /// A directory one target's tool may use between syncs.
    pub fn work_dir(&self, target: &str) -> PathBuf {
        self.dir.join(WORK_DIR).join(safe_name(target))
    }

    /// Reads one target's cache. `None` when there is none; an error when
    /// there is a file that cannot be read.
    pub fn load(&self, target: &str) -> Result<Option<Cached>, String> {
        let path = self.path(target);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let cached: Cached =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if cached.cache_version > CACHE_VERSION {
            return Err(format!(
                "{}: written by a newer meridian (cache format {}); upgrade meridian or run \
                 `meridian schema sync` again",
                path.display(),
                cached.cache_version
            ));
        }
        Ok(Some(cached))
    }

    /// The hash of one target's cache file, as the lock records it.
    pub fn integrity_of(&self, target: &str) -> Option<String> {
        std::fs::read(self.path(target))
            .ok()
            .map(|bytes| integrity(&bytes))
    }

    /// Writes one target's cache and returns the file's hash.
    pub fn save(&self, cached: &Cached) -> Result<String, String> {
        let path = self.path(&cached.target);
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let mut text = serde_json::to_string_pretty(cached).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(integrity(text.as_bytes()))
    }

    /// Removes one target's cache. Nothing to remove is not an error.
    pub fn remove(&self, target: &str) -> Result<(), String> {
        let path = self.path(target);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Every target that has a cache, with what is cached. A file that
    /// cannot be read is reported in place of its contents.
    pub fn load_all(&self) -> BTreeMap<String, Result<Cached, String>> {
        let mut out = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let found = match self.load(stem) {
                Ok(Some(cached)) => Ok(cached),
                Ok(None) => continue,
                Err(e) => Err(e),
            };
            // The file name is the target's, made safe; the file itself
            // says the name.
            let name = match &found {
                Ok(cached) if !cached.target.is_empty() => cached.target.clone(),
                _ => stem.to_owned(),
            };
            out.insert(name, found);
        }
        out
    }
}

/// How a target's cache stands against the project's lockfile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// The cache is what the lock records.
    Current,
    /// There is a cache and the lock does not mention it: synced, never
    /// committed to the lock. Usable, but others will not get the same.
    Unrecorded,
    /// The lock records a cache that is not on disk: a fresh clone.
    Missing,
    /// The cache on disk is not the one the lock records: another version
    /// was synced, or the file was changed.
    Differs,
}

impl Freshness {
    /// Compares what is on disk with what the lock says.
    pub fn of(
        cached: Option<&Cached>,
        integrity: Option<&str>,
        locked: Option<&LockedSchemas>,
    ) -> Freshness {
        match (cached, locked) {
            (None, None) | (None, Some(_)) if locked.is_none() => Freshness::Current,
            (None, Some(_)) => Freshness::Missing,
            (Some(_), None) => Freshness::Unrecorded,
            (Some(_), Some(locked)) => {
                if integrity == Some(locked.integrity.as_str()) {
                    Freshness::Current
                } else {
                    Freshness::Differs
                }
            }
            (None, None) => Freshness::Current,
        }
    }

    /// One line for a person.
    pub fn describe(&self) -> &'static str {
        match self {
            Freshness::Current => "current",
            Freshness::Unrecorded => "not recorded in meridian.lock",
            Freshness::Missing => {
                "recorded in meridian.lock but not on disk; run `meridian schema sync`"
            }
            Freshness::Differs => {
                "differs from what meridian.lock records; run `meridian schema sync`"
            }
        }
    }
}

/// Reads the lockfile beside `project_dir`, if there is one that parses.
pub fn lock_in(project_dir: &Path) -> Option<Lock> {
    Lock::load(project_dir).ok().flatten()
}

/// `h1:` and the SHA-256 of the bytes in hex, the form `meridian.lock`
/// records hashes in.
pub fn integrity(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(3 + digest.len() * 2);
    hex.push_str("h1:");
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// A target name as a file name: letters, digits, `-`, `_` and `.`; the
/// rest becomes `_`.
fn safe_name(target: &str) -> String {
    let mut out: String = target
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.starts_with('.') {
        out.insert(0, '_');
    }
    out
}

/// The current time as `YYYY-MM-DDTHH:MM:SSZ`, worked out by hand so the
/// tools carry no calendar dependency.
fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rest = secs % 86_400;
    let (h, m, s) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{ParamKind, ParamSchema};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meridian-schemas-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_cache_is_written_read_back_and_hashed() {
        let state = temp_dir("roundtrip");
        let store = Store::new(&state);
        let mut export = SchemaExport::new("terraform", "1.13.1");
        export.pin("registry.terraform.io/hashicorp/aws", "5.60.0");
        export.schemas.push(
            ResourceSchema::open("aws_instance", "An instance.")
                .param(ParamSchema::new("ami", ParamKind::String, "").required())
                .from_source("hashicorp/aws 5.60.0"),
        );
        export
            .schemas
            .push(ResourceSchema::open("<provider>_<type>", "a pattern"));
        let cached = Cached::from_export("terraform", export);
        assert_eq!(cached.schemas.len(), 1, "patterns are not cached");
        assert!(cached.synced_at.ends_with('Z'));
        let hash = store.save(&cached).unwrap();
        assert!(hash.starts_with("h1:") && hash.len() == 67);
        assert_eq!(
            store.integrity_of("terraform").as_deref(),
            Some(hash.as_str())
        );
        let read = store.load("terraform").unwrap().unwrap();
        assert_eq!(read, cached);
        assert_eq!(
            read.versions(),
            "terraform 1.13.1; registry.terraform.io/hashicorp/aws 5.60.0"
        );
        assert_eq!(store.load("nothing").unwrap(), None);
        let all = store.load_all();
        assert_eq!(all.len(), 1);
        assert!(all["terraform"].is_ok());

        // Freshness against a lock.
        let entry = cached.lock_entry(&hash);
        assert_eq!(
            Freshness::of(Some(&cached), Some(&hash), Some(&entry)),
            Freshness::Current
        );
        assert_eq!(
            Freshness::of(Some(&cached), Some(&hash), None),
            Freshness::Unrecorded
        );
        assert_eq!(Freshness::of(None, None, Some(&entry)), Freshness::Missing);
        assert_eq!(
            Freshness::of(Some(&cached), Some("h1:00"), Some(&entry)),
            Freshness::Differs
        );
        assert_eq!(Freshness::of(None, None, None), Freshness::Current);

        store.remove("terraform").unwrap();
        store.remove("terraform").unwrap();
        assert_eq!(store.load("terraform").unwrap(), None);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_newer_cache_format_is_refused_and_a_broken_file_reported() {
        let state = temp_dir("versions");
        let store = Store::new(&state);
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.path("t"), r#"{"cacheVersion": 99, "target": "t"}"#).unwrap();
        assert!(store.load("t").unwrap_err().contains("newer meridian"));
        std::fs::write(store.path("u"), "not json").unwrap();
        assert!(store.load("u").is_err());
        let all = store.load_all();
        assert!(all["t"].is_err() && all["u"].is_err());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn names_and_times_are_written_safely() {
        assert_eq!(safe_name("terraform"), "terraform");
        assert_eq!(safe_name("my target/x"), "my_target_x");
        assert_eq!(safe_name(".."), "_..");
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
    }
}
