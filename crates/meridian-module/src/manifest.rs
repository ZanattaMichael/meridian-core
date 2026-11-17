//! `meridian.yaml`: what a module is called, what version it is, and which
//! other modules it needs.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use meridian_ir::{Value, VersionTable};
use meridian_secret::{CredentialDeclaration, FieldSource};
use semver::{Version, VersionReq};

use crate::Error;

/// The file that makes a directory a module.
pub const MANIFEST_FILE: &str = "meridian.yaml";

/// Where a required module comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A directory on disk, relative to the manifest that names it.
    Path(String),
    /// A git repository.
    Git {
        url: String,
        /// A commit, branch or tag to use instead of a version.
        reference: Option<String>,
        /// The directory of the repository that holds the module.
        subdir: Option<String>,
    },
    /// An OCI registry repository, as `host[:port]/path`.
    Oci {
        repository: String,
        /// Talk plain HTTP. For a registry on a trusted network only.
        insecure: bool,
    },
}

impl Source {
    /// The source as the lockfile records it. Two requirements on one module
    /// must agree on this, character for character.
    pub fn locator(&self) -> String {
        match self {
            Source::Path(path) => format!("path+{path}"),
            Source::Git {
                url,
                reference,
                subdir,
            } => {
                let mut out = format!("git+{url}");
                if let Some(subdir) = subdir {
                    out.push_str("//");
                    out.push_str(subdir);
                }
                if let Some(reference) = reference {
                    out.push('#');
                    out.push_str(reference);
                }
                out
            }
            Source::Oci {
                repository,
                insecure,
            } => {
                let scheme = if *insecure { "oci+http://" } else { "oci+" };
                format!("{scheme}{repository}")
            }
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.locator())
    }
}

/// One entry of `spec.modules`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    pub name: String,
    /// The versions that will do. `None` for a `path` source or a git `ref`,
    /// where there is exactly one candidate.
    pub version: Option<VersionReq>,
    pub source: Source,
    /// The credential, declared in the same manifest's `spec.credentials`,
    /// to log in to an `oci` source with. It is honoured only in the
    /// project's own manifest: a fetched module cannot make a project send
    /// a login anywhere.
    pub credential: Option<String>,
}

/// A parsed `meridian.yaml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: Option<Version>,
    pub description: String,
    /// The core releases the module works with.
    pub meridian: Option<VersionReq>,
    pub modules: Vec<Requirement>,
    /// The identities the project declares, for a registry that wants a
    /// login. They say where each secret comes from and hold none.
    pub credentials: Vec<CredentialDeclaration>,
}

/// Whether `name` is one to three segments of `a-z`, `0-9` and `-`.
pub fn valid_name(name: &str) -> bool {
    let segment = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && !s.ends_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let mut count = 0;
    name.split('/').all(|s| {
        count += 1;
        segment(s)
    }) && count <= 3
}

/// A relative path that stays where it is put: no `..`, nothing absolute.
pub(crate) fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

struct Reader<'a> {
    file: &'a str,
}

impl Reader<'_> {
    fn err<T>(&self, field: &str, msg: impl fmt::Display) -> Result<T, Error> {
        Err(Error::new(format!("{}: {field}: {msg}", self.file)))
    }

    fn map<'v>(
        &self,
        value: Option<&'v Value>,
        field: &str,
        known: &[&str],
    ) -> Result<Option<&'v BTreeMap<String, Value>>, Error> {
        let map = match value {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::Map(map)) => map,
            Some(other) => {
                return self.err(
                    field,
                    format!("expected a mapping, found {}", other.describe()),
                )
            }
        };
        if let Some(unknown) = map.keys().find(|k| !known.contains(&k.as_str())) {
            let at = if field.is_empty() {
                unknown.clone()
            } else {
                format!("{field}.{unknown}")
            };
            return self.err(&at, "unknown field");
        }
        Ok(Some(map))
    }

    fn string(&self, value: Option<&Value>, field: &str) -> Result<Option<String>, Error> {
        match value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.is_empty() => self.err(field, "must not be empty"),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            // `version: 1.4` is the number 1.4 in YAML, and 1.10 would
            // silently become 1.1.
            Some(Value::Int(_) | Value::Uint(_) | Value::Float(_)) => {
                self.err(field, "expected text; put the value in quotes")
            }
            Some(other) => self.err(field, format!("expected text, found {}", other.describe())),
        }
    }

    fn requirement(&self, value: &Value, field: &str) -> Result<Requirement, Error> {
        let known = [
            "name",
            "version",
            "path",
            "git",
            "ref",
            "subdir",
            "oci",
            "insecure",
            "credential",
        ];
        let Some(map) = self.map(Some(value), field, &known)? else {
            return self.err(field, "expected a mapping");
        };
        let at = |key: &str| format!("{field}.{key}");
        let get = |key: &str| self.string(map.get(key), &at(key));

        let Some(name) = get("name")? else {
            return self.err(&at("name"), "missing required field");
        };
        if !valid_name(&name) {
            return self.err(&at("name"), invalid_name(&name));
        }
        let version = match get("version")? {
            None => None,
            Some(text) => match VersionReq::parse(&text) {
                Ok(req) => Some(req),
                Err(e) => {
                    return self.err(
                        &at("version"),
                        format!("{text:?} is not a version requirement: {e}"),
                    )
                }
            },
        };

        let (path, git, oci) = (get("path")?, get("git")?, get("oci")?);
        let reference = get("ref")?;
        let subdir = get("subdir")?;
        let credential = get("credential")?;
        let insecure = match map.get("insecure") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return self.err(&at("insecure"), "expected true or false"),
        };
        let only = |key: &str, set: bool, with: &str| -> Result<(), Error> {
            if set {
                self.err(&at(key), format!("only goes with `{with}`"))
            } else {
                Ok(())
            }
        };

        let source =
            match (path, git, oci) {
                (Some(path), None, None) => {
                    only("ref", reference.is_some(), "git")?;
                    only("subdir", subdir.is_some(), "git")?;
                    only("insecure", insecure, "oci")?;
                    only("credential", credential.is_some(), "oci")?;
                    Source::Path(path)
                }
                (None, Some(url), None) => {
                    only("insecure", insecure, "oci")?;
                    only("credential", credential.is_some(), "oci")?;
                    // Both values end up as arguments to git.
                    if url.starts_with('-') {
                        return self.err(&at("git"), "is not a repository address");
                    }
                    if reference.as_deref().is_some_and(|r| r.starts_with('-')) {
                        return self.err(&at("ref"), "is not a commit, branch or tag");
                    }
                    if subdir.as_deref().is_some_and(|s| !safe_relative(s)) {
                        return self.err(
                            &at("subdir"),
                            "must be a relative path inside the repository",
                        );
                    }
                    match (&version, &reference) {
                        (Some(_), Some(_)) => {
                            return self.err(field, "give `version` or `ref`, not both")
                        }
                        (None, None) => return self.err(
                            field,
                            "a git module needs a `version` requirement, or a `ref` to pin it to",
                        ),
                        _ => {}
                    }
                    Source::Git {
                        url,
                        reference,
                        subdir,
                    }
                }
                (None, None, Some(repository)) => {
                    only("ref", reference.is_some(), "git")?;
                    only("subdir", subdir.is_some(), "git")?;
                    if version.is_none() {
                        return self.err(field, "an oci module needs a `version` requirement");
                    }
                    if repository.contains("://") || !repository.contains('/') {
                        return self.err(
                            &at("oci"),
                            "expected a repository such as registry.example.com/team/module, \
                         without a scheme or a tag",
                        );
                    }
                    Source::Oci {
                        repository,
                        insecure,
                    }
                }
                (None, None, None) => {
                    return self.err(field, "needs a source: one of `path`, `git` or `oci`")
                }
                _ => {
                    return self.err(
                        field,
                        "has more than one source; give one of `path`, `git` or `oci`",
                    )
                }
            };
        Ok(Requirement {
            name,
            version,
            source,
            credential,
        })
    }

    /// One entry of `spec.credentials`: a name, a type, and the type's
    /// fields. A field that is not secret is text; a secret field is a
    /// mapping that says where the secret comes from.
    fn credential(&self, value: &Value, field: &str) -> Result<CredentialDeclaration, Error> {
        let Value::Map(map) = value else {
            return self.err(
                field,
                format!("expected a mapping, found {}", value.describe()),
            );
        };
        let at = |key: &str| format!("{field}.{key}");
        let mut declared = CredentialDeclaration {
            name: self
                .string(map.get("name"), &at("name"))?
                .unwrap_or_default(),
            kind: self
                .string(map.get("type"), &at("type"))?
                .unwrap_or_default(),
            ..CredentialDeclaration::default()
        };
        for (key, value) in map {
            if key == "name" || key == "type" {
                continue;
            }
            let source = match value {
                Value::Map(_) => FieldSource::Secret(self.secret_source(value, &at(key))?),
                _ => match self.string(Some(value), &at(key))? {
                    Some(text) => FieldSource::Text(text),
                    None => continue,
                },
            };
            declared.fields.insert(key.clone(), source);
        }
        // The first problem, at its field. What a field holds is never
        // repeated: it may be a secret written where its source belongs.
        if let Some((inner, why)) = declared.problems().into_iter().next() {
            return self.err(&at(&inner), why);
        }
        Ok(declared)
    }

    /// Where a secret comes from: `{from: env, key: NAME}`, `{from: file,
    /// path: FILE}`, `{from: exec, command: [PROGRAM, ARG...]}` or one of
    /// the operating system's stores, with the fields the provider takes.
    fn secret_source(&self, value: &Value, field: &str) -> Result<meridian_secret::Source, Error> {
        let known = [
            "from",
            "key",
            "path",
            "command",
            "service",
            "account",
            "attributes",
            "allowWideMode",
            "keepTrailingNewline",
        ];
        let Some(map) = self.map(Some(value), field, &known)? else {
            return self.err(field, "expected a mapping");
        };
        let at = |key: &str| format!("{field}.{key}");
        let text = |key: &str| Ok(self.string(map.get(key), &at(key))?.unwrap_or_default());
        let flag = |key: &str| match map.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => self.err(&at(key), "expected true or false"),
        };
        let command = match map.get("command") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::List(items)) => {
                let mut command = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let at = format!("{field}.command[{i}]");
                    match self.string(Some(item), &at)? {
                        Some(word) => command.push(word),
                        None => return self.err(&at, "must not be empty"),
                    }
                }
                command
            }
            Some(other) => {
                return self.err(
                    &at("command"),
                    format!(
                        "expected a list of the program and its arguments, found {}",
                        other.describe()
                    ),
                )
            }
        };
        let mut attributes = std::collections::BTreeMap::new();
        match map.get("attributes") {
            None | Some(Value::Null) => {}
            Some(Value::Map(entries)) => {
                for (name, item) in entries {
                    let at = format!("{field}.attributes.{name}");
                    match self.string(Some(item), &at)? {
                        Some(value) => {
                            attributes.insert(name.clone(), value);
                        }
                        None => return self.err(&at, "must not be empty"),
                    }
                }
            }
            Some(other) => {
                return self.err(
                    &at("attributes"),
                    format!(
                        "expected a mapping of the item's attributes, found {}",
                        other.describe()
                    ),
                )
            }
        }
        Ok(meridian_secret::Source {
            from: text("from")?,
            key: text("key")?,
            path: text("path")?,
            command,
            service: text("service")?,
            account: text("account")?,
            attributes,
            allow_wide_mode: flag("allowWideMode")?,
            keep_trailing_newline: flag("keepTrailingNewline")?,
        })
    }
}

fn invalid_name(name: &str) -> String {
    format!(
        "{name:?} is not a module name: use one to three parts of lower-case letters, digits \
         and `-`, joined by `/`, such as acme/nginx"
    )
}

impl Manifest {
    /// Reads the manifest in `dir`.
    pub fn load(dir: &Path) -> Result<Manifest, Error> {
        let path = dir.join(MANIFEST_FILE);
        let name = path.display().to_string();
        let data = std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::new(format!(
                    "{name}: not found; a module is a directory with a {MANIFEST_FILE} in it"
                ))
            } else {
                Error::new(format!("{name}: cannot read file: {e}"))
            }
        })?;
        Manifest::parse(&name, &data)
    }

    /// Parses a manifest. `file` is used only in messages.
    pub fn parse(file: &str, data: &[u8]) -> Result<Manifest, Error> {
        let value = meridian_ir::parse_value(file, data).map_err(|e| Error::new(e.to_string()))?;
        let r = Reader { file };
        let Some(top) = r.map(
            Some(&value),
            "",
            &["apiVersion", "kind", "metadata", "spec"],
        )?
        else {
            return Err(Error::new(format!("{file}: the manifest is empty")));
        };

        match r.string(top.get("apiVersion"), "apiVersion")? {
            None => return r.err("apiVersion", "missing required field"),
            Some(v) => {
                if let Err(e) = VersionTable::builtin().check(&v) {
                    return r.err("apiVersion", e);
                }
            }
        }
        match r.string(top.get("kind"), "kind")?.as_deref() {
            None => return r.err("kind", "missing required field"),
            Some("Module") => {}
            Some(other) => return r.err("kind", format!("expected \"Module\", found {other:?}")),
        }

        let Some(metadata) = r.map(
            top.get("metadata"),
            "metadata",
            &["name", "version", "description"],
        )?
        else {
            return r.err("metadata", "missing required field");
        };
        let Some(name) = r.string(metadata.get("name"), "metadata.name")? else {
            return r.err("metadata.name", "missing required field");
        };
        if !valid_name(&name) {
            return r.err("metadata.name", invalid_name(&name));
        }
        let version = match r.string(metadata.get("version"), "metadata.version")? {
            None => None,
            Some(text) => match Version::parse(&text) {
                Ok(v) => Some(v),
                Err(e) => {
                    return r.err(
                        "metadata.version",
                        format!("{text:?} is not a version such as 1.4.2: {e}"),
                    )
                }
            },
        };
        let description = r
            .string(metadata.get("description"), "metadata.description")?
            .unwrap_or_default();

        let spec = r.map(
            top.get("spec"),
            "spec",
            &["meridian", "modules", "credentials"],
        )?;
        let mut credentials: Vec<CredentialDeclaration> = Vec::new();
        match spec.and_then(|s| s.get("credentials")) {
            None | Some(Value::Null) => {}
            Some(Value::List(items)) => {
                for (i, item) in items.iter().enumerate() {
                    let field = format!("spec.credentials[{i}]");
                    let declared = r.credential(item, &field)?;
                    if credentials.iter().any(|c| c.name == declared.name) {
                        return r.err(
                            &field,
                            format!("{} is already declared above", declared.name),
                        );
                    }
                    credentials.push(declared);
                }
            }
            Some(other) => {
                return r.err(
                    "spec.credentials",
                    format!("expected a list, found {}", other.describe()),
                )
            }
        }
        let meridian = match spec.and_then(|s| s.get("meridian")) {
            None => None,
            some => match r.string(some, "spec.meridian")? {
                None => None,
                Some(text) => match VersionReq::parse(&text) {
                    Ok(req) => Some(req),
                    Err(e) => {
                        return r.err(
                            "spec.meridian",
                            format!("{text:?} is not a version requirement: {e}"),
                        )
                    }
                },
            },
        };
        let mut modules: Vec<Requirement> = Vec::new();
        match spec.and_then(|s| s.get("modules")) {
            None | Some(Value::Null) => {}
            Some(Value::List(items)) => {
                for (i, item) in items.iter().enumerate() {
                    let field = format!("spec.modules[{i}]");
                    let requirement = r.requirement(item, &field)?;
                    if requirement.name == name {
                        return r.err(&field, "a module cannot require itself");
                    }
                    if modules.iter().any(|m| m.name == requirement.name) {
                        return r.err(
                            &field,
                            format!("{} is already required above", requirement.name),
                        );
                    }
                    if let Some(wanted) = &requirement.credential {
                        if !credentials.iter().any(|c| c.name == *wanted) {
                            let declared: Vec<&str> =
                                credentials.iter().map(|c| c.name.as_str()).collect();
                            return r.err(
                                &format!("{field}.credential"),
                                format!(
                                    "no credential named {wanted} is declared in \
                                     spec.credentials{}",
                                    if declared.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" (declared: {})", declared.join(", "))
                                    }
                                ),
                            );
                        }
                    }
                    modules.push(requirement);
                }
            }
            Some(other) => {
                return r.err(
                    "spec.modules",
                    format!("expected a list, found {}", other.describe()),
                )
            }
        }

        Ok(Manifest {
            name,
            version,
            description,
            meridian,
            modules,
            credentials,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"apiVersion: meridian/v1
kind: Module
metadata:
  name: acme/nginx
  version: 1.4.2
  description: An nginx reverse proxy
spec:
  meridian: ">=0.1.0"
  modules:
    - name: acme/base
      path: ../base
    - name: acme/tls
      version: "^2.1"
      git: https://git.example.com/acme/tls.git
      subdir: modules/tls
    - name: acme/pinned
      git: https://git.example.com/acme/pinned.git
      ref: 4f2a9c1
    - name: acme/postgres
      version: "~3.2"
      oci: registry.example.com/acme/postgres
"#;

    fn refused(manifest: &str) -> String {
        Manifest::parse("meridian.yaml", manifest.as_bytes())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn a_full_manifest_reads() {
        let m = Manifest::parse("meridian.yaml", FULL.as_bytes()).unwrap();
        assert_eq!(m.name, "acme/nginx");
        assert_eq!(m.version, Some(Version::new(1, 4, 2)));
        assert_eq!(m.description, "An nginx reverse proxy");
        assert!(m.meridian.unwrap().matches(&Version::new(0, 1, 0)));
        let locators: Vec<String> = m.modules.iter().map(|r| r.source.locator()).collect();
        assert_eq!(
            locators,
            [
                "path+../base",
                "git+https://git.example.com/acme/tls.git//modules/tls",
                "git+https://git.example.com/acme/pinned.git#4f2a9c1",
                "oci+registry.example.com/acme/postgres",
            ]
        );
        assert!(m.modules[1]
            .version
            .as_ref()
            .unwrap()
            .matches(&Version::new(2, 3, 0)));
    }

    #[test]
    fn a_project_needs_only_a_name() {
        let m = Manifest::parse(
            "meridian.yaml",
            b"apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: estate\n",
        )
        .unwrap();
        assert_eq!(m.version, None);
        assert!(m.modules.is_empty());
    }

    #[test]
    fn names_are_one_to_three_plain_parts() {
        for good in ["nginx", "acme/nginx", "acme/web/nginx", "a-b/c9"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            "Acme/nginx",
            "a//b",
            "a/b/c/d",
            "-a",
            "a-",
            "a_b",
            "a/../b",
            "@a",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn every_mistake_names_its_field() {
        let with = |entry: &str| {
            format!(
                "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: estate\nspec:\n  modules:\n    - {entry}\n"
            )
        };
        for (manifest, want) in [
            (
                FULL.replace("meridian/v1", "meridian/v7"),
                "apiVersion: unsupported apiVersion \"meridian/v7\"",
            ),
            (
                FULL.replace("kind: Module", "kind: ResourceSet"),
                "kind: expected \"Module\", found \"ResourceSet\"",
            ),
            (
                FULL.replace("acme/nginx", "Acme/Nginx"),
                "metadata.name: \"Acme/Nginx\" is not a module name",
            ),
            (
                FULL.replace("version: 1.4.2", "version: 1.4"),
                "metadata.version: expected text; put the value in quotes",
            ),
            (
                FULL.replace("version: 1.4.2", "version: one"),
                "metadata.version: \"one\" is not a version such as 1.4.2",
            ),
            (
                FULL.replace("description:", "summary:"),
                "metadata.summary: unknown field",
            ),
            (
                FULL.replace("\"^2.1\"", "\"two\""),
                "spec.modules[1].version: \"two\" is not a version requirement",
            ),
            (
                FULL.replace("name: acme/base", "name: acme/nginx"),
                "spec.modules[0]: a module cannot require itself",
            ),
            (
                FULL.replace("name: acme/tls", "name: acme/base"),
                "spec.modules[1]: acme/base is already required above",
            ),
            (
                with("{name: a}"),
                "spec.modules[0]: needs a source: one of `path`, `git` or `oci`",
            ),
            (
                with("{name: a, path: x, git: y}"),
                "spec.modules[0]: has more than one source",
            ),
            (
                with("{name: a, git: https://x/y.git}"),
                "a git module needs a `version` requirement, or a `ref`",
            ),
            (
                with("{name: a, git: https://x/y.git, version: \"1\", ref: main}"),
                "give `version` or `ref`, not both",
            ),
            (
                with("{name: a, git: --upload-pack=evil, version: \"1\"}"),
                "spec.modules[0].git: is not a repository address",
            ),
            (
                with("{name: a, git: https://x/y.git, ref: --evil}"),
                "spec.modules[0].ref: is not a commit, branch or tag",
            ),
            (
                with("{name: a, git: https://x/y.git, version: \"1\", subdir: ../up}"),
                "spec.modules[0].subdir: must be a relative path inside the repository",
            ),
            (
                with("{name: a, oci: registry.example.com/a}"),
                "an oci module needs a `version` requirement",
            ),
            (
                with("{name: a, oci: \"https://registry.example.com/a\", version: \"1\"}"),
                "spec.modules[0].oci: expected a repository such as",
            ),
            (
                with("{name: a, path: x, ref: main}"),
                "spec.modules[0].ref: only goes with `git`",
            ),
            (
                with("{name: a, path: x, insecure: true}"),
                "spec.modules[0].insecure: only goes with `oci`",
            ),
            (
                with("{name: a, path: x, tag: v1}"),
                "spec.modules[0].tag: unknown field",
            ),
        ] {
            let err = refused(&manifest);
            assert!(err.contains(want), "wanted {want:?} in {err:?}");
            assert!(err.starts_with("meridian.yaml: "), "{err}");
        }
    }

    const WITH_LOGIN: &str = r#"apiVersion: meridian/v1
kind: Module
metadata:
  name: estate
spec:
  modules:
    - name: acme/postgres
      version: "~3.2"
      oci: registry.example.com/acme/postgres
      credential: internal
  credentials:
    - name: internal
      type: registry
      server: registry.example.com
      user: ci
      password:
        from: env
        key: REGISTRY_PASSWORD
    - name: build
      type: usernamePassword
      user: builder
      password:
        from: file
        path: /run/secrets/build
        allowWideMode: true
    - name: helper
      type: registry
      user: ci
      password:
        from: exec
        command: [pass, show, registry]
"#;

    #[test]
    fn credentials_are_declared_and_named_by_a_registry_source() {
        let m = Manifest::parse("meridian.yaml", WITH_LOGIN.as_bytes()).unwrap();
        assert_eq!(m.modules[0].credential.as_deref(), Some("internal"));
        let names: Vec<&str> = m.credentials.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["internal", "build", "helper"]);
        let internal = &m.credentials[0];
        assert_eq!(internal.kind, "registry");
        assert!(matches!(
            internal.fields.get("server"),
            Some(FieldSource::Text(server)) if server == "registry.example.com"
        ));
        assert!(matches!(
            internal.fields.get("password"),
            Some(FieldSource::Secret(source)) if source.from == "env" && source.key == "REGISTRY_PASSWORD"
        ));
        assert!(matches!(
            m.credentials[1].fields.get("password"),
            Some(FieldSource::Secret(source))
                if source.from == "file" && source.path == "/run/secrets/build" && source.allow_wide_mode
        ));
        assert!(matches!(
            m.credentials[2].fields.get("password"),
            Some(FieldSource::Secret(source)) if source.command == ["pass", "show", "registry"]
        ));
    }

    #[test]
    fn every_mistake_in_a_credential_names_its_field() {
        for (manifest, want) in [
            (
                WITH_LOGIN.replace("credential: internal", "credential: other"),
                "spec.modules[0].credential: no credential named other is declared in \
                 spec.credentials (declared: internal, build, helper)",
            ),
            (
                WITH_LOGIN.replace(
                    "oci: registry.example.com/acme/postgres",
                    "path: ../postgres",
                ),
                "spec.modules[0].credential: only goes with `oci`",
            ),
            (
                WITH_LOGIN.replace("name: build", "name: internal"),
                "spec.credentials[1]: internal is already declared above",
            ),
            (
                WITH_LOGIN.replace("type: usernamePassword", "type: password"),
                "spec.credentials[1].type: \"password\" is not a credential type this version has",
            ),
            (
                WITH_LOGIN.replace(
                    "      user: builder\n",
                    "      user: builder\n      host: x\n",
                ),
                "spec.credentials[1].host: host is not a field of a usernamePassword credential",
            ),
            (
                WITH_LOGIN.replace("from: env", "from: vault"),
                "spec.credentials[0].password",
            ),
            (
                WITH_LOGIN.replace("key: REGISTRY_PASSWORD", "name: REGISTRY_PASSWORD"),
                "spec.credentials[0].password.name: unknown field",
            ),
            (
                WITH_LOGIN.replace(
                    "command: [pass, show, registry]",
                    "command: pass show registry",
                ),
                "spec.credentials[2].password.command: expected a list of the program and its \
                 arguments",
            ),
            (
                WITH_LOGIN.replace("  credentials:\n", "  credentials:\n    - internal\n"),
                "spec.credentials[0]: expected a mapping",
            ),
        ] {
            let err = refused(&manifest);
            assert!(err.contains(want), "wanted {want:?} in {err:?}");
            assert!(err.starts_with("meridian.yaml: "), "{err}");
        }
    }

    #[test]
    fn a_secret_written_into_the_manifest_is_refused_and_not_repeated() {
        let written = WITH_LOGIN.replace(
            "      password:\n        from: env\n        key: REGISTRY_PASSWORD\n",
            "      password: written-in-the-file\n",
        );
        let err = refused(&written);
        assert!(
            err.contains("spec.credentials[0].password: a password is written in the file"),
            "{err}"
        );
        assert!(!err.contains("written-in-the-file"), "{err}");
    }
}
