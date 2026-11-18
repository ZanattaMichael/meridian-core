//! `meridian schema pack`, `push` and `fetch`, and how `schema sync` gets a
//! target's schemas from signed bundles before it asks the tool.
//!
//! The bundle format, the registries and the checks live in
//! `meridian_tooling::bundles`; this is the command line around them: which
//! registries a project names, which bundles describe what its documents
//! use, and what is written to the cache and to `meridian.lock`.

use std::collections::BTreeSet;
use std::path::Path;

use meridian_module::{LockedBundle, LockedSchemas, Manifest, MANIFEST_FILE};
use meridian_plugin_host::{Policy as SignaturePolicy, Verification};
use meridian_tooling::bundles::{self, Bundle, BundleId, Reference, Registry};
use meridian_tooling::schemas::{Cached, Store};

use crate::{count, trusted_keys, Failure};

/// The project's manifest, when there is one beside the documents.
pub(crate) fn project_manifest(project: &Path) -> Result<Option<Manifest>, Failure> {
    if !project.join(MANIFEST_FILE).is_file() {
        return Ok(None);
    }
    Manifest::load(project)
        .map(Some)
        .map_err(|e| Failure::Message(e.to_string()))
}

/// The signature policy for bundles: the command line's, else the
/// manifest's `spec.schemas.signatures`, else off, as for modules.
pub(crate) fn verification(
    flag: Option<SignaturePolicy>,
    manifest: Option<&Manifest>,
) -> Result<Verification, Failure> {
    let policy = flag
        .or_else(|| manifest.and_then(|m| m.schemas.signatures))
        .unwrap_or(SignaturePolicy::Off);
    let trusted = match policy {
        SignaturePolicy::Off => Vec::new(),
        _ => trusted_keys()?,
    };
    Ok(Verification { policy, trusted })
}

/// The registries `spec.schemas` names, in order, each with its login.
pub(crate) fn registries(
    project: &Path,
    manifest: Option<&Manifest>,
) -> Result<Vec<Registry>, Failure> {
    let Some(manifest) = manifest else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for named in &manifest.schemas.registries {
        out.push(open_registry(
            project,
            Some(manifest),
            &named.location,
            named.credential.as_deref(),
            named.insecure,
        )?);
    }
    Ok(out)
}

/// A registry as written on the command line or in `spec.schemas`:
/// `oci://PREFIX`, a bare `PREFIX`, or `file://DIR`.
pub(crate) fn open_registry(
    project: &Path,
    manifest: Option<&Manifest>,
    location: &str,
    credential: Option<&str>,
    insecure: bool,
) -> Result<Registry, Failure> {
    if let Some(dir) = location.strip_prefix("file://") {
        if credential.is_some() {
            return Err(Failure::Message(
                "a file:// registry takes no credential".to_owned(),
            ));
        }
        let dir = Path::new(dir);
        return Ok(Registry::Dir(if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            project.join(dir)
        }));
    }
    let prefix = location.strip_prefix("oci://").unwrap_or(location);
    let login = match (credential, manifest) {
        (None, _) => None,
        (Some(_), None) => {
            return Err(Failure::Message(format!(
                "a credential is named from spec.credentials, and there is no {MANIFEST_FILE} \
                 here"
            )))
        }
        (Some(name), Some(manifest)) => Some(
            meridian_module::resolve_login(manifest, name)
                .map_err(|e| Failure::Message(e.to_string()))?,
        ),
    };
    Ok(Registry::Oci {
        prefix: prefix.trim_end_matches('/').to_owned(),
        insecure,
        login,
    })
}

/// Fetches the bundle a lock entry names, by its digest, from the store when
/// it is there. A registry the project names lends its login and its
/// `insecure` setting to a reference under it.
fn fetch_locked(
    store: &Store,
    locked: &LockedBundle,
    registries: &[Registry],
) -> Result<Bundle, String> {
    let (_, bundle) = bundles::open_stored(store.dir(), Some(&locked.digest), || {
        if let Some(path) = locked.reference.strip_prefix("file+") {
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            return Ok((bundles::digest_of(&bytes), bytes));
        }
        let rest = locked
            .reference
            .strip_prefix("oci+")
            .ok_or_else(|| format!("{} is not a bundle reference", locked.reference))?;
        let Reference::Oci { prefix, id } = Reference::parse(&format!("oci://{rest}"))? else {
            return Err(format!("{} is not a bundle reference", locked.reference));
        };
        let named = registries.iter().find(|r| match r {
            Registry::Oci { prefix: p, .. } => *p == prefix,
            Registry::Dir(_) => false,
        });
        match named {
            Some(registry) => registry.fetch(&id, Some(&locked.digest)),
            None => Registry::Oci {
                prefix,
                insecure: false,
                login: None,
            }
            .fetch(&id, Some(&locked.digest)),
        }
    })?;
    Ok(bundle)
}

/// How `sync` treats the lock for bundles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolve {
    /// Keep the bundles the lock pins while they still describe what the
    /// documents use; add bundles for what they do not.
    Keep,
    /// Use exactly the bundles the lock pins, or fail.
    Locked,
    /// Choose again, from the indexes, the newest bundles.
    Update,
}

/// What [`from_bundles`] decided.
pub(crate) enum FromBundles {
    /// The cache, the lock entry's bundles, and warnings to show.
    Cached(Box<Cached>, Vec<LockedBundle>, Vec<String>),
    /// No bundle set describes every type; ask the tool. The text says why.
    UseTool(String),
}

/// Gets one target's schemas from bundles: the ones the lock pins, then the
/// newest a registry's index lists for each type the documents use that
/// those do not describe.
pub(crate) fn from_bundles(
    store: &Store,
    target: &str,
    needed: &BTreeSet<String>,
    registries: &[Registry],
    locked: Option<&LockedSchemas>,
    resolve: Resolve,
    trust: &Verification,
) -> Result<FromBundles, String> {
    let mut chosen: Vec<(Bundle, LockedBundle)> = Vec::new();
    let mut warnings = Vec::new();
    let mut uncovered: BTreeSet<String> = needed.clone();
    let pinned = locked.map(|l| l.bundles.as_slice()).unwrap_or_default();

    let take = |bundle: Bundle,
                entry: LockedBundle,
                chosen: &mut Vec<(Bundle, LockedBundle)>,
                uncovered: &mut BTreeSet<String>,
                warnings: &mut Vec<String>|
     -> Result<(), String> {
        if bundle.header.target != target {
            return Err(format!(
                "{} is a bundle of target {}, not {target}",
                entry.reference, bundle.header.target
            ));
        }
        if let Some(warning) = bundle.check_signature(trust)? {
            warnings.push(warning);
        }
        uncovered.retain(|t| !bundle.has(t));
        chosen.push((bundle, entry));
        Ok(())
    };

    if resolve != Resolve::Update {
        for entry in pinned {
            let bundle = fetch_locked(store, entry, registries).map_err(|e| {
                format!(
                    "the bundle {} that meridian.lock pins: {e}",
                    entry.reference
                )
            })?;
            take(
                bundle,
                entry.clone(),
                &mut chosen,
                &mut uncovered,
                &mut warnings,
            )?;
        }
    }
    if resolve == Resolve::Locked {
        if pinned.is_empty() {
            return Ok(FromBundles::UseTool(
                "meridian.lock pins no schema bundle for it".to_owned(),
            ));
        }
        if !uncovered.is_empty() {
            return Err(format!(
                "the bundles meridian.lock pins do not describe {}; run `meridian schema sync` \
                 without --locked and commit meridian.lock",
                list(&uncovered)
            ));
        }
    } else if !uncovered.is_empty() {
        if registries.is_empty() {
            return Ok(FromBundles::UseTool(format!(
                "no schema registry is named in spec.schemas, and the documents use {}",
                list(&uncovered)
            )));
        }
        let mut indexes = Vec::new();
        for registry in registries {
            match registry.index() {
                Ok(index) => indexes.push((registry, index)),
                Err(e) => warnings.push(format!(
                    "the schema registry {} could not be read: {e}",
                    registry.describe()
                )),
            }
        }
        let wanted: Vec<String> = uncovered.iter().cloned().collect();
        for type_name in wanted {
            if !uncovered.contains(&type_name) {
                continue;
            }
            let found = indexes.iter().find_map(|(registry, index)| {
                index
                    .candidates(target, &type_name)
                    .first()
                    .map(|listed| (*registry, (*listed).clone()))
            });
            let Some((registry, listed)) = found else {
                continue;
            };
            let id = listed.id();
            let (digest, bundle) = bundles::open_stored(store.dir(), Some(&listed.digest), || {
                registry.fetch(&id, Some(&listed.digest))
            })
            .map_err(|e| format!("the bundle {}: {e}", id.label()))?;
            let entry = LockedBundle {
                reference: registry.reference(&id),
                digest,
            };
            take(bundle, entry, &mut chosen, &mut uncovered, &mut warnings)?;
        }
        if !uncovered.is_empty() {
            return Ok(FromBundles::UseTool(format!(
                "no schema bundle describes {}",
                list(&uncovered)
            )));
        }
    }
    if chosen.is_empty() {
        return Ok(FromBundles::UseTool(
            "the documents use no type a tool describes".to_owned(),
        ));
    }
    // A pinned bundle that no longer describes anything the documents use is
    // dropped, so the lock does not keep what the project stopped needing.
    if !needed.is_empty() {
        chosen.retain(|(bundle, _)| needed.iter().any(|t| bundle.has(t)));
    }
    let mut caches = Vec::new();
    for (bundle, _) in &chosen {
        caches.push(bundle.to_cache(Some(needed))?);
    }
    let mut entries: Vec<LockedBundle> = chosen.into_iter().map(|(_, e)| e).collect();
    entries.sort();
    entries.dedup();
    Ok(FromBundles::Cached(
        Box::new(bundles::merge(target, caches)),
        entries,
        warnings,
    ))
}

/// The first few names of a set, for a message.
fn list(names: &BTreeSet<String>) -> String {
    let shown: Vec<&str> = names.iter().take(5).map(String::as_str).collect();
    let more = names.len().saturating_sub(shown.len());
    if more == 0 {
        shown.join(", ")
    } else {
        format!("{} and {}", shown.join(", "), count(more, "other type"))
    }
}

/// The lock entry for a cache made from bundles.
pub(crate) fn lock_entry(
    cached: &Cached,
    integrity: &str,
    bundles: Vec<LockedBundle>,
) -> LockedSchemas {
    let mut entry = cached.lock_entry(integrity);
    entry.bundles = bundles;
    entry
}

/// The name and version a bundle is packed as.
pub(crate) struct BundleNaming<'a> {
    pub(crate) name: &'a str,
    pub(crate) version: &'a str,
}

/// `meridian schema pack`.
pub(crate) fn pack(
    state_dir: &Path,
    target: &str,
    naming: &BundleNaming<'_>,
    prefixes: &[String],
    out: Option<&Path>,
    key: Option<&Path>,
) -> Result<(), Failure> {
    let (name, version) = (naming.name, naming.version);
    let id = BundleId::new(target, name, version).map_err(Failure::Message)?;
    let store = Store::new(state_dir);
    let Some(mut cached) = store.load(target).map_err(Failure::Message)? else {
        return Err(Failure::Message(format!(
            "nothing is cached for {target}; run `meridian schema sync --all-types` first"
        )));
    };
    if !prefixes.is_empty() {
        cached
            .schemas
            .retain(|s| prefixes.iter().any(|p| s.r#type.starts_with(p.as_str())));
        if cached.schemas.is_empty() {
            return Err(Failure::Message(format!(
                "no cached type of {target} starts with {}",
                prefixes.join(" or ")
            )));
        }
    }
    let named = cached
        .schemas
        .iter()
        .filter(|s| s.open && s.params.is_empty())
        .count();
    if named > 0 {
        eprintln!(
            "note: {} listed by name only; a bundle is most use with every type described: \
             sync with --all-types before packing",
            count(named, "type is")
        );
    }
    let work = state_dir
        .join(meridian_tooling::schemas::CACHE_DIR)
        .join(format!("packing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let result = (|| {
        let header = bundles::write(&cached, &id, &work)?;
        let signed = match key {
            Some(path) => {
                let key = meridian_module::SecretKey::read(path).map_err(String::from)?;
                Some(bundles::sign(&work, &key)?)
            }
            None => None,
        };
        let archive = bundles::archive(&work)?;
        Ok::<_, String>((header, signed, archive))
    })();
    let _ = std::fs::remove_dir_all(&work);
    let (header, signed, archive) = result.map_err(Failure::Message)?;
    let out = out.map(Path::to_path_buf).unwrap_or_else(|| {
        state_dir
            .join(meridian_tooling::schemas::CACHE_DIR)
            .join("packed")
            .join(format!("{target}-{name}-{version}.tar.gz"))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| Failure::Message(format!("{}: {e}", parent.display())))?;
    }
    std::fs::write(&out, &archive)
        .map_err(|e| Failure::Message(format!("writing {}: {e}", out.display())))?;
    println!(
        "packed {} ({} from {}) into {}",
        id.label(),
        count(header.types.len(), "resource type"),
        cached.versions(),
        out.display()
    );
    println!("  digest:  {}", bundles::digest_of(&archive));
    match signed {
        Some(key_id) => println!("  signed:  key {key_id}"),
        None => println!("  signed:  no (give --key to sign it)"),
    }
    Ok(())
}

/// `meridian schema sign`.
pub(crate) fn sign(file: &Path, key: &Path, out: Option<&Path>) -> Result<(), Failure> {
    let archive = std::fs::read(file)
        .map_err(|e| Failure::Message(format!("reading {}: {e}", file.display())))?;
    let key = meridian_module::SecretKey::read(key).map_err(|e| Failure::Message(e.into()))?;
    let work = std::env::temp_dir().join(format!("meridian-schema-sign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let result = Bundle::unpack(&archive, &work).and_then(|bundle| {
        let key_id = bundles::sign(&work, &key)?;
        Ok((bundle.id(), key_id, bundles::archive(&work)?))
    });
    let _ = std::fs::remove_dir_all(&work);
    let (id, key_id, signed) =
        result.map_err(|e| Failure::Message(format!("{}: {e}", file.display())))?;
    let out = out.unwrap_or(file);
    std::fs::write(out, &signed)
        .map_err(|e| Failure::Message(format!("writing {}: {e}", out.display())))?;
    println!(
        "signed {} with key {key_id} into {}",
        id.label(),
        out.display()
    );
    println!("  digest:  {}", bundles::digest_of(&signed));
    Ok(())
}

/// `meridian schema push`.
pub(crate) fn push(
    file: &Path,
    location: &str,
    credential: Option<&str>,
    insecure: bool,
    index: bool,
) -> Result<(), Failure> {
    let archive = std::fs::read(file)
        .map_err(|e| Failure::Message(format!("reading {}: {e}", file.display())))?;
    let project = Path::new(".");
    let manifest = project_manifest(project)?;
    let registry = open_registry(project, manifest.as_ref(), location, credential, insecure)?;
    let work = std::env::temp_dir().join(format!("meridian-schema-push-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let result = Bundle::unpack(&archive, &work).and_then(|bundle| {
        let published = registry.publish(&bundle, &archive, index)?;
        Ok((bundle.id(), bundle.header.types.len(), published))
    });
    let _ = std::fs::remove_dir_all(&work);
    let (id, types, published) =
        result.map_err(|e| Failure::Message(format!("{}: {e}", file.display())))?;
    if published.already_published {
        println!(
            "{} is already published in {}, with this content",
            id.label(),
            registry.describe()
        );
    } else {
        println!(
            "published {} ({}) to {}",
            id.label(),
            count(types, "resource type"),
            registry.describe()
        );
    }
    println!("  reference: {}", published.reference);
    println!("  digest:    {}", published.digest);
    if index {
        println!("  listed in the registry's index");
    }
    Ok(())
}

/// `meridian schema fetch`.
pub(crate) fn fetch(
    state_dir: &Path,
    reference: &str,
    wanted: Option<BTreeSet<String>>,
    flag: Option<SignaturePolicy>,
    write_lock: bool,
    credential: Option<&str>,
    insecure: bool,
) -> Result<(), Failure> {
    let project = Path::new(".");
    let manifest = project_manifest(project)?;
    let trust = verification(flag, manifest.as_ref())?;
    let store = Store::new(state_dir);
    let named = registries(project, manifest.as_ref())?;
    let (bundle, entry) = match Reference::parse(reference).map_err(Failure::Message)? {
        Reference::File(path) => {
            let entry_reference = format!("file+{}", path.display());
            let (digest, bundle) = bundles::open_stored(store.dir(), None, || {
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok((bundles::digest_of(&bytes), bytes))
            })
            .map_err(Failure::Message)?;
            (
                bundle,
                LockedBundle {
                    reference: entry_reference,
                    digest,
                },
            )
        }
        Reference::Oci { prefix, id } => {
            let declared = named
                .into_iter()
                .find(|r| matches!(r, Registry::Oci { prefix: p, .. } if *p == prefix));
            let registry = match declared {
                Some(registry) if credential.is_none() && !insecure => registry,
                _ => open_registry(project, manifest.as_ref(), &prefix, credential, insecure)?,
            };
            let (digest, bundle) =
                bundles::open_stored(store.dir(), None, || registry.fetch(&id, None))
                    .map_err(Failure::Message)?;
            if bundle.id() != id {
                return Err(Failure::Message(format!(
                    "{reference} holds the bundle {}, not {}",
                    bundle.id().label(),
                    id.label()
                )));
            }
            (
                bundle,
                LockedBundle {
                    reference: registry.reference(&id),
                    digest,
                },
            )
        }
    };
    if let Some(warning) = bundle.check_signature(&trust).map_err(Failure::Message)? {
        eprintln!("warning: {warning}");
    }
    let cached = bundle.to_cache(wanted.as_ref()).map_err(Failure::Message)?;
    let target = cached.target.clone();
    let integrity = store.save(&cached).map_err(Failure::Message)?;
    let described = cached
        .schemas
        .iter()
        .filter(|s| !s.params.is_empty() || !s.open)
        .count();
    println!(
        "{target}: cached {} ({} in full) from the bundle {} in {}",
        count(cached.schemas.len(), "resource type"),
        described,
        bundle.id().label(),
        store.path(&target).display()
    );
    println!("  digest: {}", entry.digest);
    if write_lock {
        let mut lock = meridian_module::Lock::load(project)
            .map_err(|e| Failure::Message(e.to_string()))?
            .unwrap_or_default();
        lock.set_schemas(lock_entry(&cached, &integrity, vec![entry]));
        let path = project.join(meridian_module::LOCK_FILE);
        std::fs::write(&path, lock.render())
            .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;
        println!("recorded in {}", meridian_module::LOCK_FILE);
    }
    Ok(())
}

/// The types the documents of each target use that a tool would describe:
/// every type, less the ones the plugin itself describes param by param.
pub(crate) fn needed_types(
    graphs: &[meridian_sdk::ResourceGraph],
    plugin_describes: impl Fn(&str) -> bool,
) -> BTreeSet<String> {
    graphs
        .iter()
        .flat_map(|g| g.resources.iter().map(|r| r.r#type.clone()))
        .filter(|t| !plugin_describes(t))
        .collect()
}
