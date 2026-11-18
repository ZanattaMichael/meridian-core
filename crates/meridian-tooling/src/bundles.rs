//! Schema bundles: a target's tool schemas, packed, signed and published,
//! so a project gets them without running the tool.
//!
//! `meridian schema sync` asks a target's installed tool for its schemas,
//! which needs the tool, often the network and sometimes a cloud account,
//! and may take minutes. A bundle is what one such sync produced for one set
//! of pins (`hashicorp/aws 5.60.0`, `kubernetes 1.31`), laid out so that a
//! reader loads only the types it meets:
//!
//! ```text
//! bundle.json           the header and the index: one line per type
//! types/<hash>.json     one resource type's schema each
//! meridian.sig          the publisher's signature, when signed
//! ```
//!
//! A bundle is stored as a gzipped tar of that directory, the same archive
//! modules use, in an OCI registry under `<registry>/<target>/<name>:<version>`
//! or in a directory laid out the same way. Each registry has an index
//! (`<registry>/index:latest`, or `index.json` in a directory) that lists
//! every bundle with its types and its digest, which is how `sync` finds the
//! bundles that describe what the documents use.
//!
//! What a bundle holds is data in the [`ResourceSchema`] model and nothing
//! else: the reader refuses anything outside it, anything too large, and a
//! type file that names another type. Schemas stay advice: a bad bundle can
//! mislead the editor and `lint`, never change what `emit` writes or what a
//! tool applies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use meridian_module::{
    hash_dir, pack, unpack, valid_tag, Credential, Policy, SecretKey, Signature, Verification,
    SCHEMA_BUNDLE_ARTIFACT, SCHEMA_INDEX_ARTIFACT, SIGNATURE_FILE,
};
use meridian_sdk::ResourceSchema;
use sha2::{Digest, Sha256};

use crate::schemas::Cached;

/// The bundle format this build writes and reads.
pub const BUNDLE_VERSION: u32 = 1;
/// The index format this build writes and reads.
pub const INDEX_VERSION: u32 = 1;
/// The header and index of a bundle.
pub const BUNDLE_FILE: &str = "bundle.json";
/// Where a bundle keeps one file per type.
pub const TYPES_DIR: &str = "types";
/// The tag a registry's index is published under.
pub const INDEX_TAG: &str = "latest";
/// The repository, under a registry, that holds its index.
pub const INDEX_REPOSITORY: &str = "index";
/// The file a directory registry keeps its index in.
pub const INDEX_FILE: &str = "index.json";

/// The most one type's schema may take. The largest real ones (an Azure
/// resource with every nested property) are a few hundred kilobytes.
const MAX_TYPE_BYTES: u64 = 8 << 20;
/// The most the header and index may take.
const MAX_HEADER_BYTES: u64 = 64 << 20;
/// The most types one bundle may list.
const MAX_TYPES: usize = 200_000;
/// How deep params may nest. Real schemas stop well before this.
const MAX_DEPTH: usize = 32;
/// The longest type name and the longest description line.
const MAX_NAME: usize = 512;
const MAX_LINE: usize = 1024;

/// What a bundle is: the target its schemas are for, its name within the
/// target, and its version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundleId {
    pub target: String,
    pub name: String,
    pub version: String,
}

impl BundleId {
    /// Checks the three parts: a target and a name are lower-case letters,
    /// digits, `.`, `_` and `-`; a version is a registry tag.
    pub fn new(target: &str, name: &str, version: &str) -> Result<BundleId, String> {
        for (what, part) in [("target", target), ("name", name)] {
            let ok = !part.is_empty()
                && part.len() <= 128
                && part.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && part.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
                });
            if !ok {
                return Err(format!(
                    "{part:?} cannot be a bundle {what}: use lower-case letters, digits, `.`, `_` \
                     and `-`, starting with a letter or a digit, such as hashicorp-aws"
                ));
            }
        }
        if !valid_tag(version) {
            return Err(format!(
                "{version:?} cannot be a bundle version: use letters, digits, `.`, `_` and `-`, \
                 such as 5.60.0 or 2026-11-18"
            ));
        }
        Ok(BundleId {
            target: target.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
        })
    }

    /// `terraform/hashicorp-aws:5.60.0`.
    pub fn label(&self) -> String {
        format!("{}/{}:{}", self.target, self.name, self.version)
    }
}

/// One line of a bundle's index.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    /// The type as a document writes it.
    #[serde(rename = "type")]
    pub r#type: String,
    /// The first line of its description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The file under the bundle that describes it in full.
    pub file: String,
}

/// `bundle.json`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Header {
    pub bundle_version: u32,
    pub target: String,
    pub name: String,
    pub version: String,
    /// The tool the schemas were read from, and its version.
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub tool_version: String,
    /// The exact versions of the parts the schemas were read from.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// What the sync that made the bundle reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Every type, sorted.
    pub types: Vec<IndexEntry>,
}

impl Header {
    pub fn id(&self) -> BundleId {
        BundleId {
            target: self.target.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
        }
    }
}

/// The file one type's schema is kept in: a hash of its name, so that no
/// type name, whatever characters it holds, becomes a path.
fn type_file(name: &str) -> String {
    let hex: String = Sha256::digest(name.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{TYPES_DIR}/{hex}.json")
}

/// The first line of a description, cut to a length an index can hold.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.len() <= MAX_LINE {
        return line.to_owned();
    }
    let mut end = MAX_LINE;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

/// What is signed: the bundle's name and its content hash. It cannot pass
/// for a module's statement, nor one bundle's for another's.
fn statement(id: &BundleId, integrity: &str) -> Vec<u8> {
    format!(
        "meridian-schema-bundle-statement-v1\ntarget {}\nname {}\nversion {}\nintegrity {integrity}\n",
        id.target, id.name, id.version
    )
    .into_bytes()
}

/// Writes the bundle for `cached` into the new or empty directory `dir`.
/// Every type in the cache is written in full; a type the cache lists only
/// by name (open, with no params) is written as it is.
pub fn write(cached: &Cached, id: &BundleId, dir: &Path) -> Result<Header, String> {
    if id.target != cached.target {
        return Err(format!(
            "the cache is of target {}, and the bundle is named for {}",
            cached.target, id.target
        ));
    }
    if dir.exists()
        && std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .next()
            .is_some()
    {
        return Err(format!("{} is not empty", dir.display()));
    }
    let types_dir = dir.join(TYPES_DIR);
    std::fs::create_dir_all(&types_dir).map_err(|e| format!("{}: {e}", types_dir.display()))?;
    let mut entries = Vec::with_capacity(cached.schemas.len());
    let mut seen = BTreeSet::new();
    for schema in &cached.schemas {
        if schema.is_pattern() || schema.r#type.is_empty() || !seen.insert(&schema.r#type) {
            continue;
        }
        let file = type_file(&schema.r#type);
        let mut text = serde_json::to_string(schema).map_err(|e| e.to_string())?;
        text.push('\n');
        let path = dir.join(&file);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        entries.push(IndexEntry {
            r#type: schema.r#type.clone(),
            description: first_line(&schema.description),
            file,
        });
    }
    entries.sort_by(|a, b| a.r#type.cmp(&b.r#type));
    let header = Header {
        bundle_version: BUNDLE_VERSION,
        target: id.target.clone(),
        name: id.name.clone(),
        version: id.version.clone(),
        tool: cached.tool.clone(),
        tool_version: cached.tool_version.clone(),
        pins: cached.pins.clone(),
        notes: cached.notes.clone(),
        types: entries,
    };
    let mut text = serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?;
    text.push('\n');
    let path = dir.join(BUNDLE_FILE);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(header)
}

/// Signs the bundle in `dir` as it is now, writing `meridian.sig` in it, and
/// returns the id of the key.
pub fn sign(dir: &Path, key: &SecretKey) -> Result<String, String> {
    let bundle = Bundle::open(dir)?;
    let integrity = hash_dir(dir).map_err(String::from)?;
    let text = key.sign_bytes(&statement(&bundle.header.id(), &integrity));
    let path = dir.join(SIGNATURE_FILE);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(key.public().id())
}

/// The archive of the bundle in `dir`: the same deterministic gzipped tar
/// modules are packed as, so the same bundle always has the same digest.
pub fn archive(dir: &Path) -> Result<Vec<u8>, String> {
    pack(dir).map_err(String::from)
}

/// `sha256:` and the hex SHA-256 of `bytes`.
pub fn digest_of(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// A bundle on disk, opened: its header read and checked, its types read
/// one at a time when asked for.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub dir: PathBuf,
    pub header: Header,
}

impl Bundle {
    /// Opens the bundle in `dir`, checking its header.
    pub fn open(dir: &Path) -> Result<Bundle, String> {
        let path = dir.join(BUNDLE_FILE);
        let bytes = read_limited(&path, MAX_HEADER_BYTES)?;
        let header: Header = serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "{}: not a schema bundle header this meridian reads: {e}",
                path.display()
            )
        })?;
        if header.bundle_version != BUNDLE_VERSION {
            return Err(format!(
                "{}: bundle format {} (this meridian reads format {BUNDLE_VERSION}); upgrade \
                 meridian",
                path.display(),
                header.bundle_version
            ));
        }
        BundleId::new(&header.target, &header.name, &header.version)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if header.types.len() > MAX_TYPES {
            return Err(format!(
                "{}: lists {} types, more than the {MAX_TYPES} a bundle may hold",
                path.display(),
                header.types.len()
            ));
        }
        let mut seen = BTreeSet::new();
        for entry in &header.types {
            if entry.r#type.is_empty()
                || entry.r#type.len() > MAX_NAME
                || entry.r#type.contains(|c: char| c.is_control() || c == '<')
                || entry.description.len() > MAX_LINE + 4
                || entry.file != type_file(&entry.r#type)
                || !seen.insert(entry.r#type.as_str())
            {
                return Err(format!(
                    "{}: the index entry for {:?} is not one `meridian schema pack` writes",
                    path.display(),
                    entry.r#type
                ));
            }
        }
        Ok(Bundle {
            dir: dir.to_path_buf(),
            header,
        })
    }

    /// Unpacks an archive into the new directory `dir` and opens it.
    pub fn unpack(archive: &[u8], dir: &Path) -> Result<Bundle, String> {
        unpack(archive, dir).map_err(|e| e.to_string().replace("module archive", "bundle"))?;
        Bundle::open(dir)
    }

    pub fn id(&self) -> BundleId {
        self.header.id()
    }

    /// Whether the bundle lists `name`.
    pub fn has(&self, name: &str) -> bool {
        self.entry(name).is_some()
    }

    fn entry(&self, name: &str) -> Option<&IndexEntry> {
        self.header
            .types
            .binary_search_by(|e| e.r#type.as_str().cmp(name))
            .ok()
            .map(|i| &self.header.types[i])
    }

    /// The full schema of one type, read from its file and checked.
    pub fn load_type(&self, name: &str) -> Result<Option<ResourceSchema>, String> {
        let Some(entry) = self.entry(name) else {
            return Ok(None);
        };
        let path = self.dir.join(&entry.file);
        let bytes = read_limited(&path, MAX_TYPE_BYTES)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: not JSON: {e}", path.display()))?;
        check_shape(&value, 0).map_err(|e| format!("{}: {e}", path.display()))?;
        let schema: ResourceSchema = serde_json::from_value(value)
            .map_err(|e| format!("{}: not a resource type schema: {e}", path.display()))?;
        if schema.r#type != name {
            return Err(format!(
                "{}: describes {:?}, and the index says it describes {name:?}",
                path.display(),
                schema.r#type
            ));
        }
        Ok(Some(schema))
    }

    /// What the signature in the bundle says, whatever the policy.
    pub fn signature(&self, trusted: &Verification) -> Signature {
        let text = match std::fs::read_to_string(self.dir.join(SIGNATURE_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Signature::Unsigned,
            Err(e) => return Signature::Invalid(format!("{SIGNATURE_FILE} cannot be read: {e}")),
        };
        let integrity = match hash_dir(&self.dir) {
            Ok(integrity) => integrity,
            Err(e) => return Signature::Invalid(e.to_string()),
        };
        meridian_signing::verify_bytes(
            &text,
            &statement(&self.id(), &integrity),
            &trusted.trusted,
            "schema bundle",
        )
    }

    /// Applies the policy. `Ok` carries a warning to show when the bundle is
    /// used although its signature did not pass, which only `warn` allows.
    pub fn check_signature(&self, trusted: &Verification) -> Result<Option<String>, String> {
        if trusted.policy == Policy::Off {
            return Ok(None);
        }
        let label = self.id().label();
        match (self.signature(trusted), trusted.policy) {
            (Signature::Verified { .. } | Signature::NotChecked, _) => Ok(None),
            (Signature::Unsigned, Policy::Require) => Err(format!(
                "the schema bundle {label} is not signed, and schema bundle signatures are \
                 required; ask its publisher to sign it with `meridian schema pack --key`, or \
                 relax the check with --schema-signatures warn"
            )),
            (Signature::Invalid(why), Policy::Require) => Err(format!(
                "the schema bundle {label}: its signature does not pass: {why}. Schema bundle \
                 signatures are required, so it was not used; trust its signer's key with \
                 --trusted-key"
            )),
            (Signature::Unsigned, _) => {
                Ok(Some(format!("the schema bundle {label} is not signed")))
            }
            (Signature::Invalid(why), _) => Ok(Some(format!(
                "the schema bundle {label}: its signature does not pass: {why}"
            ))),
        }
    }

    /// The cache this bundle gives: the types in `wanted` in full and every
    /// other type by name, or every type in full when `wanted` is `None`.
    pub fn to_cache(&self, wanted: Option<&BTreeSet<String>>) -> Result<Cached, String> {
        let mut schemas = Vec::with_capacity(self.header.types.len());
        for entry in &self.header.types {
            let full = wanted.is_none_or(|w| w.contains(&entry.r#type));
            if full {
                if let Some(schema) = self.load_type(&entry.r#type)? {
                    schemas.push(schema);
                }
            } else {
                schemas.push(ResourceSchema::open(&entry.r#type, &entry.description));
            }
        }
        Ok(Cached {
            cache_version: crate::schemas::CACHE_VERSION,
            target: self.header.target.clone(),
            tool: self.header.tool.clone(),
            tool_version: self.header.tool_version.clone(),
            pins: self.header.pins.clone(),
            // A cache made from bundles is the same bytes on every machine,
            // so the lock's hash of it means the same everywhere.
            synced_at: String::new(),
            notes: self.header.notes.clone(),
            schemas,
            bundles: vec![self.id().label()],
        })
    }
}

/// Joins the caches several bundles of one target gave into one. The first
/// bundle to describe a type in full wins; a type only named is replaced by
/// a bundle that describes it.
pub fn merge(target: &str, caches: Vec<Cached>) -> Cached {
    let mut out = Cached {
        cache_version: crate::schemas::CACHE_VERSION,
        target: target.to_owned(),
        ..Cached::default()
    };
    let mut by_type: BTreeMap<String, ResourceSchema> = BTreeMap::new();
    for cached in caches {
        if out.tool.is_empty() {
            out.tool = cached.tool;
            out.tool_version = cached.tool_version;
        }
        out.pins.extend(cached.pins);
        out.notes.extend(cached.notes);
        out.bundles.extend(cached.bundles);
        for schema in cached.schemas {
            let named_only = schema.open && schema.params.is_empty();
            match by_type.get(&schema.r#type) {
                Some(kept) if !(kept.open && kept.params.is_empty()) || named_only => {}
                _ => {
                    by_type.insert(schema.r#type.clone(), schema);
                }
            }
        }
    }
    out.notes.sort();
    out.notes.dedup();
    out.bundles.sort();
    out.bundles.dedup();
    out.schemas = by_type.into_values().collect();
    out
}

/// Reads a file, refusing one larger than `limit`.
fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > limit {
        return Err(format!(
            "{}: {} bytes, more than the {limit} it may take",
            path.display(),
            meta.len()
        ));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Holds a type's JSON to the shape a schema has: objects nested no deeper
/// than params can, no string longer than a schema needs.
fn check_shape(value: &serde_json::Value, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH * 2 + 2 {
        return Err(format!("nests deeper than {MAX_DEPTH} levels of params"));
    }
    match value {
        serde_json::Value::String(s) if s.len() > 1 << 20 => {
            Err("holds a string longer than a schema needs".to_owned())
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(|v| check_shape(v, depth + 1)),
        serde_json::Value::Object(map) => map.values().try_for_each(|v| check_shape(v, depth + 1)),
        _ => Ok(()),
    }
}

/// One bundle, as a registry's index lists it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Listed {
    pub target: String,
    pub name: String,
    pub version: String,
    /// What it is fetched by: the manifest digest in an OCI registry, the
    /// archive's in a directory.
    pub digest: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub tool_version: String,
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// Every type it describes, sorted.
    pub types: Vec<String>,
}

impl Listed {
    pub fn id(&self) -> BundleId {
        BundleId {
            target: self.target.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
        }
    }
}

/// A registry's index: every bundle it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Index {
    pub index_version: u32,
    pub bundles: Vec<Listed>,
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        let index: Index = serde_json::from_slice(bytes)
            .map_err(|e| format!("not a schema index this meridian reads: {e}"))?;
        if index.index_version != INDEX_VERSION {
            return Err(format!(
                "index format {} (this meridian reads format {INDEX_VERSION}); upgrade meridian",
                index.index_version
            ));
        }
        for listed in &index.bundles {
            BundleId::new(&listed.target, &listed.name, &listed.version)?;
            if !meridian_module::valid_digest(&listed.digest) {
                return Err(format!(
                    "{} is listed with {:?}, which is not a sha256 digest",
                    listed.id().label(),
                    listed.digest
                ));
            }
        }
        Ok(index)
    }

    /// The index's text: sorted, so the same bundles give the same bytes.
    pub fn render(&self) -> String {
        let mut index = self.clone();
        index.index_version = INDEX_VERSION;
        index.bundles.sort_by_key(|b| b.id());
        for b in &mut index.bundles {
            b.types.sort();
            b.types.dedup();
        }
        let mut text = serde_json::to_string_pretty(&index).unwrap_or_default();
        text.push('\n');
        text
    }

    /// Adds a bundle, replacing the entry for the same target, name and
    /// version.
    pub fn add(&mut self, listed: Listed) {
        self.bundles.retain(|b| b.id() != listed.id());
        self.bundles.push(listed);
        self.bundles.sort_by_key(|b| b.id());
    }

    /// The bundles of `target` that describe `type_name`, newest first.
    pub fn candidates<'a>(&'a self, target: &str, type_name: &str) -> Vec<&'a Listed> {
        let mut found: Vec<&Listed> = self
            .bundles
            .iter()
            .filter(|b| {
                b.target == target
                    && b.types
                        .binary_search_by(|t| t.as_str().cmp(type_name))
                        .is_ok()
            })
            .collect();
        found.sort_by(|a, b| compare_versions(&b.version, &a.version).then(a.name.cmp(&b.name)));
        found
    }
}

/// Orders two bundle versions: as SemVer when both are (`5.9.0` before
/// `5.10.0`), as text otherwise, which orders a date tag (`2026-11-18`)
/// correctly.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let semver = |v: &str| {
        let v = v.strip_prefix('v').unwrap_or(v);
        semver::Version::parse(v)
            .or_else(|_| semver::Version::parse(&format!("{v}.0")))
            .ok()
    };
    match (semver(a), semver(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

/// A place bundles are kept: an OCI registry, or a directory laid out as
/// `<target>/<name>/<version>.tar.gz` with an `index.json`.
pub enum Registry {
    Oci {
        /// The repository prefix, such as `ghcr.io/acme/meridian-schemas`.
        prefix: String,
        insecure: bool,
        login: Option<Credential>,
    },
    Dir(PathBuf),
}

impl Registry {
    /// What the registry is called in messages and in the lock.
    pub fn describe(&self) -> String {
        match self {
            Registry::Oci { prefix, .. } => prefix.clone(),
            Registry::Dir(dir) => format!("file://{}", dir.display()),
        }
    }

    /// The lock's reference for one bundle in this registry.
    pub fn reference(&self, id: &BundleId) -> String {
        match self {
            Registry::Oci { prefix, .. } => {
                format!("oci+{prefix}/{}/{}:{}", id.target, id.name, id.version)
            }
            Registry::Dir(dir) => format!(
                "file+{}/{}/{}/{}.tar.gz",
                dir.display(),
                id.target,
                id.name,
                id.version
            ),
        }
    }

    /// The registry's index; an empty one when it has none yet.
    pub fn index(&self) -> Result<Index, String> {
        match self {
            Registry::Oci {
                prefix,
                insecure,
                login,
            } => {
                let repository = format!("{prefix}/{INDEX_REPOSITORY}");
                match meridian_module::pull_artifact(
                    &repository,
                    *insecure,
                    login.as_ref(),
                    SCHEMA_INDEX_ARTIFACT,
                    INDEX_TAG,
                )
                .map_err(String::from)?
                {
                    None => Ok(Index::default()),
                    Some((_, bytes)) => {
                        Index::parse(&bytes).map_err(|e| format!("the index of {prefix}: {e}"))
                    }
                }
            }
            Registry::Dir(dir) => {
                let path = dir.join(INDEX_FILE);
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        Index::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
                    Err(e) => Err(format!("{}: {e}", path.display())),
                }
            }
        }
    }

    /// Fetches the archive of one bundle, checked against `digest` when one
    /// is given. Returns the digest it was fetched by and the archive.
    pub fn fetch(&self, id: &BundleId, digest: Option<&str>) -> Result<(String, Vec<u8>), String> {
        match self {
            Registry::Oci {
                prefix,
                insecure,
                login,
            } => {
                let repository = format!("{prefix}/{}/{}", id.target, id.name);
                let reference = digest.unwrap_or(&id.version);
                let found = meridian_module::pull_artifact(
                    &repository,
                    *insecure,
                    login.as_ref(),
                    SCHEMA_BUNDLE_ARTIFACT,
                    reference,
                )
                .map_err(String::from)?;
                found.ok_or_else(|| format!("{repository} holds no {reference}"))
            }
            Registry::Dir(dir) => {
                let path = dir
                    .join(&id.target)
                    .join(&id.name)
                    .join(format!("{}.tar.gz", id.version));
                let bytes = read_limited(&path, meridian_module::MAX_UNPACKED_BYTES)?;
                let found = digest_of(&bytes);
                if let Some(wanted) = digest.filter(|d| *d != found) {
                    return Err(format!(
                        "{} is not the bundle that was pinned: it is {found}, and {wanted} was \
                         pinned",
                        path.display()
                    ));
                }
                Ok((found, bytes))
            }
        }
    }

    /// Publishes one packed bundle, then lists it in the index unless
    /// `index` is false. A version is published once: the same content again
    /// changes nothing, and other content under the same version is refused.
    pub fn publish(
        &self,
        bundle: &Bundle,
        archive: &[u8],
        index: bool,
    ) -> Result<Published, String> {
        let id = bundle.id();
        let (digest, already) = match self {
            Registry::Oci {
                prefix,
                insecure,
                login,
            } => {
                let repository = format!("{prefix}/{}/{}", id.target, id.name);
                // The title is the file name `oras pull` writes the layer to.
                let title = format!("{}-{}-{}.tar.gz", id.target, id.name, id.version);
                let pushed = meridian_module::push_artifact(
                    &repository,
                    *insecure,
                    login.as_ref(),
                    &meridian_module::Artifact {
                        kind: SCHEMA_BUNDLE_ARTIFACT,
                        tag: &id.version,
                        annotations: &[
                            ("org.opencontainers.image.title", title.as_str()),
                            ("org.opencontainers.image.version", id.version.as_str()),
                        ],
                        layer: archive,
                        replace: false,
                    },
                )
                .map_err(String::from)?;
                (pushed.digest, pushed.already_published)
            }
            Registry::Dir(dir) => {
                let folder = dir.join(&id.target).join(&id.name);
                let path = folder.join(format!("{}.tar.gz", id.version));
                let digest = digest_of(archive);
                let already = match std::fs::read(&path) {
                    Ok(existing) if digest_of(&existing) == digest => true,
                    Ok(existing) => {
                        return Err(format!(
                            "{} is already published in {} with different content ({}); a \
                             bundle version is published once",
                            id.label(),
                            self.describe(),
                            digest_of(&existing)
                        ))
                    }
                    Err(_) => false,
                };
                if !already {
                    std::fs::create_dir_all(&folder)
                        .map_err(|e| format!("{}: {e}", folder.display()))?;
                    std::fs::write(&path, archive)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
                (digest, already)
            }
        };
        if index {
            let mut current = self.index()?;
            current.add(Listed {
                target: id.target.clone(),
                name: id.name.clone(),
                version: id.version.clone(),
                digest: digest.clone(),
                tool: bundle.header.tool.clone(),
                tool_version: bundle.header.tool_version.clone(),
                pins: bundle.header.pins.clone(),
                types: bundle
                    .header
                    .types
                    .iter()
                    .map(|t| t.r#type.clone())
                    .collect(),
            });
            self.write_index(&current)?;
        }
        Ok(Published {
            reference: self.reference(&id),
            digest,
            already_published: already,
        })
    }

    fn write_index(&self, index: &Index) -> Result<(), String> {
        let text = index.render();
        match self {
            Registry::Oci {
                prefix,
                insecure,
                login,
            } => {
                let repository = format!("{prefix}/{INDEX_REPOSITORY}");
                meridian_module::push_artifact(
                    &repository,
                    *insecure,
                    login.as_ref(),
                    &meridian_module::Artifact {
                        kind: SCHEMA_INDEX_ARTIFACT,
                        tag: INDEX_TAG,
                        annotations: &[("org.opencontainers.image.title", INDEX_FILE)],
                        layer: text.as_bytes(),
                        replace: true,
                    },
                )
                .map_err(String::from)?;
                Ok(())
            }
            Registry::Dir(dir) => {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let path = dir.join(INDEX_FILE);
                std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
            }
        }
    }
}

/// What [`Registry::publish`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub reference: String,
    pub digest: String,
    pub already_published: bool,
}

/// Where fetched bundles are kept, unpacked, by digest: under the cache, so
/// a sync that needs a bundle it has fetched before works offline.
pub fn store_dir(cache_dir: &Path, digest: &str) -> PathBuf {
    cache_dir.join("bundles").join(digest.replace(':', "-"))
}

/// Opens the bundle fetched as `digest`, from the store when it is there,
/// otherwise by `fetch`, which returns the digest it got and the archive.
pub fn open_stored(
    cache_dir: &Path,
    digest: Option<&str>,
    fetch: impl FnOnce() -> Result<(String, Vec<u8>), String>,
) -> Result<(String, Bundle), String> {
    if let Some(digest) = digest {
        let dir = store_dir(cache_dir, digest);
        if dir.join(BUNDLE_FILE).is_file() {
            if let Ok(bundle) = Bundle::open(&dir) {
                return Ok((digest.to_owned(), bundle));
            }
        }
    }
    let (got, archive) = fetch()?;
    if let Some(wanted) = digest.filter(|d| *d != got) {
        return Err(format!(
            "the registry answered for {wanted} with {got}; the bundle was not used"
        ));
    }
    let dir = store_dir(cache_dir, &got);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // Unpacked beside its final place and moved in, so a bundle that does not
    // unpack leaves nothing behind that looks like one.
    let partial = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&partial);
    let bundle = match Bundle::unpack(&archive, &partial) {
        Ok(_) => {
            std::fs::rename(&partial, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            Bundle::open(&dir)?
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(e);
        }
    };
    Ok((got, bundle))
}

/// Reads a bundle reference as `meridian schema fetch` takes one:
/// `oci://REPOSITORY/<target>/<name>:<version>`, or a packed bundle file as
/// `file://PATH` or a plain path.
pub enum Reference {
    Oci { prefix: String, id: BundleId },
    File(PathBuf),
}

impl Reference {
    pub fn parse(text: &str) -> Result<Reference, String> {
        if let Some(rest) = text.strip_prefix("oci://") {
            let bad = || {
                format!(
                    "{text:?} is not a bundle in a registry, such as \
                     oci://ghcr.io/acme/meridian-schemas/terraform/hashicorp-aws:5.60.0"
                )
            };
            let (path, version) = rest.rsplit_once(':').ok_or_else(bad)?;
            if version.contains('/') {
                return Err(bad());
            }
            let mut parts: Vec<&str> = path.rsplitn(3, '/').collect();
            if parts.len() != 3 {
                return Err(bad());
            }
            let prefix = parts.pop().unwrap_or_default();
            let target = parts.pop().unwrap_or_default();
            let name = parts.pop().unwrap_or_default();
            if !prefix.contains('/') {
                return Err(bad());
            }
            let id = BundleId::new(target, name, version)?;
            return Ok(Reference::Oci {
                prefix: prefix.to_owned(),
                id,
            });
        }
        let path = text.strip_prefix("file://").unwrap_or(text);
        if path.is_empty() {
            return Err("a bundle file needs a path".to_owned());
        }
        Ok(Reference::File(PathBuf::from(path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{ParamKind, ParamSchema};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meridian-bundles-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn sample() -> Cached {
        Cached {
            cache_version: 1,
            target: "terraform".into(),
            tool: "terraform".into(),
            tool_version: "1.13.1".into(),
            pins: BTreeMap::from([(
                "registry.terraform.io/hashicorp/aws".into(),
                "5.60.0".into(),
            )]),
            synced_at: "2026-11-18T00:00:00Z".into(),
            notes: vec![],
            schemas: vec![
                ResourceSchema::open("aws_instance", "An EC2 instance.\nMore text.")
                    .param(ParamSchema::new("ami", ParamKind::String, "The image.").required()),
                ResourceSchema::open("aws_s3_bucket", "A bucket.").param(ParamSchema::new(
                    "bucket",
                    ParamKind::String,
                    "",
                )),
                ResourceSchema::open("aws_vpc", "A network."),
            ],
            bundles: vec![],
        }
    }

    #[test]
    fn a_bundle_is_written_packed_unpacked_and_read_type_by_type() {
        let root = temp_dir("roundtrip");
        let id = BundleId::new("terraform", "hashicorp-aws", "5.60.0").unwrap();
        let header = write(&sample(), &id, &root.join("b")).unwrap();
        assert_eq!(header.types.len(), 3);
        assert_eq!(header.types[0].description, "An EC2 instance.");
        let packed = archive(&root.join("b")).unwrap();
        assert_eq!(
            packed,
            archive(&root.join("b")).unwrap(),
            "packing is deterministic"
        );

        let bundle = Bundle::unpack(&packed, &root.join("u")).unwrap();
        assert_eq!(bundle.id(), id);
        assert!(bundle.has("aws_vpc") && !bundle.has("aws_nothing"));
        let instance = bundle.load_type("aws_instance").unwrap().unwrap();
        assert!(instance.find_param("ami").unwrap().required);
        assert_eq!(bundle.load_type("aws_nothing").unwrap(), None);

        // Only the types asked for are read in full.
        let wanted = BTreeSet::from(["aws_instance".to_owned()]);
        let cache = bundle.to_cache(Some(&wanted)).unwrap();
        assert_eq!(cache.schemas.len(), 3);
        assert_eq!(cache.schemas[1].r#type, "aws_s3_bucket");
        assert!(cache.schemas[1].params.is_empty(), "named only");
        assert_eq!(cache.schemas[1].description, "A bucket.");
        assert_eq!(cache.synced_at, "", "the same on every machine");
        assert_eq!(cache.bundles, ["terraform/hashicorp-aws:5.60.0"]);
        let all = bundle.to_cache(None).unwrap();
        assert_eq!(all.schemas[1].params.len(), 1);

        // A bundle is never written over another.
        assert!(write(&sample(), &id, &root.join("b"))
            .unwrap_err()
            .contains("not empty"));
        let other = BundleId::new("pulumi", "aws", "1").unwrap();
        assert!(write(&sample(), &other, &root.join("c"))
            .unwrap_err()
            .contains("of target terraform"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bundle_signature_is_checked_by_policy() {
        let root = temp_dir("signing");
        let dir = root.join("b");
        let id = BundleId::new("terraform", "hashicorp-aws", "5.60.0").unwrap();
        write(&sample(), &id, &dir).unwrap();
        let key = SecretKey::generate().unwrap();
        let other = SecretKey::generate().unwrap();
        let trusting = |policy, key: &SecretKey| Verification {
            policy,
            trusted: vec![key.public()],
        };

        let bundle = Bundle::open(&dir).unwrap();
        assert_eq!(
            bundle.signature(&trusting(Policy::Require, &key)),
            Signature::Unsigned
        );
        assert!(bundle
            .check_signature(&trusting(Policy::Require, &key))
            .unwrap_err()
            .contains("is not signed"));
        assert!(bundle
            .check_signature(&trusting(Policy::Warn, &key))
            .unwrap()
            .unwrap()
            .contains("is not signed"));
        assert_eq!(
            bundle
                .check_signature(&trusting(Policy::Off, &key))
                .unwrap(),
            None
        );

        let key_id = sign(&dir, &key).unwrap();
        assert!(matches!(
            bundle.signature(&trusting(Policy::Require, &key)),
            Signature::Verified { key_id: k } if k == key_id
        ));
        assert!(bundle
            .check_signature(&trusting(Policy::Require, &other))
            .unwrap_err()
            .contains("not a trusted key"));

        // A changed type is a changed bundle.
        let file = dir.join(&bundle.header.types[0].file);
        std::fs::write(
            &file,
            std::fs::read_to_string(&file)
                .unwrap()
                .replace("ami", "amx"),
        )
        .unwrap();
        assert!(bundle
            .check_signature(&trusting(Policy::Require, &key))
            .unwrap_err()
            .contains("does not pass"));

        // The signature is over the bundle's name too: renamed, it fails.
        std::fs::write(
            &file,
            std::fs::read_to_string(&file)
                .unwrap()
                .replace("amx", "ami"),
        )
        .unwrap();
        let header = dir.join(BUNDLE_FILE);
        std::fs::write(
            &header,
            std::fs::read_to_string(&header)
                .unwrap()
                .replace("\"5.60.0\"", "\"5.61.0\""),
        )
        .unwrap();
        let renamed = Bundle::open(&dir).unwrap();
        assert!(matches!(
            renamed.signature(&trusting(Policy::Require, &key)),
            Signature::Invalid(_)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_a_bundle_holds_is_held_to_the_schema_model() {
        let root = temp_dir("refusals");
        let id = BundleId::new("terraform", "hashicorp-aws", "5.60.0").unwrap();
        let dir = root.join("b");
        write(&sample(), &id, &dir).unwrap();
        let header_path = dir.join(BUNDLE_FILE);
        let header = std::fs::read_to_string(&header_path).unwrap();

        // An unknown field in the header, a path that is not the type's own
        // file, and a newer format are each refused.
        for (from, to, want) in [
            (
                "\"bundleVersion\": 1",
                "\"bundleVersion\": 1, \"run\": \"x\"",
                "unknown field",
            ),
            (
                "\"bundleVersion\": 1",
                "\"bundleVersion\": 2",
                "bundle format 2",
            ),
            (
                "\"file\": \"types/",
                "\"file\": \"../",
                "not one `meridian schema pack` writes",
            ),
            (
                "\"name\": \"hashicorp-aws\"",
                "\"name\": \"../x\"",
                "cannot be a bundle name",
            ),
        ] {
            std::fs::write(&header_path, header.replacen(from, to, 1)).unwrap();
            let err = Bundle::open(&dir).unwrap_err();
            assert!(err.contains(want), "wanted {want:?} in {err}");
        }
        std::fs::write(&header_path, &header).unwrap();

        // A type file that names another type, or nests past the limit.
        let bundle = Bundle::open(&dir).unwrap();
        let file = dir.join(&bundle.header.types[0].file);
        std::fs::write(&file, r#"{"type": "aws_other"}"#).unwrap();
        assert!(bundle
            .load_type("aws_instance")
            .unwrap_err()
            .contains("describes \"aws_other\""));
        let deep = format!(
            "{{\"type\":\"aws_instance\",\"params\":{}}}",
            "[{\"name\":\"a\",\"kind\":\"mapping\",\"params\":".repeat(40)
                + "[]"
                + &"}]".repeat(40)
        );
        std::fs::write(&file, deep).unwrap();
        assert!(bundle
            .load_type("aws_instance")
            .unwrap_err()
            .contains("nests deeper"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_registry_publishes_once_and_lists_what_it_holds() {
        let root = temp_dir("dir-registry");
        let registry = Registry::Dir(root.join("registry"));
        assert_eq!(registry.index().unwrap(), Index::default());

        let mut published = Vec::new();
        for (version, extra) in [("5.59.0", false), ("5.60.0", true)] {
            let mut cached = sample();
            if !extra {
                cached.schemas.pop();
            }
            let id = BundleId::new("terraform", "hashicorp-aws", version).unwrap();
            let dir = root.join(version);
            write(&cached, &id, &dir).unwrap();
            let bundle = Bundle::open(&dir).unwrap();
            let packed = archive(&dir).unwrap();
            let first = registry.publish(&bundle, &packed, true).unwrap();
            assert!(!first.already_published);
            let again = registry.publish(&bundle, &packed, true).unwrap();
            assert!(again.already_published);
            published.push((id, first));
        }
        let index = registry.index().unwrap();
        assert_eq!(index.bundles.len(), 2);
        // The newest that describes a type comes first.
        let found = index.candidates("terraform", "aws_vpc");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].version, "5.60.0");
        let found = index.candidates("terraform", "aws_instance");
        assert_eq!(found[0].version, "5.60.0");
        assert_eq!(found[1].version, "5.59.0");
        assert!(index.candidates("pulumi", "aws_instance").is_empty());

        // Fetched by its digest, and refused under another.
        let (id, first) = &published[1];
        let (digest, bytes) = registry.fetch(id, Some(&first.digest)).unwrap();
        assert_eq!(&digest, &first.digest);
        assert_eq!(digest_of(&bytes), digest);
        let wrong = format!("sha256:{}", "0".repeat(64));
        assert!(registry
            .fetch(id, Some(&wrong))
            .unwrap_err()
            .contains("not the bundle that was pinned"));

        // Other content under a published version is refused.
        let mut cached = sample();
        cached.tool_version = "1.14.0".into();
        let dir = root.join("changed");
        write(&cached, id, &dir).unwrap();
        let err = registry
            .publish(&Bundle::open(&dir).unwrap(), &archive(&dir).unwrap(), true)
            .unwrap_err();
        assert!(err.contains("is published once"), "{err}");

        // A fetched bundle is kept by digest and opened from there next time.
        let cache_dir = root.join("cache");
        let (got, bundle) =
            open_stored(&cache_dir, Some(&first.digest), || registry.fetch(id, None)).unwrap();
        assert_eq!(got, first.digest);
        assert!(bundle.dir.starts_with(&cache_dir));
        let (_, again) = open_stored(&cache_dir, Some(&first.digest), || {
            Err("not fetched again".to_owned())
        })
        .unwrap();
        assert_eq!(again.id(), bundle.id());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn versions_names_and_references_are_read_as_written() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("5.9.0", "5.10.0"), Less);
        assert_eq!(compare_versions("1.31", "1.30"), Greater);
        assert_eq!(compare_versions("2026-11-18", "2026-11-11"), Greater);
        assert!(BundleId::new("terraform", "Hashicorp", "1").is_err());
        assert!(BundleId::new("terraform", "a", "1+x").is_err());
        assert_eq!(
            BundleId::new("kubernetes", "core", "1.31").unwrap().label(),
            "kubernetes/core:1.31"
        );
        match Reference::parse("oci://ghcr.io/acme/schemas/terraform/hashicorp-aws:5.60.0").unwrap()
        {
            Reference::Oci { prefix, id } => {
                assert_eq!(prefix, "ghcr.io/acme/schemas");
                assert_eq!(id.label(), "terraform/hashicorp-aws:5.60.0");
            }
            Reference::File(_) => panic!("an oci reference"),
        }
        for bad in [
            "oci://ghcr.io/terraform:1",
            "oci://a/b/c",
            "oci://ghcr.io/a/t/n:1/x",
        ] {
            assert!(Reference::parse(bad).is_err(), "{bad}");
        }
        assert!(matches!(
            Reference::parse("file://bundles/aws.tar.gz").unwrap(),
            Reference::File(p) if p == Path::new("bundles/aws.tar.gz")
        ));
        assert_eq!(first_line("\n  one\ntwo"), "one");
    }

    #[test]
    fn bundles_of_one_target_merge_with_full_descriptions_winning() {
        let mut a = sample();
        a.bundles = vec!["terraform/hashicorp-aws:5.60.0".into()];
        a.schemas[0] = ResourceSchema::open("aws_instance", "named only");
        let mut b = sample();
        b.pins = BTreeMap::from([(
            "registry.terraform.io/hashicorp/google".into(),
            "6.0.0".into(),
        )]);
        b.bundles = vec!["terraform/hashicorp-google:6.0.0".into()];
        let merged = merge("terraform", vec![a, b]);
        assert_eq!(merged.pins.len(), 2);
        assert_eq!(merged.bundles.len(), 2);
        let instance = merged
            .schemas
            .iter()
            .find(|s| s.r#type == "aws_instance")
            .unwrap();
        assert_eq!(instance.params.len(), 1, "the full description won");
    }
}
