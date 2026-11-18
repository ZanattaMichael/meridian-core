//! Evaluating rules over a stack of layers.

use std::collections::BTreeSet;
use std::fmt;

use meridian_ir::Value;
use meridian_resolve::Data;

use crate::model::{Check, DefaultAccess, Rule, Rules, COMMAND_LINE, UNCLAIMED};
use crate::pattern::{display_path, glob, KeyPattern};
use crate::{Error, Kind};

/// How two layers' values for one key combine, reduced to what matters to a
/// rule: what a more specific layer's value does to a less specific one's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeKind {
    /// The more specific value replaces the whole. Whatever the less specific
    /// layer had beneath the key and the more specific one does not repeat
    /// is gone.
    Replace,
    /// Mappings merge key by key, at every depth. Anything else is replaced.
    Deep,
    /// Lists are joined, mappings merge key by key. Anything else is
    /// replaced.
    Append,
}

/// How a stack of layers merges. Implemented for today's
/// [`meridian_resolve::Hierarchy`]; any other way of layering only has to
/// answer these three questions to be checked by the same rules.
pub trait MergePlan {
    /// How the value at `path` (outermost key first, never empty) merges.
    fn kind(&self, path: &[String]) -> MergeKind;
    /// The name of the strategy the top-level key `key` merges with, as its
    /// author would write it.
    fn strategy(&self, key: &str) -> String;
    /// Every strategy name there is, so a rule naming another is an error
    /// and not a rule that can never be satisfied.
    fn strategies(&self) -> Vec<String>;
}

impl MergePlan for meridian_resolve::Hierarchy {
    fn kind(&self, path: &[String]) -> MergeKind {
        use meridian_resolve::Strategy;
        // A hierarchy's strategy belongs to the top-level key and governs
        // everything beneath it.
        match strategy_of(self, &path[0]) {
            Strategy::FirstFound => MergeKind::Replace,
            Strategy::DeepMerge => MergeKind::Deep,
            Strategy::UniqueArrayMerge => MergeKind::Append,
        }
    }

    fn strategy(&self, key: &str) -> String {
        strategy_of(self, key).as_str().to_owned()
    }

    fn strategies(&self) -> Vec<String> {
        use meridian_resolve::Strategy;
        [
            Strategy::FirstFound,
            Strategy::DeepMerge,
            Strategy::UniqueArrayMerge,
        ]
        .iter()
        .map(|s| s.as_str().to_owned())
        .collect()
    }
}

fn strategy_of(h: &meridian_resolve::Hierarchy, key: &str) -> meridian_resolve::Strategy {
    h.keys
        .get(key)
        .and_then(|rule| rule.strategy)
        .or(h.default.strategy)
        .unwrap_or_default()
}

/// Where a layer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A layer of the hierarchy. Its group is decided by its path.
    Hierarchy,
    /// A file of values given on the command line (`--data`) and merged
    /// over the hierarchy: a key in it replaces the same key beneath it,
    /// whole. It belongs to the group [`COMMAND_LINE`], whatever its path.
    CommandLine,
    /// Single values given on the command line (`--set`), merged last. Each
    /// one sets its own key inside the mappings above it and keeps their
    /// other keys. Also in the group [`COMMAND_LINE`].
    Set,
}

/// One layer as the rules see it.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// What a refusal calls the layer: its path as the hierarchy expands it.
    pub path: String,
    pub data: Data,
    pub origin: Origin,
}

impl Layer {
    pub fn new(path: impl Into<String>, data: Data) -> Self {
        Layer {
            path: path.into(),
            data,
            origin: Origin::Hierarchy,
        }
    }

    pub fn command_line(path: impl Into<String>, data: Data) -> Self {
        Layer {
            path: path.into(),
            data,
            origin: Origin::CommandLine,
        }
    }

    /// The `--set` values, already assembled into one mapping.
    pub fn set(path: impl Into<String>, data: Data) -> Self {
        Layer {
            path: path.into(),
            data,
            origin: Origin::Set,
        }
    }
}

/// Everything the rules are checked against.
pub struct Stack<'a> {
    /// The layers that exist, least specific first.
    pub layers: &'a [Layer],
    pub plan: &'a dyn MergePlan,
    /// The merged result of `layers`.
    pub resolved: &'a Data,
}

/// One broken rule. It says which rule, which layer and which key; it never
/// carries a value, because the values a rule guards are the ones that must
/// not reach a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The rule's id.
    pub rule: String,
    /// The kind of rule, such as `no-override`.
    pub check: &'static str,
    /// The layer at fault, when the fault is one layer's.
    pub layer: Option<String>,
    /// That layer's group, when groups are declared.
    pub group: Option<String>,
    /// The key, written the way a key pattern would name it.
    pub key: String,
    /// The whole sentence.
    pub text: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.rule, self.text)
    }
}

/// The id of the built-in check that every layer has an owner.
pub const UNCLAIMED_RULE: &str = "meridian/unclaimed-layer";

/// The id of the built-in check of each group's `capabilities`.
pub const CAPABILITY_RULE: &str = "meridian/capabilities";

/// Checks every rule and returns every violation, in a fixed order: by rule
/// as the file lists them, then by layer from least to most specific, then
/// by key. An `Err` means the rules could not be evaluated at all.
pub fn evaluate(rules: &Rules, stack: &Stack<'_>) -> Result<Vec<Violation>, Error> {
    let ev = Evaluation {
        rules,
        stack,
        groups: stack
            .layers
            .iter()
            .map(|layer| match layer.origin {
                Origin::CommandLine | Origin::Set => COMMAND_LINE,
                Origin::Hierarchy => rules.group_of(&layer.path),
            })
            .collect(),
    };
    let mut out = Vec::new();

    // A layer nobody owns is not subject to any rule that names an owner, so
    // it may not take part at all.
    if !rules.groups.is_empty() {
        for (i, layer) in stack.layers.iter().enumerate() {
            if ev.groups[i] == UNCLAIMED {
                out.push(Violation {
                    rule: UNCLAIMED_RULE.to_owned(),
                    check: "unclaimed-layer",
                    layer: Some(layer.path.clone()),
                    group: None,
                    key: String::new(),
                    text: format!(
                        "layer {:?} is not claimed by any group under `layers`; a layer nobody owns may set nothing",
                        layer.path
                    ),
                });
            }
        }
    }

    ev.capabilities(&mut out);

    for rule in &rules.rules {
        match &rule.check {
            Check::NoOverride => ev.overrides(rule, false, &mut out),
            Check::Final => ev.overrides(rule, true, &mut out),
            Check::Restrict { allow } => ev.restrict(rule, allow, &mut out),
            Check::Required => ev.required(rule, &mut out),
            Check::ForbiddenValue {
                values,
                matches,
                ignore_case,
            } => ev.forbidden(rule, values, matches, *ignore_case, &mut out),
            Check::MergeStrategy { allow } => ev.merge_strategy(rule, allow, &mut out)?,
        }
    }
    Ok(out)
}

struct Evaluation<'a> {
    rules: &'a Rules,
    stack: &'a Stack<'a>,
    /// The group of each layer, by index.
    groups: Vec<&'a str>,
}

/// What a more specific layer did to one value of a less specific layer.
struct Loss {
    /// The key the more specific layer wrote.
    at: Vec<String>,
    /// The less specific layer's value that is affected.
    leaf: Vec<String>,
    how: How,
}

#[derive(Clone, Copy, PartialEq)]
enum How {
    /// A different value was written over it.
    Changed,
    /// The same value was written again.
    Restated,
    /// Its parent was replaced by a value that leaves it out.
    Dropped,
}

impl Evaluation<'_> {
    fn within(&self, groups: &Option<Vec<String>>, layer: usize) -> bool {
        match groups {
            None => true,
            Some(names) => names.iter().any(|n| n == self.groups[layer]),
        }
    }

    /// `layer "path" (group)`, or without the group when none are declared.
    fn name(&self, layer: usize) -> String {
        let path = &self.stack.layers[layer].path;
        if self.rules.groups.is_empty() && self.groups[layer] == UNCLAIMED {
            format!("layer {path:?}")
        } else {
            format!("layer {path:?} ({})", self.groups[layer])
        }
    }

    fn violation(
        &self,
        rule: &Rule,
        layer: Option<usize>,
        key: &[String],
        text: String,
    ) -> Violation {
        let text = if rule.message.is_empty() {
            text
        } else {
            format!("{text}. {}", rule.message.trim_end_matches('.'))
        };
        Violation {
            rule: rule.id.clone(),
            check: rule.check.name(),
            layer: layer.map(|i| self.stack.layers[i].path.clone()),
            group: layer
                .map(|i| self.groups[i])
                .filter(|g| !(self.rules.groups.is_empty() && *g == UNCLAIMED))
                .map(str::to_owned),
            key: display_path(key),
            text,
        }
    }

    /// What each group may set: its `capabilities.keys`, or the file's
    /// default when it does not list them. Checked layer by layer, so a value
    /// several layers contribute to must be allowed to every one of them.
    fn capabilities(&self, out: &mut Vec<Violation>) {
        for (i, layer) in self.stack.layers.iter().enumerate() {
            // The command line is the operator's, not a layer of the
            // hierarchy; an unclaimed layer has been refused already.
            if layer.origin != Origin::Hierarchy {
                continue;
            }
            let Some(group) = self.rules.group(&layer.path) else {
                continue;
            };
            let why = match (&group.keys, self.rules.default) {
                (None, DefaultAccess::Allow) => continue,
                (None, DefaultAccess::Deny) => {
                    "it lists no `capabilities.keys`, and the default is deny".to_owned()
                }
                (Some(keys), _) if keys.is_empty() => "it may set no keys".to_owned(),
                (Some(keys), _) => format!(
                    "it may set only {}",
                    keys.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            let allowed = group.keys.as_deref().unwrap_or(&[]);
            let mut leaves = Vec::new();
            for (key, value) in &layer.data {
                collect_leaves(&mut vec![key.clone()], value, &mut leaves);
            }
            for (path, _) in leaves {
                if selects(allowed, &path) {
                    continue;
                }
                out.push(Violation {
                    rule: CAPABILITY_RULE.to_owned(),
                    check: "capabilities",
                    layer: Some(layer.path.clone()),
                    group: Some(group.name.clone()),
                    key: display_path(&path),
                    text: format!(
                        "{} sets {:?}, which {} is not allowed to set: {why}",
                        self.name(i),
                        display_path(&path),
                        group.name
                    ),
                });
            }
        }
    }

    /// `no-override` and `final`.
    fn overrides(&self, rule: &Rule, is_final: bool, out: &mut Vec<Violation>) {
        let layers = self.stack.layers;
        for later in 0..layers.len() {
            if !self.within(&rule.applies_to, later) {
                continue;
            }
            // One violation per key, blamed on the least specific layer that
            // set it: that is the value the author meant to protect.
            let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
            let mut found: Vec<(Vec<String>, Violation)> = Vec::new();
            for earlier in 0..later {
                if !self.within(&rule.set_by, earlier) {
                    continue;
                }
                let mut losses = Vec::new();
                for (key, theirs) in &layers[earlier].data {
                    if let Some(ours) = layers[later].data.get(key) {
                        let path = vec![key.clone()];
                        // A command-line value replaces the key it names,
                        // whatever strategy the hierarchy gives that key.
                        let kind = match layers[later].origin {
                            Origin::CommandLine => MergeKind::Replace,
                            Origin::Set => MergeKind::Deep,
                            Origin::Hierarchy => self.stack.plan.kind(&path),
                        };
                        diff(self.stack.plan, &path, theirs, ours, kind, &mut losses);
                    }
                }
                for loss in losses {
                    if loss.how == How::Restated && !is_final {
                        continue;
                    }
                    if !selects(&rule.keys, &loss.leaf) || !reported.insert(loss.leaf.clone()) {
                        continue;
                    }
                    let (us, them) = (self.name(later), self.name(earlier));
                    let leaf = display_path(&loss.leaf);
                    let at = display_path(&loss.at);
                    let text = match (loss.how, loss.at == loss.leaf, is_final) {
                        (How::Dropped, _, _) => {
                            let why = match layers[later].origin {
                                Origin::CommandLine => {
                                    "which a command-line value replaces whole".to_owned()
                                }
                                Origin::Set | Origin::Hierarchy => format!(
                                    "which is merged by replacing it whole ({})",
                                    self.stack.plan.strategy(&loss.at[0])
                                ),
                            };
                            format!("{us} sets {at:?}, {why}, and so drops {leaf:?}, which {them} set")
                        }
                        (_, true, true) => format!(
                            "{us} sets {leaf:?}, which is final once set, and {them} set it"
                        ),
                        (_, false, true) => format!(
                            "{us} sets {at:?}, replacing {leaf:?}, which is final once set, and {them} set it"
                        ),
                        (_, true, false) => {
                            format!("{us} changes {leaf:?}, which {them} set")
                        }
                        (_, false, false) => {
                            format!("{us} sets {at:?}, replacing {leaf:?}, which {them} set")
                        }
                    };
                    found.push((
                        loss.leaf.clone(),
                        self.violation(rule, Some(later), &loss.leaf, text),
                    ));
                }
            }
            found.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(found.into_iter().map(|(_, v)| v));
        }
    }

    fn restrict(&self, rule: &Rule, allow: &[String], out: &mut Vec<Violation>) {
        let who = if allow.is_empty() {
            "no layer may set".to_owned()
        } else {
            format!("only {} may set", allow.join(", "))
        };
        for (i, layer) in self.stack.layers.iter().enumerate() {
            if allow.iter().any(|g| g == self.groups[i]) {
                continue;
            }
            let mut leaves = Vec::new();
            for (key, value) in &layer.data {
                collect_leaves(&mut vec![key.clone()], value, &mut leaves);
            }
            for (path, _) in leaves {
                if selects(&rule.keys, &path) {
                    let text = format!(
                        "{} sets {:?}, which {who}",
                        self.name(i),
                        display_path(&path)
                    );
                    out.push(self.violation(rule, Some(i), &path, text));
                    continue;
                }
                // A value written above a restricted key replaces it without
                // ever naming it. That counts when there is something there
                // to replace: a restricted key set by a layer that may.
                if !rule.keys.iter().any(|k| k.reaches_below(&path)) {
                    continue;
                }
                let shadowed = self.stack.layers[..i]
                    .iter()
                    .enumerate()
                    .find_map(|(j, other)| {
                        if !allow.iter().any(|g| g == self.groups[j]) {
                            return None;
                        }
                        let mut beneath = Vec::new();
                        if let Some(value) = lookup(&other.data, &path) {
                            collect_leaves(&mut path.clone(), value, &mut beneath);
                        }
                        beneath
                            .into_iter()
                            .find(|(p, _)| p.len() > path.len() && selects(&rule.keys, p))
                            .map(|(p, _)| (j, p))
                    });
                if let Some((j, hidden)) = shadowed {
                    let text = format!(
                        "{} sets {:?}, replacing {:?}, which {} set and {who}",
                        self.name(i),
                        display_path(&path),
                        display_path(&hidden),
                        self.name(j)
                    );
                    out.push(self.violation(rule, Some(i), &path, text));
                }
            }
        }
    }

    fn required(&self, rule: &Rule, out: &mut Vec<Violation>) {
        let paths: Vec<Vec<String>> = rule
            .keys
            .iter()
            .map(|k| {
                k.exact_path()
                    .expect("a required rule's keys are exact")
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .collect();
        let set =
            |data: &Data, path: &[String]| !matches!(lookup(data, path), None | Some(Value::Null));
        match &rule.applies_to {
            None => {
                for path in &paths {
                    if !set(self.stack.resolved, path) {
                        let text =
                            format!("no layer sets {:?}, which must be set", display_path(path));
                        out.push(self.violation(rule, None, path, text));
                    }
                }
            }
            Some(groups) => {
                for (i, layer) in self.stack.layers.iter().enumerate() {
                    if !self.within(&rule.applies_to, i) {
                        continue;
                    }
                    for path in &paths {
                        if !set(&layer.data, path) {
                            let text = format!(
                                "{} does not set {:?}, which every layer of {} must set",
                                self.name(i),
                                display_path(path),
                                groups.join(", ")
                            );
                            out.push(self.violation(rule, Some(i), path, text));
                        }
                    }
                }
            }
        }
    }

    fn forbidden(
        &self,
        rule: &Rule,
        values: &[Value],
        matches: &[String],
        ignore_case: bool,
        out: &mut Vec<Violation>,
    ) {
        for (i, layer) in self.stack.layers.iter().enumerate() {
            if !self.within(&rule.applies_to, i) {
                continue;
            }
            let mut scalars = Vec::new();
            for (key, value) in &layer.data {
                collect_scalars(
                    &mut vec![key.clone()],
                    display_path(&[key]),
                    value,
                    &mut scalars,
                );
            }
            for (path, shown, value) in scalars {
                if !selects(&rule.keys, &path) {
                    continue;
                }
                // The entry is named by its place in the rule and not by its
                // text, so the refusal does not repeat the layer's value.
                let hit = values
                    .iter()
                    .position(|v| same_scalar(v, value, ignore_case))
                    .map(|n| format!("values[{n}]"))
                    .or_else(|| {
                        let text = scalar_text(value)?;
                        matches
                            .iter()
                            .position(|m| glob(m, &text, ignore_case))
                            .map(|n| format!("matches[{n}]"))
                    });
                if let Some(entry) = hit {
                    let text = format!(
                        "{} sets {shown:?} to a value the rule forbids ({entry})",
                        self.name(i)
                    );
                    let mut violation = self.violation(rule, Some(i), &path, text);
                    violation.key = shown;
                    out.push(violation);
                }
            }
        }
    }

    fn merge_strategy(
        &self,
        rule: &Rule,
        allow: &[String],
        out: &mut Vec<Violation>,
    ) -> Result<(), Error> {
        let known = self.stack.plan.strategies();
        if let Some(unknown) = allow.iter().find(|a| !known.contains(a)) {
            return Err(Error::new(
                Kind::File,
                format!(
                    "rule {:?}: allow: unknown merge strategy {unknown:?} (expected one of: {})",
                    rule.id,
                    known.join(", ")
                ),
            ));
        }
        for key in &rule.keys {
            let path: Vec<String> = key
                .exact_path()
                .expect("a merge-strategy rule's keys are exact")
                .into_iter()
                .map(str::to_owned)
                .collect();
            let strategy = self.stack.plan.strategy(&path[0]);
            if !allow.contains(&strategy) {
                let text = format!(
                    "{:?} is merged with {strategy}, and may only be merged with {}",
                    display_path(&path),
                    allow.join(" or ")
                );
                out.push(self.violation(rule, None, &path, text));
            }
        }
        Ok(())
    }
}

fn selects(keys: &[KeyPattern], path: &[String]) -> bool {
    keys.iter().any(|k| k.selects(path))
}

fn lookup<'d>(data: &'d Data, path: &[String]) -> Option<&'d Value> {
    let (first, rest) = path.split_first()?;
    let mut value = data.get(first)?;
    for key in rest {
        value = value.as_map()?.get(key)?;
    }
    Some(value)
}

/// What merging `ours` (more specific) over `theirs` does to the values in
/// `theirs`, leaf by leaf. A leaf is anything that is not a non-empty
/// mapping: lists are replaced or joined whole, so they are leaves.
fn diff(
    plan: &dyn MergePlan,
    path: &[String],
    theirs: &Value,
    ours: &Value,
    kind: MergeKind,
    out: &mut Vec<Loss>,
) {
    match (theirs, ours) {
        (Value::Map(theirs), Value::Map(ours)) if !theirs.is_empty() => {
            for (key, value) in theirs {
                let mut child = path.to_vec();
                child.push(key.clone());
                match ours.get(key) {
                    Some(our_value) => {
                        // Beneath a replaced key everything is replaced.
                        // Otherwise the plan says how the child merges.
                        let child_kind = match kind {
                            MergeKind::Replace => MergeKind::Replace,
                            MergeKind::Append => MergeKind::Append,
                            MergeKind::Deep => plan.kind(&child),
                        };
                        diff(plan, &child, value, our_value, child_kind, out);
                    }
                    None if kind == MergeKind::Replace => {
                        let mut leaves = Vec::new();
                        collect_leaves(&mut child, value, &mut leaves);
                        out.extend(leaves.into_iter().map(|(leaf, _)| Loss {
                            at: path.to_vec(),
                            leaf,
                            how: How::Dropped,
                        }));
                    }
                    None => {}
                }
            }
        }
        // An empty mapping holds nothing a more specific mapping could take away.
        (Value::Map(_), Value::Map(_)) => out.push(Loss {
            at: path.to_vec(),
            leaf: path.to_vec(),
            how: How::Restated,
        }),
        // Joined lists keep every item both layers gave.
        (Value::List(_), Value::List(_)) if kind == MergeKind::Append => out.push(Loss {
            at: path.to_vec(),
            leaf: path.to_vec(),
            how: How::Restated,
        }),
        _ => {
            let how = if theirs == ours {
                How::Restated
            } else {
                How::Changed
            };
            let mut leaves = Vec::new();
            collect_leaves(&mut path.to_vec(), theirs, &mut leaves);
            out.extend(leaves.into_iter().map(|(leaf, _)| Loss {
                at: path.to_vec(),
                leaf,
                how,
            }));
        }
    }
}

/// Every leaf beneath `value`, with its path. An empty mapping is a leaf.
fn collect_leaves<'v>(
    path: &mut Vec<String>,
    value: &'v Value,
    out: &mut Vec<(Vec<String>, &'v Value)>,
) {
    match value {
        Value::Map(map) if !map.is_empty() => {
            for (key, child) in map {
                path.push(key.clone());
                collect_leaves(path, child, out);
                path.pop();
            }
        }
        _ => out.push((path.clone(), value)),
    }
}

/// Every scalar beneath `value`, looking inside lists too. A list's items
/// are selected by the list's own key; `shown` adds the index for a person.
fn collect_scalars<'v>(
    path: &mut Vec<String>,
    shown: String,
    value: &'v Value,
    out: &mut Vec<(Vec<String>, String, &'v Value)>,
) {
    match value {
        Value::Map(map) => {
            for (key, child) in map {
                path.push(key.clone());
                let shown = format!("{shown}.{}", display_path(&[key]));
                collect_scalars(path, shown, child, out);
                path.pop();
            }
        }
        Value::List(items) => {
            for (i, child) in items.iter().enumerate() {
                collect_scalars(path, format!("{shown}[{i}]"), child, out);
            }
        }
        _ => out.push((path.clone(), shown, value)),
    }
}

/// A scalar as the text a glob is matched against. `null` has none.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.canonical()),
    }
}

fn same_scalar(forbidden: &Value, value: &Value, ignore_case: bool) -> bool {
    match (forbidden, value) {
        (Value::String(a), Value::String(b)) if ignore_case => a.to_lowercase() == b.to_lowercase(),
        (a, b) => a == b,
    }
}
