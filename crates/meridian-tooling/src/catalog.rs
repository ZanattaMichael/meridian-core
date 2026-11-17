//! What the loaded target plugins say about themselves, gathered once so
//! every tool reads the same answer.
//!
//! Nothing about a target is written down here. A catalog is filled from the
//! plugins' own `Describe` replies, so a plugin that is added, updated or
//! replaced changes what the tools offer without any change to them.

use std::collections::BTreeMap;

use meridian_orchestrator::{kind_fits, StageKind};
use meridian_plugin_host::{Info, Registry};
use meridian_sdk::{Capabilities, Category, Operations, ResourceSchema};

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
    /// describing appears here as an open type with no params.
    pub types: Vec<ResourceSchema>,
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
            types,
        }
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

    /// Every target, sorted by name.
    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        self.targets.values()
    }

    pub fn names(&self) -> Vec<&str> {
        self.targets.keys().map(String::as_str).collect()
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
}
