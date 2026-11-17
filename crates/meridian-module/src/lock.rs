//! `meridian.lock`: exactly which version of each module a project uses, and
//! the hash its content must have. It also records which versions the
//! cached tool schemas (`meridian schema sync`) were read from, so a cache
//! that was synced from other versions is told apart from the one the
//! project pinned.

use std::collections::BTreeMap;
use std::path::Path;

use meridian_ir::Value;

use crate::manifest::valid_name;
use crate::project::normalize;
use crate::Error;

/// The lockfile's name, beside the manifest.
pub const LOCK_FILE: &str = "meridian.lock";

/// The lockfile format this build writes. A newer build reads an older
/// lockfile; an older build refuses a newer one instead of guessing at it.
pub const LOCK_VERSION: u64 = 1;

/// One pinned module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Locked {
    pub name: String,
    /// Empty when the module declares no version, which only a `path` module
    /// or a git `ref` may do.
    pub version: String,
    /// The source's locator: `git+…`, `oci+…` or `path+…`.
    pub source: String,
    /// Exactly what was fetched: a commit, or a manifest digest. Empty for a
    /// `path` module.
    pub resolved: String,
    /// The content hash. Empty for a `path` module, which is a working copy.
    pub integrity: String,
    /// The modules this one requires, by name, sorted.
    pub requires: Vec<String>,
}

/// The versions one target's cached tool schemas were read from, as
/// `meridian schema sync` recorded them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockedSchemas {
    /// The target name, such as `terraform`.
    pub target: String,
    /// The tool that produced the schemas, and its version.
    pub tool: String,
    pub tool_version: String,
    /// The exact versions of the parts the schemas were read from: a
    /// provider, a package, an API server, keyed by the tool's own name.
    pub pins: BTreeMap<String, String>,
    /// The hash of the cache file, `h1:` and SHA-256 in hex.
    pub integrity: String,
}

/// A parsed lockfile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lock {
    /// Sorted by name.
    pub modules: Vec<Locked>,
    /// Sorted by target.
    pub schemas: Vec<LockedSchemas>,
}

/// Quotes a string for YAML when leaving it bare could change what it is.
fn scalar(s: &str) -> String {
    // Starting with a letter rules out everything YAML would read as a
    // number, a list item or an alias.
    let plain = s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'/' | b'_' | b'-' | b'+' | b':' | b'@')
        })
        && !s.ends_with(':')
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        );
    if plain {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

impl Lock {
    pub fn get(&self, name: &str) -> Option<&Locked> {
        self.modules.iter().find(|m| m.name == name)
    }

    /// What was recorded for a target's cached schemas, if anything.
    pub fn schemas_for(&self, target: &str) -> Option<&LockedSchemas> {
        self.schemas.iter().find(|s| s.target == target)
    }

    /// Records a target's cached schemas, replacing an earlier record.
    pub fn set_schemas(&mut self, entry: LockedSchemas) {
        self.schemas.retain(|s| s.target != entry.target);
        self.schemas.push(entry);
        self.schemas.sort_by(|a, b| a.target.cmp(&b.target));
    }

    /// Forgets a target's cached schemas.
    pub fn remove_schemas(&mut self, target: &str) {
        self.schemas.retain(|s| s.target != target);
    }

    /// Whether there is nothing to write down.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty() && self.schemas.is_empty()
    }

    /// The lockfile's text. The same modules always render to the same
    /// bytes: sorted, with no timestamps, so a diff shows only what moved.
    pub fn render(&self) -> String {
        let mut modules = self.modules.clone();
        modules.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = format!(
            "# Written by `meridian module`. Commit it; do not edit it.\nlockVersion: {LOCK_VERSION}\n"
        );
        if modules.is_empty() {
            out.push_str("modules: []\n");
            self.render_schemas(&mut out);
            return out;
        }
        out.push_str("modules:\n");
        for m in &modules {
            out.push_str(&format!("  - name: {}\n", scalar(&m.name)));
            if !m.version.is_empty() {
                out.push_str(&format!("    version: {}\n", scalar(&m.version)));
            }
            out.push_str(&format!("    source: {}\n", scalar(&m.source)));
            if !m.resolved.is_empty() {
                out.push_str(&format!("    resolved: {}\n", scalar(&m.resolved)));
            }
            if !m.integrity.is_empty() {
                out.push_str(&format!("    integrity: {}\n", scalar(&m.integrity)));
            }
            if !m.requires.is_empty() {
                let mut requires = m.requires.clone();
                requires.sort();
                let list: Vec<String> = requires.iter().map(|r| scalar(r)).collect();
                out.push_str(&format!("    requires: [{}]\n", list.join(", ")));
            }
        }
        self.render_schemas(&mut out);
        out
    }

    /// The `schemas:` section, left out when there is none.
    fn render_schemas(&self, out: &mut String) {
        let mut schemas = self.schemas.clone();
        schemas.sort_by(|a, b| a.target.cmp(&b.target));
        if schemas.is_empty() {
            return;
        }
        out.push_str("schemas:\n");
        for s in &schemas {
            out.push_str(&format!("  - target: {}\n", scalar(&s.target)));
            if !s.tool.is_empty() {
                out.push_str(&format!("    tool: {}\n", scalar(&s.tool)));
            }
            if !s.tool_version.is_empty() {
                out.push_str(&format!("    toolVersion: {}\n", scalar(&s.tool_version)));
            }
            if !s.pins.is_empty() {
                out.push_str("    pins:\n");
                for (name, version) in &s.pins {
                    out.push_str(&format!("      {}: {}\n", scalar(name), scalar(version)));
                }
            }
            if !s.integrity.is_empty() {
                out.push_str(&format!("    integrity: {}\n", scalar(&s.integrity)));
            }
        }
    }

    /// Reads the lockfile in `dir`; `None` when there is none.
    pub fn load(dir: &Path) -> Result<Option<Lock>, Error> {
        let path = dir.join(LOCK_FILE);
        let name = path.display().to_string();
        match std::fs::read(&path) {
            Ok(data) => Lock::parse(&name, &data).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::new(format!("{name}: cannot read file: {e}"))),
        }
    }

    /// Parses a lockfile. `file` is used only in messages.
    pub fn parse(file: &str, data: &[u8]) -> Result<Lock, Error> {
        let bad = |msg: String| Error::new(format!("{file}: {msg}"));
        let value = meridian_ir::parse_value(file, data).map_err(|e| Error::new(e.to_string()))?;
        let Some(top) = value.as_map() else {
            return Err(bad(
                "the lockfile is not a mapping; run `meridian module install` to write it again"
                    .into(),
            ));
        };
        match top.get("lockVersion") {
            Some(Value::Int(v)) if *v >= 1 && (*v as u64) <= LOCK_VERSION => {}
            Some(Value::Int(v)) if *v >= 1 => {
                return Err(bad(format!(
                    "the lockfile is format {v}, and this build reads up to {LOCK_VERSION}; it \
                     was written by a newer meridian. Upgrade meridian"
                )))
            }
            _ => return Err(bad("lockVersion: expected a whole number".into())),
        }
        let mut lock = Lock::default();
        let items = match top.get("modules") {
            None | Some(Value::Null) => &[][..],
            Some(Value::List(items)) => items.as_slice(),
            Some(_) => return Err(bad("modules: expected a list".into())),
        };
        for (i, item) in items.iter().enumerate() {
            let Some(map) = item.as_map() else {
                return Err(bad(format!("modules[{i}]: expected a mapping")));
            };
            let text = |key: &str| -> Result<String, Error> {
                match map.get(key) {
                    None | Some(Value::Null) => Ok(String::new()),
                    Some(Value::String(s)) => Ok(s.clone()),
                    Some(_) => Err(bad(format!("modules[{i}].{key}: expected text"))),
                }
            };
            let mut requires = Vec::new();
            match map.get("requires") {
                None | Some(Value::Null) => {}
                Some(Value::List(names)) => {
                    for n in names {
                        match n.as_str() {
                            Some(n) => requires.push(n.to_owned()),
                            None => {
                                return Err(bad(format!("modules[{i}].requires: expected names")))
                            }
                        }
                    }
                }
                Some(_) => return Err(bad(format!("modules[{i}].requires: expected a list"))),
            }
            let locked = Locked {
                name: text("name")?,
                version: text("version")?,
                source: text("source")?,
                resolved: text("resolved")?,
                integrity: text("integrity")?,
                requires,
            };
            if locked.name.is_empty() || locked.source.is_empty() {
                return Err(bad(format!("modules[{i}]: needs a name and a source")));
            }
            // The lockfile is input, like the manifest and a fetched archive:
            // a project is cloned from elsewhere. `name` and `version` become
            // directory names under the state directory, so each is held to
            // what `meridian module install` would have written.
            if !valid_name(&locked.name) {
                return Err(bad(format!(
                    "modules[{i}].name: {:?} is not a module name",
                    locked.name
                )));
            }
            if !locked.version.is_empty() && semver::Version::parse(&locked.version).is_err() {
                return Err(bad(format!(
                    "modules[{i}].version: {:?} is not a version",
                    locked.version
                )));
            }
            if let Some(path) = locked.source.strip_prefix("path+") {
                if path.is_empty() || path != normalize(Path::new(path)) {
                    return Err(bad(format!(
                        "modules[{i}].source: {:?} is not a path as `meridian module install` \
                         writes it",
                        locked.source
                    )));
                }
            }
            if locked
                .resolved
                .contains(|c: char| c.is_control() || c.is_whitespace())
                || locked.resolved.starts_with('-')
            {
                return Err(bad(format!(
                    "modules[{i}].resolved: {:?} is not a commit or a digest",
                    locked.resolved
                )));
            }
            lock.modules.push(locked);
        }
        lock.modules.sort_by(|a, b| a.name.cmp(&b.name));

        let items = match top.get("schemas") {
            None | Some(Value::Null) => &[][..],
            Some(Value::List(items)) => items.as_slice(),
            Some(_) => return Err(bad("schemas: expected a list".into())),
        };
        for (i, item) in items.iter().enumerate() {
            let Some(map) = item.as_map() else {
                return Err(bad(format!("schemas[{i}]: expected a mapping")));
            };
            let text = |key: &str| -> Result<String, Error> {
                match map.get(key) {
                    None | Some(Value::Null) => Ok(String::new()),
                    Some(Value::String(s)) => Ok(s.clone()),
                    Some(Value::Int(n)) => Ok(n.to_string()),
                    Some(Value::Float(n)) => Ok(n.to_string()),
                    Some(_) => Err(bad(format!("schemas[{i}].{key}: expected text"))),
                }
            };
            let mut pins = BTreeMap::new();
            match map.get("pins") {
                None | Some(Value::Null) => {}
                Some(Value::Map(entries)) => {
                    for (name, version) in entries {
                        let version = match version {
                            Value::String(s) => s.clone(),
                            Value::Int(n) => n.to_string(),
                            Value::Float(n) => n.to_string(),
                            _ => {
                                return Err(bad(format!(
                                    "schemas[{i}].pins.{name}: expected a version"
                                )))
                            }
                        };
                        pins.insert(name.clone(), version);
                    }
                }
                Some(_) => return Err(bad(format!("schemas[{i}].pins: expected a mapping"))),
            }
            let entry = LockedSchemas {
                target: text("target")?,
                tool: text("tool")?,
                tool_version: text("toolVersion")?,
                pins,
                integrity: text("integrity")?,
            };
            if entry.target.is_empty() {
                return Err(bad(format!("schemas[{i}]: needs a target")));
            }
            lock.schemas.push(entry);
        }
        lock.schemas.sort_by(|a, b| a.target.cmp(&b.target));
        Ok(lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_schemas_are_recorded_and_read_back() {
        let mut lock = Lock::default();
        lock.set_schemas(LockedSchemas {
            target: "terraform".into(),
            tool: "terraform".into(),
            tool_version: "1.13.1".into(),
            pins: BTreeMap::from([(
                "registry.terraform.io/hashicorp/aws".into(),
                "5.60.0".into(),
            )]),
            integrity: "h1:ab12".into(),
        });
        lock.set_schemas(LockedSchemas {
            target: "kubernetes".into(),
            tool: "kubectl".into(),
            tool_version: "1.31".into(),
            pins: BTreeMap::new(),
            integrity: "h1:cd34".into(),
        });
        assert!(!lock.is_empty());
        let text = lock.render();
        assert!(
            text.contains("modules: []\nschemas:\n  - target: kubernetes\n"),
            "{text}"
        );
        assert!(
            text.contains("      registry.terraform.io/hashicorp/aws: \"5.60.0\"\n"),
            "{text}"
        );
        let read = Lock::parse("meridian.lock", text.as_bytes()).unwrap();
        assert_eq!(read, lock);
        assert_eq!(
            read.schemas_for("terraform").unwrap().tool_version,
            "1.13.1"
        );
        // A record is replaced, not repeated, and can be forgotten.
        lock.set_schemas(LockedSchemas {
            target: "kubernetes".into(),
            tool_version: "1.32".into(),
            ..LockedSchemas::default()
        });
        assert_eq!(lock.schemas.len(), 2);
        lock.remove_schemas("kubernetes");
        assert!(lock.schemas_for("kubernetes").is_none());
        // A lockfile without the section reads as before.
        let old = Lock::parse("meridian.lock", b"lockVersion: 1\nmodules: []\n").unwrap();
        assert!(old.schemas.is_empty() && old.is_empty());
    }

    fn sample() -> Lock {
        Lock {
            modules: vec![
                Locked {
                    name: "acme/nginx".into(),
                    version: "1.4.2".into(),
                    source: "git+https://git.example.com/acme/nginx.git//modules/nginx".into(),
                    resolved: "4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a".into(),
                    integrity: "h1:91c4".into(),
                    requires: vec!["acme/local".into(), "acme/base".into()],
                },
                Locked {
                    name: "acme/base".into(),
                    version: "2.0.0".into(),
                    source: "oci+registry.example.com/acme/base".into(),
                    resolved: "sha256:ab12".into(),
                    integrity: "h1:6b1f".into(),
                    requires: vec![],
                },
                Locked {
                    name: "acme/local".into(),
                    source: "path+../local dir".into(),
                    ..Locked::default()
                },
            ],
            schemas: Vec::new(),
        }
    }

    #[test]
    fn the_lockfile_is_sorted_and_reads_back_as_written() {
        let text = sample().render();
        assert_eq!(
            text,
            "# Written by `meridian module`. Commit it; do not edit it.\n\
             lockVersion: 1\n\
             modules:\n  \
             - name: acme/base\n    \
             version: \"2.0.0\"\n    \
             source: oci+registry.example.com/acme/base\n    \
             resolved: sha256:ab12\n    \
             integrity: h1:6b1f\n  \
             - name: acme/local\n    \
             source: \"path+../local dir\"\n  \
             - name: acme/nginx\n    \
             version: \"1.4.2\"\n    \
             source: git+https://git.example.com/acme/nginx.git//modules/nginx\n    \
             resolved: \"4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a\"\n    \
             integrity: h1:91c4\n    \
             requires: [acme/base, acme/local]\n"
        );
        let read = Lock::parse("meridian.lock", text.as_bytes()).unwrap();
        assert_eq!(read.render(), text);
        assert_eq!(read.get("acme/nginx").unwrap().version, "1.4.2");
        assert_eq!(read.get("acme/local").unwrap().source, "path+../local dir");
    }

    #[test]
    fn awkward_values_survive_the_round_trip() {
        for value in [
            "1.4",
            "12345678",
            "1e5",
            "true",
            "a: b",
            "0.1.0-rc.1",
            "he said \"hi\"",
            "#x",
        ] {
            let lock = Lock {
                modules: vec![Locked {
                    name: "a".into(),
                    source: value.into(),
                    integrity: value.into(),
                    ..Locked::default()
                }],
                schemas: Vec::new(),
            };
            let read = Lock::parse("meridian.lock", lock.render().as_bytes()).unwrap();
            assert_eq!(read, lock, "{value}");
        }
    }

    #[test]
    fn a_lockfile_cannot_name_a_directory_outside_the_store() {
        // Issue 132: `name` and `version` become directory names under the
        // state directory, and `module install` deletes and replaces that
        // directory. Each is held to what install would have written.
        let entry = |name: &str, version: &str, source: &str, resolved: &str| {
            format!(
                "lockVersion: 1\nmodules:\n  - name: {name:?}\n    version: {version:?}\n    \
                 source: {source:?}\n    resolved: {resolved:?}\n    integrity: \"h1:0\"\n"
            )
        };
        for (text, want) in [
            (
                entry("evil", "../../../../victim", "git+x#main", "abc"),
                "modules[0].version: \"../../../../victim\" is not a version",
            ),
            (
                entry("evil", "1.0.0/../..", "git+x#main", "abc"),
                "modules[0].version: \"1.0.0/../..\" is not a version",
            ),
            (
                entry("../victim", "1.0.0", "git+x#main", "abc"),
                "modules[0].name: \"../victim\" is not a module name",
            ),
            (
                entry("Evil", "1.0.0", "git+x#main", "abc"),
                "modules[0].name: \"Evil\" is not a module name",
            ),
            (
                entry("a", "", "path+./x/../y/", ""),
                "modules[0].source: \"path+./x/../y/\" is not a path as `meridian module \
                 install` writes it",
            ),
            (
                entry("a", "", "path+", ""),
                "modules[0].source: \"path+\" is not a path as `meridian module install` \
                 writes it",
            ),
            (
                entry("a", "1.0.0", "git+x#main", "--upload-pack=touch x"),
                "modules[0].resolved: \"--upload-pack=touch x\" is not a commit or a digest",
            ),
        ] {
            let err = Lock::parse("meridian.lock", text.as_bytes())
                .unwrap_err()
                .to_string();
            assert_eq!(err, format!("meridian.lock: {want}"));
        }
        // What install writes is read back: a plain version, a `..` path
        // between sibling directories, a bare `ref` pin.
        for text in [
            entry("acme/nginx", "1.4.2+build.7", "git+x#main", "abc"),
            entry("acme/local", "", "path+../local dir", ""),
            entry(
                "a",
                "",
                "git+x#main",
                "4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a",
            ),
        ] {
            Lock::parse("meridian.lock", text.as_bytes()).unwrap();
        }
    }

    #[test]
    fn an_empty_lockfile_is_valid_and_a_newer_one_is_refused() {
        let empty = Lock::default().render();
        assert!(empty.ends_with("lockVersion: 1\nmodules: []\n"));
        assert_eq!(
            Lock::parse("meridian.lock", empty.as_bytes()).unwrap(),
            Lock::default()
        );
        assert_eq!(
            Lock::parse("meridian.lock", b"lockVersion: 2\nmodules: []\n")
                .unwrap_err()
                .to_string(),
            "meridian.lock: the lockfile is format 2, and this build reads up to 1; it was \
             written by a newer meridian. Upgrade meridian"
        );
        assert_eq!(
            Lock::parse("meridian.lock", b"modules: []\n")
                .unwrap_err()
                .to_string(),
            "meridian.lock: lockVersion: expected a whole number"
        );
    }
}
