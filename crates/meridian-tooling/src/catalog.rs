//! What the loaded target plugins say about themselves, gathered once so
//! every tool reads the same answer.
//!
//! Nothing about a target is written down here. A catalog is filled from the
//! plugins' own `Describe` replies, so a plugin that is added, updated or
//! replaced changes what the tools offer without any change to them. The
//! schemas a tool itself described, cached by `meridian schema sync`, are
//! read in on top ([`Catalog::read_cache`]): each becomes an exact type of
//! its target, found before the pattern it fits.

use std::collections::BTreeMap;

use meridian_module::Lock;
use meridian_orchestrator::{kind_fits, StageKind};
use meridian_plugin_host::{Info, Registry};
use meridian_sdk::{Capabilities, Category, Operations, ResourceSchema};

use crate::schemas::{Cached, Freshness, Store};

/// Where a target's cached tool schemas came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheNote {
    /// The tool and the versions, as one line.
    pub versions: String,
    /// When the sync ran.
    pub synced_at: String,
    /// How the cache stands against `meridian.lock`.
    pub freshness: Freshness,
    /// How many types it describes.
    pub types: usize,
}

/// One target, as its plugin describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub category: Category,
    pub capabilities: Capabilities,
    pub operations: Operations,
    /// The versions of its tool the target compiles for; empty when it does
    /// not say.
    pub versions: Vec<String>,
    /// Every resource type, sorted by name. A type the plugin lists without
    /// describing appears here as an open type with no params. A type a
    /// tool described, read from the cache, is here under its exact name.
    pub types: Vec<ResourceSchema>,
    /// The types as the plugin alone describes them, which a cache that is
    /// read later refines without losing.
    declared: Vec<ResourceSchema>,
    /// The cache of tool schemas that was read in, if any.
    pub cache: Option<CacheNote>,
}

impl Target {
    fn from_info(info: &Info) -> Target {
        let mut types = info.resource_schemas.clone();
        for name in &info.resource_types {
            if !types.iter().any(|t| &t.r#type == name) {
                types.push(ResourceSchema::open(name, ""));
            }
        }
        types.sort_by(|a, b| a.r#type.cmp(&b.r#type));
        Target {
            name: info.name.clone(),
            category: info.category,
            capabilities: info.capabilities,
            operations: info.operations,
            versions: info.target_versions.clone(),
            declared: types.clone(),
            types,
            cache: None,
        }
    }

    /// Adds the types a tool described. A type the plugin described param
    /// by param under the same exact name is kept, since the plugin is the
    /// one that compiles it. A type the plugin only named, leaving its
    /// params open, is what the tool's own schema refines: that is the case
    /// the cache exists for. Everything else from the cache is added, and
    /// an earlier cache's entries are replaced.
    pub fn add_cached(&mut self, cached: &Cached, freshness: Freshness) {
        self.types = self.declared.clone();
        for schema in &cached.schemas {
            let described = self
                .declared
                .iter()
                .any(|t| t.r#type == schema.r#type && !t.open);
            if schema.is_pattern() || described {
                continue;
            }
            let mut schema = schema.clone();
            if schema.source.is_empty() {
                schema.source = cached.versions();
            }
            if let Some(at) = self.types.iter().position(|t| t.r#type == schema.r#type) {
                let named = &self.types[at];
                if schema.description.is_empty() {
                    schema.description = named.description.clone();
                }
                if schema.states.is_empty() {
                    schema.states = named.states.clone();
                    schema.default_state = named.default_state.clone();
                }
                if schema.actions.is_empty() {
                    schema.actions = named.actions.clone();
                }
                self.types[at] = schema;
            } else {
                self.types.push(schema);
            }
        }
        self.types.sort_by(|a, b| a.r#type.cmp(&b.r#type));
        self.cache = Some(CacheNote {
            versions: cached.versions(),
            synced_at: cached.synced_at.clone(),
            freshness,
            types: cached.schemas.len(),
        });
    }

    /// The types a tool described, as opposed to the ones the plugin
    /// declares.
    pub fn cached_types(&self) -> impl Iterator<Item = &ResourceSchema> {
        self.types.iter().filter(|t| !t.source.is_empty())
    }

    /// What the target says about a resource type: the type listed under
    /// exactly that name, or else the pattern the name fits best.
    pub fn resource(&self, name: &str) -> Option<&ResourceSchema> {
        if let Some(exact) = self.types.iter().find(|t| t.r#type == name) {
            return Some(exact);
        }
        self.types
            .iter()
            .filter(|t| t.is_pattern() && pattern_fits(&t.r#type, name))
            .max_by_key(|t| literal_len(&t.r#type))
    }

    /// Whether the target lists any resource types at all.
    pub fn knows_types(&self) -> bool {
        !self.types.is_empty()
    }

    /// Why a document of `kind` cannot use this target, if it cannot.
    pub fn refuses_kind(&self, kind: meridian_ir::Kind) -> Option<String> {
        let kind = match kind {
            meridian_ir::Kind::Infrastructure => StageKind::Infrastructure,
            meridian_ir::Kind::ResourceSet => StageKind::ResourceSet,
        };
        kind_fits(kind, self.category).err()
    }

    /// What the target can do, as words.
    pub fn can(&self) -> Vec<&'static str> {
        let mut can = vec!["compile"];
        if self.operations.plan {
            can.push("plan");
        }
        if self.operations.apply {
            can.push("apply");
        }
        if self.operations.outputs {
            can.push("outputs");
        }
        can
    }
}

/// The pieces of a pattern such as `<provider>_<type>`: literal text and the
/// names of its placeholders, in order.
pub fn pattern_parts(pattern: &str) -> Vec<PatternPart<'_>> {
    let mut parts = Vec::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        if open > 0 {
            parts.push(PatternPart::Literal(&rest[..open]));
        }
        parts.push(PatternPart::Placeholder(&rest[open + 1..open + close]));
        rest = &rest[open + close + 1..];
    }
    if !rest.is_empty() {
        parts.push(PatternPart::Literal(rest));
    }
    parts
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternPart<'a> {
    Literal(&'a str),
    Placeholder(&'a str),
}

fn literal_len(pattern: &str) -> usize {
    pattern_parts(pattern)
        .iter()
        .map(|p| match p {
            PatternPart::Literal(text) => text.len(),
            PatternPart::Placeholder(_) => 0,
        })
        .sum()
}

/// Whether `name` could be written from `pattern`: its literal text appears
/// in order and every placeholder stands for at least one character.
pub fn pattern_fits(pattern: &str, name: &str) -> bool {
    fn fits(parts: &[PatternPart<'_>], name: &str) -> bool {
        match parts.split_first() {
            None => name.is_empty(),
            Some((PatternPart::Literal(text), rest)) => {
                name.strip_prefix(text).is_some_and(|n| fits(rest, n))
            }
            Some((PatternPart::Placeholder(_), rest)) => {
                // Try every non-empty prefix the placeholder could stand for.
                name.char_indices()
                    .skip(1)
                    .map(|(i, _)| i)
                    .chain([name.len()])
                    .any(|i| i > 0 && fits(rest, &name[i..]))
            }
        }
    }
    fits(&pattern_parts(pattern), name)
}

/// Every loaded target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    targets: BTreeMap<String, Target>,
}

impl Catalog {
    /// A catalog of nothing: only the language itself is known.
    pub fn empty() -> Catalog {
        Catalog::default()
    }

    /// Reads what every plugin in a registry reported when it was loaded.
    pub fn from_registry(registry: &Registry) -> Catalog {
        Catalog::from_infos(
            registry
                .names()
                .into_iter()
                .filter_map(|name| registry.target(name))
                .map(|target| target.info()),
        )
    }

    pub fn from_infos<'a>(infos: impl IntoIterator<Item = &'a Info>) -> Catalog {
        Catalog {
            targets: infos
                .into_iter()
                .map(|info| (info.name.clone(), Target::from_info(info)))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn target(&self, name: &str) -> Option<&Target> {
        self.targets.get(name)
    }

    pub fn target_mut(&mut self, name: &str) -> Option<&mut Target> {
        self.targets.get_mut(name)
    }

    /// Every target, sorted by name.
    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        self.targets.values()
    }

    pub fn names(&self) -> Vec<&str> {
        self.targets.keys().map(String::as_str).collect()
    }

    /// Reads the cache under a state directory into the catalog, for every
    /// target that has both a plugin and a cache file, and judges each
    /// against `lock` when there is one. Returns what could not be read,
    /// one line each; a missing cache is not a problem, just less known.
    pub fn read_cache(&mut self, store: &Store, lock: Option<&Lock>) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, found) in store.load_all() {
            let cached = match found {
                Ok(cached) => cached,
                Err(e) => {
                    problems.push(e);
                    continue;
                }
            };
            let Some(target) = self.targets.get_mut(&name) else {
                continue;
            };
            let integrity = store.integrity_of(&name);
            let locked = lock.and_then(|l| l.schemas_for(&name));
            let freshness = Freshness::of(Some(&cached), integrity.as_deref(), locked);
            target.add_cached(&cached, freshness);
        }
        problems
    }

    /// The targets the lock records a cache for that have none on disk:
    /// what a fresh clone looks like before `meridian schema sync`.
    pub fn missing_caches<'l>(&self, lock: &'l Lock) -> Vec<&'l str> {
        lock.schemas
            .iter()
            .filter(|s| {
                self.targets
                    .get(&s.target)
                    .is_some_and(|t| t.cache.is_none())
            })
            .map(|s| s.target.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_fits_names_written_from_it() {
        assert!(pattern_fits("<provider>_<type>", "aws_instance"));
        assert!(pattern_fits("data.<type>", "data.aws_ami"));
        assert!(pattern_fits(
            "<Vendor>::<Service>::<Type>",
            "AWS::S3::Bucket"
        ));
        assert!(pattern_fits("<apiVersion>/<Kind>", "apps/v1/Deployment"));
        assert!(!pattern_fits("data.<type>", "data."));
        assert!(!pattern_fits("<provider>_<type>", "backend"));
        assert!(!pattern_fits("<Module>/<Resource>", "runner"));
    }

    #[test]
    fn an_exact_type_wins_over_a_pattern_and_the_closest_pattern_wins() {
        let info = Info {
            name: "t".to_owned(),
            resource_types: vec![
                "<provider>_<type>".to_owned(),
                "data.<type>".to_owned(),
                "module".to_owned(),
            ],
            ..Info::default()
        };
        let target = Target::from_info(&info);
        assert_eq!(target.resource("module").unwrap().r#type, "module");
        assert_eq!(
            target.resource("data.aws_ami").unwrap().r#type,
            "data.<type>"
        );
        assert_eq!(
            target.resource("aws_instance").unwrap().r#type,
            "<provider>_<type>"
        );
        assert!(target.resource("nothing").is_none());
        assert!(target.types.iter().all(|t| t.open));
    }

    #[test]
    fn cached_types_join_the_declared_ones_and_exact_wins_over_the_pattern() {
        use meridian_sdk::{ParamKind, ParamSchema, SchemaExport};
        let info = Info {
            name: "t".to_owned(),
            resource_types: vec!["<provider>_<type>".to_owned(), "backend".to_owned()],
            ..Info::default()
        };
        let mut target = Target::from_info(&info);
        let mut export = SchemaExport::new("tool", "1.0");
        export.schemas.push(
            ResourceSchema::closed("aws_instance", "An instance.")
                .param(ParamSchema::new("ami", ParamKind::String, "").required()),
        );
        // The plugin only named `backend`, so the tool's description of it
        // takes over; `module`, which the plugin described itself, is kept.
        export
            .schemas
            .push(ResourceSchema::closed("backend", "from the tool"));
        export
            .schemas
            .push(ResourceSchema::closed("module", "from the tool"));
        target.declared.push(
            ResourceSchema::closed("module", "from the plugin")
                .param(ParamSchema::new("source", ParamKind::String, "").required()),
        );
        let cached = Cached::from_export("t", export);
        target.add_cached(&cached, Freshness::Unrecorded);
        let found = target.resource("aws_instance").unwrap();
        assert!(!found.open && found.source == "tool 1.0");
        assert_eq!(found.params[0].name, "ami");
        let backend = target.resource("backend").unwrap();
        assert_eq!(backend.description, "from the tool");
        assert!(!backend.open && backend.source == "tool 1.0");
        let module = target.resource("module").unwrap();
        assert_eq!(module.description, "from the plugin");
        assert!(module.source.is_empty());
        assert_eq!(
            target.resource("aws_other").unwrap().r#type,
            "<provider>_<type>"
        );
        assert_eq!(target.cached_types().count(), 2);
        assert_eq!(target.cache.as_ref().unwrap().types, 3);
        // Reading another cache replaces the first's types.
        let mut again = SchemaExport::new("tool", "2.0");
        again.schemas.push(ResourceSchema::closed("aws_bucket", ""));
        target.add_cached(&Cached::from_export("t", again), Freshness::Current);
        assert!(target.resource("aws_instance").unwrap().is_pattern());
        assert_eq!(target.resource("aws_bucket").unwrap().source, "tool 2.0");
        // ... and gives `backend` back to the plugin's own open description.
        let backend = target.resource("backend").unwrap();
        assert!(backend.open && backend.description.is_empty());
    }
}
