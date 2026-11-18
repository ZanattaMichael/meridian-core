use std::collections::BTreeMap;
use std::fmt;

use meridian_ir::Value;

use crate::resolve::{Error, Kind};

/// A per-key merge behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Strategy {
    /// Takes the value from the highest-priority layer that declares the key
    /// and ignores every lower layer. The default.
    #[default]
    FirstFound,
    /// Merges nested maps recursively, so a higher layer can override one
    /// nested key without clobbering its siblings.
    DeepMerge,
    /// Concatenates arrays from every layer and removes duplicates, keeping
    /// the first occurrence in priority order.
    UniqueArrayMerge,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::FirstFound => "first-found",
            Strategy::DeepMerge => "deep-merge",
            Strategy::UniqueArrayMerge => "unique-array-merge",
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "first-found" => Ok(Strategy::FirstFound),
            "deep-merge" => Ok(Strategy::DeepMerge),
            "unique-array-merge" => Ok(Strategy::UniqueArrayMerge),
            other => Err(format!(
                "unknown merge strategy {other:?} (expected \"first-found\", \"deep-merge\" or \"unique-array-merge\")"
            )),
        }
    }
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How one top-level key is merged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyRule {
    /// `None` means the hierarchy's default strategy.
    pub strategy: Option<Strategy>,
    /// The field that identifies an object element under unique-array-merge.
    /// Without it, object elements are deduplicated by their whole canonical
    /// value, which is rarely what an author wants. Empty when unset.
    pub dedup_key: String,
}

impl KeyRule {
    pub fn new(strategy: Strategy) -> Self {
        KeyRule {
            strategy: Some(strategy),
            dedup_key: String::new(),
        }
    }

    pub fn with_dedup_key(mut self, key: &str) -> Self {
        self.dedup_key = key.to_owned();
        self
    }
}

/// What a missing hierarchy layer means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Strictness {
    /// A missing layer file is an empty layer. The usual mode: most nodes
    /// have no node-specific file.
    #[default]
    Lenient,
    /// A missing layer file is an error, for deployments that require every
    /// declared layer to exist.
    Strict,
}

/// The variables a hierarchy path may interpolate with `%{name}`.
pub type Scope = BTreeMap<String, String>;

/// The parsed `hierarchy.yaml`: an ordered list of layer paths plus the merge
/// rules applied across them.
///
/// Paths are listed lowest-priority first, matching the design plan's example
/// (common, environment, role, node), so the last matching layer wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hierarchy {
    pub paths: Vec<String>,
    pub default: KeyRule,
    pub keys: BTreeMap<String, KeyRule>,
    pub strictness: Strictness,
}

/// A scope value may itself reference another variable. A chain that comes
/// back to a variable already being expanded is a configuration error, not a
/// stack overflow, and so is one that simply never ends.
const MAX_INTERPOLATION_DEPTH: usize = 32;

impl Hierarchy {
    /// A hierarchy over `paths` with every rule at its default.
    pub fn new<S: Into<String>>(paths: impl IntoIterator<Item = S>) -> Self {
        Hierarchy {
            paths: paths.into_iter().map(Into::into).collect(),
            ..Hierarchy::default()
        }
    }

    /// Parses a `hierarchy.yaml` (or JSON) document. `name` is used only in
    /// diagnostics.
    ///
    /// An unknown strategy or strictness, a field of the wrong shape and an
    /// unrecognised field are all errors here, so a `Hierarchy` that exists is
    /// one whose rules mean something.
    pub fn parse(name: &str, data: &[u8]) -> Result<Self, Error> {
        let value = meridian_ir::parse_value(name, data)
            .map_err(|e| Error::new(Kind::Hierarchy, e.to_string()))?;
        let h = Self::from_value(&value)
            .map_err(|e| Error::new(Kind::Hierarchy, format!("{name}: {e}")))?;
        h.validate()?;
        Ok(h)
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let top = fields(
            value,
            "hierarchy file",
            &["hierarchy", "default", "keys", "strictness"],
        )?;
        let mut h = Hierarchy::default();
        if let Some(paths) = top.get("hierarchy").filter(|v| **v != Value::Null) {
            let items = paths
                .as_list()
                .ok_or_else(|| format!("hierarchy: expected a list, found {}", paths.describe()))?;
            for (i, item) in items.iter().enumerate() {
                h.paths.push(string(item, &format!("hierarchy[{i}]"))?);
            }
        }
        if let Some(rule) = top.get("default") {
            h.default = key_rule(rule, "default")?;
        }
        if let Some(keys) = top.get("keys").filter(|v| **v != Value::Null) {
            let map = keys
                .as_map()
                .ok_or_else(|| format!("keys: expected a mapping, found {}", keys.describe()))?;
            for (key, rule) in map {
                h.keys
                    .insert(key.clone(), key_rule(rule, &format!("keys.{key}"))?);
            }
        }
        match top.get("strictness").filter(|v| **v != Value::Null) {
            None => {}
            Some(v) => {
                h.strictness = match string(v, "strictness")?.as_str() {
                    "lenient" => Strictness::Lenient,
                    "strict" => Strictness::Strict,
                    other => {
                        return Err(format!(
                            "hierarchy: unknown strictness {other:?} (expected \"lenient\" or \"strict\")"
                        ))
                    }
                };
            }
        }
        Ok(h)
    }

    /// The merge rule governing a top-level key.
    pub(crate) fn rule_for(&self, key: &str) -> (Strategy, &str) {
        let default = self.default.strategy.unwrap_or_default();
        match self.keys.get(key) {
            Some(rule) => (rule.strategy.unwrap_or(default), &rule.dedup_key),
            None => (default, &self.default.dedup_key),
        }
    }

    /// Reports configuration errors in the hierarchy itself, independently of
    /// any node being resolved.
    pub fn validate(&self) -> Result<(), Error> {
        if self.paths.is_empty() {
            return Err(Error::new(Kind::Hierarchy, "hierarchy: no layers declared"));
        }
        Ok(())
    }

    /// Interpolates every hierarchy path against `scope`, returning them in
    /// priority order (lowest first).
    pub fn expand_paths(&self, scope: &Scope) -> Result<Vec<String>, Error> {
        let mut out: Vec<String> = Vec::with_capacity(self.paths.len());
        for (i, raw) in self.paths.iter().enumerate() {
            let expanded = interpolate(raw, scope, &mut Vec::new())
                .map_err(|e| Error::new(Kind::Hierarchy, format!("hierarchy[{i}] {raw:?}: {e}")))?;
            // Clean the path so two spellings of the same layer (for example
            // "role/x.yaml" and "node/../role/x.yaml") compare equal and a
            // layer cannot be loaded twice under different names.
            let expanded = clean(&expanded);
            if let Some(prev) = out.iter().position(|p| *p == expanded) {
                return Err(Error::new(
                    Kind::Hierarchy,
                    format!(
                    "hierarchy[{i}] {raw:?} resolves to {expanded:?}, which hierarchy[{prev}] already resolves to: \
                     a layer cannot appear twice in one hierarchy"
                ),
                ));
            }
            out.push(expanded);
        }
        Ok(out)
    }
}

fn fields<'v>(
    value: &'v Value,
    what: &str,
    known: &[&str],
) -> Result<&'v BTreeMap<String, Value>, String> {
    static EMPTY: BTreeMap<String, Value> = BTreeMap::new();
    let map = match value {
        Value::Null => &EMPTY,
        Value::Map(map) => map,
        other => {
            return Err(format!(
                "{what}: expected a mapping, found {}",
                other.describe()
            ))
        }
    };
    match map.keys().find(|k| !known.contains(&k.as_str())) {
        Some(unknown) => Err(format!("{what}: unknown field {unknown:?}")),
        None => Ok(map),
    }
}

fn string(value: &Value, path: &str) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        other => Err(format!(
            "{path}: expected a string, found {}",
            other.describe()
        )),
    }
}

fn key_rule(value: &Value, path: &str) -> Result<KeyRule, String> {
    let map = fields(value, path, &["strategy", "dedupKey"])?;
    let mut rule = KeyRule::default();
    if let Some(v) = map.get("strategy").filter(|v| **v != Value::Null) {
        let text = string(v, &format!("{path}.strategy"))?;
        rule.strategy =
            Some(Strategy::parse(&text).map_err(|e| format!("hierarchy: {path}: {e}"))?);
    }
    if let Some(v) = map.get("dedupKey").filter(|v| **v != Value::Null) {
        rule.dedup_key = string(v, &format!("{path}.dedupKey"))?;
    }
    Ok(rule)
}

/// Replaces every `%{name}` in `s` with its scope value, recursively, so a
/// variable whose value itself contains `%{...}` is expanded too. `visiting`
/// carries the chain of variables currently being expanded, which is what
/// makes a circular reference a reported error instead of infinite recursion.
fn interpolate<'a>(
    s: &'a str,
    scope: &'a Scope,
    visiting: &mut Vec<&'a str>,
) -> Result<String, String> {
    if visiting.len() > MAX_INTERPOLATION_DEPTH {
        return Err(format!(
            "interpolation nested more than {MAX_INTERPOLATION_DEPTH} levels deep (chain: {})",
            visiting.join(" -> ")
        ));
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("%{") {
        out.push_str(&rest[..start]);
        let offset = s.len() - rest.len() + start;
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("unterminated %{{ at offset {offset}"));
        };
        let name = after[..end].trim();
        rest = &after[end + 1..];

        if name.is_empty() {
            return Err("empty interpolation %{}".to_owned());
        }
        if visiting.contains(&name) {
            visiting.push(name);
            return Err(format!(
                "circular hierarchy reference: {}",
                visiting.join(" -> ")
            ));
        }
        let Some(value) = scope.get(name) else {
            return Err(format!("undefined variable %{{{name}}}"));
        };
        visiting.push(name);
        let nested = interpolate(value, scope, visiting)?;
        visiting.pop();
        out.push_str(&nested);
    }
    out.push_str(rest);
    Ok(out)
}

/// Returns the shortest path equivalent to `path` by purely lexical
/// processing: repeated slashes collapse, `.` elements vanish, and an inner
/// `..` removes the element before it. A `..` that would climb above a rooted
/// path is dropped; above a relative one it is kept. The empty path is `.`.
fn clean(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ if rooted => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

#[cfg(test)]
mod tests {
    use super::clean;

    /// The cases from Go's `path.Clean` tests, which this has to match so two
    /// spellings of one layer are recognised as the same layer.
    #[test]
    fn clean_matches_the_reference() {
        for (input, want) in [
            ("", "."),
            ("abc", "abc"),
            ("abc/def", "abc/def"),
            ("a/b/c", "a/b/c"),
            (".", "."),
            ("..", ".."),
            ("../..", "../.."),
            ("../../abc", "../../abc"),
            ("/abc", "/abc"),
            ("/", "/"),
            ("abc/", "abc"),
            ("abc/def/", "abc/def"),
            ("a/b/c/", "a/b/c"),
            ("./", "."),
            ("../", ".."),
            ("../../", "../.."),
            ("/abc/", "/abc"),
            ("abc//def//ghi", "abc/def/ghi"),
            ("//abc", "/abc"),
            ("///abc", "/abc"),
            ("//abc//", "/abc"),
            ("abc//", "abc"),
            ("abc/./def", "abc/def"),
            ("/./abc/def", "/abc/def"),
            ("abc/.", "abc"),
            ("abc/def/ghi/../jkl", "abc/def/jkl"),
            ("abc/def/../ghi/../jkl", "abc/jkl"),
            ("abc/def/..", "abc"),
            ("abc/def/../..", "."),
            ("/abc/def/../..", "/"),
            ("abc/def/../../..", ".."),
            ("/abc/def/../../..", "/"),
            ("abc/def/../../../ghi/jkl/../../../mno", "../../mno"),
            ("abc/./../def", "def"),
            ("abc//./../def", "def"),
            ("abc/../../././../def", "../../def"),
        ] {
            assert_eq!(clean(input), want, "{input:?}");
        }
    }
}
