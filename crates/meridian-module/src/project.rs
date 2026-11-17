//! A project's modules: resolving what its manifest requires, fetching it,
//! pinning it, and opening the pinned modules for use.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use semver::{Version, VersionReq};

use crate::integrity::hash_dir;
use crate::lock::{Lock, Locked, LOCK_FILE};
use crate::manifest::{safe_relative, Manifest, Source, MANIFEST_FILE};
use meridian_signing::{Signature, Verification};

use crate::{git, oci, signature, Error};

/// What opens a reference to a file in a module: `@acme/nginx:documents/web.yaml`.
pub const MODULE_REFERENCE_PREFIX: char = '@';

/// Where a project is and where its fetched modules are kept.
#[derive(Clone, Copy, Debug)]
pub struct Project<'a> {
    /// The directory holding the project's `meridian.yaml`.
    pub dir: &'a Path,
    /// Meridian's state directory; fetched modules live under `modules/` in it.
    pub state_dir: &'a Path,
    /// The core release doing the work, checked against each module's
    /// `spec.meridian`.
    pub core_version: &'a str,
    /// Whether fetched modules must be signed, and by whom. A `path` module
    /// is the project's own working copy and is never checked.
    pub signatures: &'a Verification,
}

/// What [`install`] is allowed to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keep every pinned version that still fits; resolve the rest.
    Install,
    /// As [`Mode::Install`], but fail if the lockfile would change.
    Locked,
    /// Move the named modules, or every module when none is named, to the
    /// newest version that fits.
    Update(Vec<String>),
}

/// What happened to one module's pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    /// The pin moved; `from` is the version it had, or what it resolved to
    /// when the version did not change.
    Updated {
        from: String,
    },
    Unchanged,
    /// Nothing requires it any more.
    Removed,
}

/// The result of [`install`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub lock: Lock,
    /// Every module in the old or the new lockfile, by name.
    pub changes: Vec<(Locked, Change)>,
    /// Whether `meridian.lock` was written.
    pub wrote_lock: bool,
    /// What checking each fetched module's signature found, by module name.
    /// Empty when signatures are off.
    pub signatures: BTreeMap<String, Signature>,
}

impl Report {
    /// A line for each module that was installed although its signature did
    /// not pass, which only the `warn` policy allows.
    pub fn warnings(&self) -> Vec<String> {
        warnings(&self.lock.modules, &self.signatures)
    }
}

fn warnings(modules: &[Locked], signatures: &BTreeMap<String, Signature>) -> Vec<String> {
    modules
        .iter()
        .filter_map(|m| signature::warning(&label(m), signatures.get(&m.name)?))
        .collect()
}

/// One pinned module and what, if anything, is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub module: Locked,
    pub problem: Option<String>,
    /// What its signature says. `NotChecked` when signatures are off, for a
    /// `path` module, and when `problem` is about something else.
    pub signature: Signature,
}

fn label(locked: &Locked) -> String {
    if locked.version.is_empty() {
        locked.name.clone()
    } else {
        format!("{} {}", locked.name, locked.version)
    }
}

/// Resolves `.` and `..` in a path without touching the disk, with `/`
/// between the parts.
fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => prefix = p.as_os_str().to_string_lossy().into_owned(),
            Component::RootDir => prefix.push('/'),
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(last) if last != ".." => {
                    parts.pop();
                }
                _ if prefix.is_empty() => parts.push("..".to_owned()),
                _ => {}
            },
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
        }
    }
    let joined = parts.join("/");
    match (prefix.is_empty(), joined.is_empty()) {
        (true, true) => ".".to_owned(),
        (true, false) => joined,
        (false, _) => format!("{prefix}{joined}"),
    }
}

/// Where a fetched module is kept.
fn store_dir(state_dir: &Path, locked: &Locked) -> PathBuf {
    let leaf = if locked.version.is_empty() {
        let short: String = locked
            .resolved
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(12)
            .collect();
        format!("ref-{short}")
    } else {
        // `+` and build metadata are fine in a directory name.
        locked.version.clone()
    };
    let mut dir = state_dir.join("modules");
    dir.extend(locked.name.split('/'));
    dir.join(leaf)
}

/// A module that resolution has settled on.
struct Node {
    manifest: Manifest,
    locked: Locked,
    /// For a `path` module, its directory relative to the project, which is
    /// what its own `path` requirements are relative to.
    rel_dir: Option<String>,
}

/// One requirement on a module, and who made it.
struct Demand {
    version: Option<VersionReq>,
    by: String,
    source: Source,
    /// The source as the lockfile will record it.
    locator: String,
}

/// The modules required, in the order they were first met, and every
/// requirement on each.
type Demands = (Vec<String>, BTreeMap<String, Vec<Demand>>);

struct Resolver<'a> {
    project: &'a Project<'a>,
    root: Manifest,
    previous: Lock,
    /// The modules whose old pin may not be reused. `None` means every one.
    refresh: Option<BTreeSet<String>>,
    chosen: BTreeMap<String, Node>,
    listings: HashMap<String, BTreeMap<Version, String>>,
}

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

impl Resolver<'_> {
    fn reuse_pin(&self, name: &str) -> bool {
        match &self.refresh {
            None => false,
            Some(names) => !names.contains(name),
        }
    }

    /// Walks the requirements from the project through every chosen module.
    fn demands(&self) -> Result<Demands, Error> {
        let mut order = Vec::new();
        let mut demands: BTreeMap<String, Vec<Demand>> = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();
        queue.push_back((
            "the project".to_owned(),
            Some(String::new()),
            self.root.modules.clone(),
        ));
        while let Some((by, rel_dir, requirements)) = queue.pop_front() {
            for requirement in requirements {
                let locator = match &requirement.source {
                    Source::Path(path) => match &rel_dir {
                        Some(base) => {
                            format!("path+{}", normalize(&Path::new(base).join(path)))
                        }
                        None => {
                            return Err(Error::new(format!(
                                "{by} requires {} by `path`, which only works between modules \
                                 on disk; a fetched module must name a `git` or `oci` source",
                                requirement.name
                            )))
                        }
                    },
                    other => other.locator(),
                };
                let name = requirement.name.clone();
                if !demands.contains_key(&name) {
                    order.push(name.clone());
                }
                demands.entry(name.clone()).or_default().push(Demand {
                    version: requirement.version,
                    by: by.clone(),
                    source: requirement.source,
                    locator,
                });
                if let Some(node) = self.chosen.get(&name) {
                    if visited.insert(name) {
                        queue.push_back((
                            label(&node.locked),
                            node.rel_dir.clone(),
                            node.manifest.modules.clone(),
                        ));
                    }
                }
            }
        }
        Ok((order, demands))
    }

    fn satisfied(node: &Node, demands: &[Demand]) -> bool {
        let version = Version::parse(&node.locked.version).ok();
        demands.iter().all(|d| {
            d.locator == node.locked.source
                && match (&d.version, &version) {
                    (None, _) => true,
                    (Some(req), Some(v)) => req.matches(v),
                    (Some(_), None) => false,
                }
        })
    }

    /// Everything `name` requires, directly or not, among the chosen.
    fn below(&self, name: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut pending = vec![name.to_owned()];
        while let Some(next) = pending.pop() {
            if let Some(node) = self.chosen.get(&next) {
                for requirement in &node.manifest.modules {
                    if out.insert(requirement.name.clone()) {
                        pending.push(requirement.name.clone());
                    }
                }
            }
        }
        out
    }

    fn resolve(&mut self) -> Result<Lock, Error> {
        // Every pass either settles one more module or ends. The bound is
        // only there so a bug cannot become a hang.
        for _ in 0..10_000 {
            let (order, demands) = self.demands()?;
            let mut first_error = None;
            let mut progressed = false;
            for name in &order {
                let wanted = &demands[name];
                if self
                    .chosen
                    .get(name)
                    .is_some_and(|node| Resolver::satisfied(node, wanted))
                {
                    continue;
                }
                match self.choose(name, wanted) {
                    Ok(node) => {
                        // What the replaced version required may not be
                        // required any more; it is settled again from scratch.
                        for stale in self.below(name) {
                            self.chosen.remove(&stale);
                        }
                        self.chosen.insert(name.clone(), node);
                        progressed = true;
                        break;
                    }
                    // A requirement may come from a module that is itself
                    // about to be replaced, so try the others first.
                    Err(e) => first_error = first_error.or(Some(e)),
                }
            }
            if progressed {
                continue;
            }
            if let Some(e) = first_error {
                return Err(e);
            }
            let mut modules: Vec<Locked> = order
                .iter()
                .map(|name| self.chosen[name].locked.clone())
                .collect();
            modules.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(Lock { modules });
        }
        Err(Error::new(
            "the module requirements did not settle; this is a bug in meridian",
        ))
    }

    fn conflict(
        name: &str,
        demands: &[Demand],
        available: Option<&BTreeMap<Version, String>>,
    ) -> Error {
        let mut msg = format!("no version of {name} satisfies every requirement on it");
        let width = demands
            .iter()
            .map(|d| d.version.as_ref().map_or(1, |v| v.to_string().len()))
            .max()
            .unwrap_or(1);
        for d in demands {
            let req = d
                .version
                .as_ref()
                .map_or("*".to_owned(), ToString::to_string);
            msg.push_str(&format!("\n    {req:<width$}   required by {}", d.by));
        }
        if let Some(available) = available {
            let list: Vec<String> = available.keys().map(ToString::to_string).collect();
            msg.push_str(&format!(
                "\n    available: {}",
                if list.is_empty() {
                    "none: the source has no tags that are versions".to_owned()
                } else {
                    list.join(", ")
                }
            ));
        }
        Error::new(msg)
    }

    fn choose(&mut self, name: &str, demands: &[Demand]) -> Result<Node, Error> {
        let first = &demands[0];
        if let Some(other) = demands.iter().find(|d| d.locator != first.locator) {
            return Err(Error::new(format!(
                "{name} is required from two different sources\n    {}   required by {}\n    {}   required by {}",
                first.locator, first.by, other.locator, other.by
            )));
        }
        let fits = |version: &Version| {
            demands
                .iter()
                .all(|d| d.version.as_ref().is_none_or(|req| req.matches(version)))
        };

        // A `path` module is read where it is.
        if let Source::Path(_) = &first.source {
            let rel_dir = first.locator["path+".len()..].to_owned();
            let dir = self.project.dir.join(&rel_dir);
            let manifest = Manifest::load(&dir)?;
            let locked = Locked {
                name: name.to_owned(),
                version: manifest
                    .version
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                source: first.locator.clone(),
                requires: manifest.modules.iter().map(|m| m.name.clone()).collect(),
                ..Locked::default()
            };
            if let Some(d) = demands.iter().find(|d| d.version.is_some()) {
                let ok = manifest.version.as_ref().is_some_and(fits);
                if !ok {
                    let has = manifest
                        .version
                        .as_ref()
                        .map_or("no version".to_owned(), |v| format!("version {v}"));
                    return Err(Error::new(format!(
                        "{name} at {} has {has}, and {} requires {}",
                        dir.display(),
                        d.by,
                        d.version
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default()
                    )));
                }
            }
            self.accept(name, &manifest, &locked)?;
            return Ok(Node {
                manifest,
                locked,
                rel_dir: Some(rel_dir),
            });
        }

        // An old pin is kept for as long as it still fits.
        let pinned = self
            .previous
            .get(name)
            .filter(|p| self.reuse_pin(name) && p.source == first.locator)
            .filter(|p| match Version::parse(&p.version) {
                Ok(v) => fits(&v),
                Err(_) => demands.iter().all(|d| d.version.is_none()),
            })
            .cloned();
        if let Some(pin) = pinned {
            let dir = store_dir(self.project.state_dir, &pin);
            if !(dir.is_dir() && hash_dir(&dir).is_ok_and(|h| h == pin.integrity)) {
                let found = self.fetch(&first.source, &pin.resolved, &pin)?;
                if found.integrity != pin.integrity {
                    return Err(Error::new(format!(
                        "{}: what {} now serves does not match {LOCK_FILE}\n    pinned:  {}\n    \
                         fetched: {}\nThe source changed after it was pinned. If that is \
                         expected, run `meridian module update {name}`.",
                        label(&pin),
                        pin.source,
                        pin.integrity,
                        found.integrity
                    )));
                }
            }
            let manifest = Manifest::load(&dir)?;
            let mut locked = pin;
            locked.requires = manifest.modules.iter().map(|m| m.name.clone()).collect();
            self.accept(name, &manifest, &locked)?;
            return Ok(Node {
                manifest,
                locked,
                rel_dir: None,
            });
        }

        // Otherwise the newest version that fits, or the one `ref`.
        let (reference, version) = match &first.source {
            Source::Git {
                reference: Some(reference),
                ..
            } => (reference.clone(), None),
            source => {
                let listing = match self.listings.get(&first.locator) {
                    Some(listing) => listing.clone(),
                    None => {
                        let listing = match source {
                            Source::Git { url, .. } => git::versions(url)?,
                            Source::Oci {
                                repository,
                                insecure,
                            } => oci::versions(repository, *insecure)?,
                            Source::Path(_) => unreachable!("a path module was handled above"),
                        };
                        self.listings.insert(first.locator.clone(), listing.clone());
                        listing
                    }
                };
                match listing.iter().rev().find(|(v, _)| fits(v)) {
                    Some((version, tag)) => (tag.clone(), Some(version.clone())),
                    None => return Err(Resolver::conflict(name, demands, Some(&listing))),
                }
            }
        };
        let wanted = Locked {
            name: name.to_owned(),
            version: version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            source: first.locator.clone(),
            ..Locked::default()
        };
        let locked = self.fetch(&first.source, &reference, &wanted)?;
        let dir = store_dir(self.project.state_dir, &locked);
        let manifest = Manifest::load(&dir)?;
        let mut locked = locked;
        locked.requires = manifest.modules.iter().map(|m| m.name.clone()).collect();
        self.accept(name, &manifest, &locked)?;
        Ok(Node {
            manifest,
            locked,
            rel_dir: None,
        })
    }

    /// Checks that what was found is the module that was asked for, and that
    /// it works with this core.
    fn accept(&self, name: &str, manifest: &Manifest, locked: &Locked) -> Result<(), Error> {
        if manifest.name != name {
            return Err(Error::new(format!(
                "{} holds the module {}, and it was required as {name}",
                locked.source, manifest.name
            )));
        }
        let declared = manifest
            .version
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        if !locked.source.starts_with("path+")
            && !locked.version.is_empty()
            && declared != locked.version
        {
            return Err(Error::new(format!(
                "{name}: the version {} of {} holds a manifest that says {}",
                locked.version,
                locked.source,
                if declared.is_empty() {
                    "no version"
                } else {
                    &declared
                }
            )));
        }
        check_core(&label(locked), manifest, self.project.core_version)
    }

    /// Fetches one module into the store and returns its pin. `reference` is
    /// a tag, a branch, a commit or a manifest digest.
    fn fetch(&self, source: &Source, reference: &str, wanted: &Locked) -> Result<Locked, Error> {
        let scratch = self
            .project
            .state_dir
            .join("modules")
            .join(".tmp")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&scratch);
        let out = scratch.join("module");
        let result = (|| {
            fs::create_dir_all(&scratch)
                .map_err(|e| Error::new(format!("creating {}: {e}", scratch.display())))?;
            let resolved = match source {
                Source::Git { url, subdir, .. } => {
                    git::fetch(url, reference, subdir.as_deref(), &scratch, &out)?
                }
                Source::Oci {
                    repository,
                    insecure,
                } => oci::fetch(repository, *insecure, reference, &out)?,
                Source::Path(_) => unreachable!("a path module is never fetched"),
            };
            let locked = Locked {
                resolved,
                integrity: hash_dir(&out)?,
                ..wanted.clone()
            };
            let dir = store_dir(self.project.state_dir, &locked);
            let moving = |e: std::io::Error| Error::new(format!("writing {}: {e}", dir.display()));
            let _ = fs::remove_dir_all(&dir);
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent).map_err(moving)?;
            }
            fs::rename(&out, &dir).map_err(moving)?;
            Ok(locked)
        })();
        let _ = fs::remove_dir_all(&scratch);
        result.map_err(|e: Error| Error::new(format!("{}: {e}", label(wanted))))
    }
}

fn check_core(who: &str, manifest: &Manifest, core_version: &str) -> Result<(), Error> {
    let (Some(req), Ok(core)) = (&manifest.meridian, Version::parse(core_version)) else {
        return Ok(());
    };
    if req.matches(&core) {
        return Ok(());
    }
    Err(Error::new(format!(
        "{who} needs meridian {req}, and this is meridian {core}"
    )))
}

fn write_atomically(path: &Path, content: &str) -> Result<(), Error> {
    let failed = |e: std::io::Error| Error::new(format!("writing {}: {e}", path.display()));
    let mut staged = path.as_os_str().to_owned();
    staged.push(".tmp");
    let staged = PathBuf::from(staged);
    fs::write(&staged, content).map_err(failed)?;
    fs::rename(&staged, path).map_err(|e| {
        let _ = fs::remove_file(&staged);
        failed(e)
    })
}

/// Resolves what the project's manifest requires, fetches what is missing,
/// and writes `meridian.lock`.
pub fn install(project: &Project<'_>, mode: Mode) -> Result<Report, Error> {
    let root = Manifest::load(project.dir)?;
    check_core("the project", &root, project.core_version)?;
    let previous = Lock::load(project.dir)?;
    if mode == Mode::Locked && previous.is_none() && !root.modules.is_empty() {
        return Err(Error::new(format!(
            "there is no {LOCK_FILE} to hold to; run `meridian module install` and commit it"
        )));
    }
    let previous = previous.unwrap_or_default();
    let refresh = match &mode {
        Mode::Install | Mode::Locked => Some(BTreeSet::new()),
        Mode::Update(names) if names.is_empty() => None,
        Mode::Update(names) => {
            if let Some(unknown) = names.iter().find(|n| previous.get(n).is_none()) {
                return Err(Error::new(format!(
                    "{unknown} is not a module in {LOCK_FILE}, so there is nothing to update"
                )));
            }
            Some(names.iter().cloned().collect())
        }
    };
    let mut resolver = Resolver {
        project,
        root,
        previous: previous.clone(),
        refresh,
        chosen: BTreeMap::new(),
        listings: HashMap::new(),
    };
    let lock = resolver.resolve()?;

    let mut changes = Vec::new();
    for module in &lock.modules {
        let change = match previous.get(&module.name) {
            None => Change::Added,
            Some(old) if old == module => Change::Unchanged,
            Some(old) if old.version != module.version => Change::Updated {
                from: old.version.clone(),
            },
            Some(old) if old.resolved != module.resolved => Change::Updated {
                from: old.resolved.clone(),
            },
            Some(old) => Change::Updated {
                from: old.source.clone(),
            },
        };
        changes.push((module.clone(), change));
    }
    for old in &previous.modules {
        if lock.get(&old.name).is_none() {
            changes.push((old.clone(), Change::Removed));
        }
    }
    changes.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    // Before the lockfile is written: a module that may not be used is not
    // pinned either.
    let mut signatures = BTreeMap::new();
    for module in lock
        .modules
        .iter()
        .filter(|m| !m.source.starts_with("path+"))
    {
        let dir = store_dir(project.state_dir, module);
        let found = signed(project, module, &dir).map_err(Error::new)?;
        if found != Signature::NotChecked {
            signatures.insert(module.name.clone(), found);
        }
    }

    let lock_path = project.dir.join(LOCK_FILE);
    let on_disk = fs::read_to_string(&lock_path).ok();
    let rendered = lock.render();
    let differs = match &on_disk {
        Some(text) => *text != rendered,
        // A project with no modules needs no lockfile.
        None => !lock.modules.is_empty(),
    };
    if differs && mode == Mode::Locked {
        let moved: Vec<String> = changes
            .iter()
            .filter(|(_, c)| *c != Change::Unchanged)
            .map(|(m, _)| m.name.clone())
            .collect();
        return Err(Error::new(format!(
            "{LOCK_FILE} is out of date with {MANIFEST_FILE}{}; run `meridian module install` \
             and commit the result",
            if moved.is_empty() {
                String::new()
            } else {
                format!(" ({} would change)", moved.join(", "))
            }
        )));
    }
    if differs {
        write_atomically(&lock_path, &rendered)?;
    }
    Ok(Report {
        lock,
        changes,
        wrote_lock: differs,
        signatures,
    })
}

/// The directory a pinned module's files are in, and what is wrong with it
/// if anything is.
fn locate(project: &Project<'_>, module: &Locked) -> Result<(PathBuf, Signature), String> {
    if let Some(path) = module.source.strip_prefix("path+") {
        let dir = project.dir.join(path);
        return if dir.join(MANIFEST_FILE).is_file() {
            Ok((dir, Signature::NotChecked))
        } else {
            Err(format!(
                "{} is required from {}, which has no {MANIFEST_FILE}",
                module.name,
                dir.display()
            ))
        };
    }
    let dir = store_dir(project.state_dir, module);
    if !dir.is_dir() {
        return Err(format!(
            "{} is not installed; run `meridian module install`",
            label(module)
        ));
    }
    match hash_dir(&dir) {
        Ok(found) if found == module.integrity => {
            let signature = signed(project, module, &dir)?;
            Ok((dir, signature))
        }
        Ok(found) => Err(format!(
            "{} in {} does not match {LOCK_FILE}\n    pinned: {}\n    found:  {found}\nA file \
             in it was changed after it was fetched. Delete that directory and run `meridian \
             module install`.",
            label(module),
            dir.display(),
            module.integrity
        )),
        Err(e) => Err(format!("{} in {}: {e}", label(module), dir.display())),
    }
}

/// Applies the project's signature policy to a fetched module whose content
/// already matched its pin.
fn signed(project: &Project<'_>, module: &Locked, dir: &Path) -> Result<Signature, String> {
    signature::check(
        project.signatures,
        dir,
        &label(module),
        (&module.name, &module.version, &module.integrity),
    )
}

/// Checks every pinned module, and reports on each instead of stopping at
/// the first that is wrong.
pub fn verify(project: &Project<'_>) -> Result<Vec<Check>, Error> {
    Manifest::load(project.dir)?;
    let lock = Lock::load(project.dir)?.unwrap_or_default();
    Ok(lock
        .modules
        .into_iter()
        .map(|module| match locate(project, &module) {
            Ok((_, signature)) => Check {
                module,
                problem: None,
                signature,
            },
            Err(problem) => Check {
                module,
                problem: Some(problem),
                signature: Signature::NotChecked,
            },
        })
        .collect())
}

/// The pinned modules of a project, open for use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modules {
    roots: BTreeMap<String, PathBuf>,
    /// A line for each module in use although its signature did not pass.
    warnings: Vec<String>,
}

impl Modules {
    /// Opens the project's pinned modules. Nothing is fetched: a module that
    /// is missing, changed, or out of step with the manifest is an error that
    /// says to run `meridian module install`.
    pub fn load(project: &Project<'_>) -> Result<Modules, Error> {
        let root = Manifest::load(project.dir)?;
        check_core("the project", &root, project.core_version)?;
        let lock = Lock::load(project.dir)?;
        let stale = |why: String| {
            Error::new(format!(
                "{LOCK_FILE} is out of date with {MANIFEST_FILE}: {why}; run `meridian module \
                 install`"
            ))
        };
        let Some(lock) = lock else {
            if root.modules.is_empty() {
                return Ok(Modules {
                    roots: BTreeMap::new(),
                    warnings: Vec::new(),
                });
            }
            return Err(Error::new(format!(
                "the project requires modules and has no {LOCK_FILE}; run `meridian module \
                 install`"
            )));
        };
        for requirement in &root.modules {
            let Some(pin) = lock.get(&requirement.name) else {
                return Err(stale(format!("{} is not pinned", requirement.name)));
            };
            let locator = match &requirement.source {
                Source::Path(path) => format!("path+{}", normalize(Path::new(path))),
                other => other.locator(),
            };
            if pin.source != locator {
                return Err(stale(format!(
                    "{} is pinned from {} and required from {locator}",
                    requirement.name, pin.source
                )));
            }
            if let Some(req) = &requirement.version {
                // A path module is a working copy: its version is whatever
                // its manifest says now, and install is what checks it.
                let fits = pin.source.starts_with("path+")
                    || Version::parse(&pin.version).is_ok_and(|v| req.matches(&v));
                if !fits {
                    return Err(stale(format!(
                        "{} is pinned at {} and required as {req}",
                        requirement.name, pin.version
                    )));
                }
            }
        }
        let mut roots = BTreeMap::new();
        let mut signatures = BTreeMap::new();
        for module in &lock.modules {
            let (dir, signature) = locate(project, module).map_err(Error::new)?;
            roots.insert(module.name.clone(), dir);
            signatures.insert(module.name.clone(), signature);
        }
        Ok(Modules {
            roots,
            warnings: warnings(&lock.modules, &signatures),
        })
    }

    /// A line for each module in use although it is unsigned or its
    /// signature does not pass, which only the `warn` policy allows.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The names of the pinned modules, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.roots.keys().map(String::as_str)
    }

    /// The directory holding a module's files.
    pub fn root(&self, name: &str) -> Option<&Path> {
        self.roots.get(name).map(PathBuf::as_path)
    }

    /// Every module's directory, by name. A caller that confines file access
    /// to a set of trees adds these to it.
    pub fn roots(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.roots.iter().map(|(n, p)| (n.as_str(), p.as_path()))
    }

    /// Whether `text` is written as a reference to a file in a module.
    pub fn is_reference(text: &str) -> bool {
        text.starts_with(MODULE_REFERENCE_PREFIX)
    }

    /// Resolves `@<module>:<path>` to the file it names.
    pub fn resolve(&self, reference: &str) -> Result<PathBuf, Error> {
        let bad = |why: String| Error::new(format!("{reference}: {why}"));
        let Some((name, path)) = reference
            .strip_prefix(MODULE_REFERENCE_PREFIX)
            .and_then(|rest| rest.split_once(':'))
        else {
            return Err(bad(
                "a file in a module is written @<module>:<path>, as in \
                 @acme/nginx:documents/proxy.yaml"
                    .to_owned(),
            ));
        };
        let Some(root) = self.root(name) else {
            let known: Vec<&str> = self.names().collect();
            return Err(bad(format!(
                "{name} is not a module of this project ({})",
                if known.is_empty() {
                    format!("{MANIFEST_FILE} requires none")
                } else {
                    format!("it has {}", known.join(", "))
                }
            )));
        };
        if !safe_relative(path) {
            return Err(bad(format!(
                "{path:?} is not a path inside the module {name}"
            )));
        }
        let file = root.join(path);
        if !file.is_file() {
            return Err(bad(format!("the module {name} has no file {path}")));
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalised_without_touching_the_disk() {
        for (path, want) in [
            ("modules/base", "modules/base"),
            ("./modules/../shared/base/", "shared/base"),
            ("../base", "../base"),
            ("a/../../base", "../base"),
            ("", "."),
            ("a/..", "."),
            ("/opt/modules/../base", "/opt/base"),
        ] {
            assert_eq!(normalize(Path::new(path)), want, "{path}");
        }
    }

    #[test]
    fn a_fetched_module_is_kept_by_name_and_version() {
        let state = Path::new("/state");
        let versioned = Locked {
            name: "acme/nginx".into(),
            version: "1.4.2".into(),
            ..Locked::default()
        };
        assert_eq!(
            store_dir(state, &versioned),
            Path::new("/state/modules/acme/nginx/1.4.2")
        );
        let by_ref = Locked {
            name: "pinned".into(),
            resolved: "4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a".into(),
            ..Locked::default()
        };
        assert_eq!(
            store_dir(state, &by_ref),
            Path::new("/state/modules/pinned/ref-4f2a9c1e5b3d")
        );
    }
}
