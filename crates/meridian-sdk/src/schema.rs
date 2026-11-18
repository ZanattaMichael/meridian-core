//! What a target tells an editor about its resource types.
//!
//! A target already refuses a param or a state it does not take, but only
//! once a document is compiled. A schema says the same thing ahead of time,
//! so an editor can offer a type's params while the author is still typing
//! and explain each one. It is advice: `emit` remains the only judge of
//! whether a document compiles, and a schema must never promise something
//! `emit` refuses.
//!
//! A target that takes its tool's own types, such as `aws_instance`, cannot
//! list every param of every provider. It marks such a type `open`, which
//! tells an editor to accept any param and offer only what is listed. A
//! plugin that can ask its tool for those types serves
//! [`SchemaSource`](crate::SchemaSource), and the host caches what the tool
//! says; those schemas carry a [`source`](ResourceSchema::source).
//!
//! A schema is also data: it is written to the cache as JSON, and read back
//! by an editor that never started the plugin.

use crate::types::{CompileError, Resource};
use std::collections::BTreeMap;

use meridian_value::Value;

/// The shape of one param's value.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    /// Not stated: any value.
    #[default]
    Any,
    String,
    Bool,
    Integer,
    Number,
    List,
    #[serde(rename = "mapping")]
    Map,
}

impl ParamKind {
    /// The name the kind travels under and is shown as.
    pub fn as_str(self) -> &'static str {
        match self {
            ParamKind::Any => "any",
            ParamKind::String => "string",
            ParamKind::Bool => "boolean",
            ParamKind::Integer => "integer",
            ParamKind::Number => "number",
            ParamKind::List => "list",
            ParamKind::Map => "mapping",
        }
    }

    /// The kind a name stands for. A name this build does not know is `Any`,
    /// so a newer plugin's kinds never make an older host refuse it.
    pub fn from_name(name: &str) -> ParamKind {
        match name {
            "string" => ParamKind::String,
            "boolean" => ParamKind::Bool,
            "integer" => ParamKind::Integer,
            "number" => ParamKind::Number,
            "list" => ParamKind::List,
            "mapping" => ParamKind::Map,
            _ => ParamKind::Any,
        }
    }
}

/// One param of a resource type.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ParamSchema {
    pub name: String,
    /// What the param means, for the person filling it in.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub kind: ParamKind,
    /// Whether the target refuses a resource that leaves it out.
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
    /// The values it accepts, when they are a fixed set; empty otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// What a mapping holds, or each item of a list, when that is known:
    /// one entry per key. Empty means any content.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamSchema>,
    /// Whether a mapping accepts keys beyond the ones in `params`.
    #[serde(skip_serializing_if = "is_false")]
    pub open: bool,
    /// Whether the tool sets the value. A document never writes a computed
    /// param, but a reference such as `${id.name}` may read it.
    #[serde(skip_serializing_if = "is_false")]
    pub computed: bool,
    /// Whether the tool still takes the param but warns about it.
    #[serde(skip_serializing_if = "is_false")]
    pub deprecated: bool,
    /// Whether the tool treats the value as a secret.
    #[serde(skip_serializing_if = "is_false")]
    pub sensitive: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ParamSchema {
    pub fn new(name: &str, kind: ParamKind, description: &str) -> Self {
        ParamSchema {
            name: name.to_owned(),
            description: description.to_owned(),
            kind,
            ..ParamSchema::default()
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn values(mut self, values: &[&str]) -> Self {
        self.values = values.iter().map(|v| (*v).to_owned()).collect();
        self
    }

    /// Describes what a mapping, or each item of a list, holds.
    pub fn with_params(mut self, params: Vec<ParamSchema>) -> Self {
        self.params = params;
        self
    }

    /// Says the mapping accepts keys beyond the listed ones.
    pub fn open(mut self) -> Self {
        self.open = true;
        self
    }

    /// Says the tool sets the value and a document does not write it.
    pub fn computed(mut self) -> Self {
        self.computed = true;
        self
    }

    /// The nested param called `name`, if this one lists it.
    pub fn find_param(&self, name: &str) -> Option<&ParamSchema> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// One resource type of a target.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ResourceSchema {
    /// The type as a document writes it. A name with `<` in it, such as
    /// `<provider>_<type>`, is a pattern and not a literal type.
    pub r#type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The states the type accepts. Empty means it takes no state.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    /// The state assumed when a resource declares none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub default_state: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamSchema>,
    /// Whether params beyond the listed ones are accepted.
    #[serde(skip_serializing_if = "is_false")]
    pub open: bool,
    /// The notify actions the type can perform.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// Where the schema came from, when a tool described it: the provider,
    /// package or API server and its version. Empty for a schema the plugin
    /// itself declares.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
}

impl ResourceSchema {
    /// A type whose params are exactly the listed ones.
    pub fn closed(name: &str, description: &str) -> Self {
        ResourceSchema {
            r#type: name.to_owned(),
            description: description.to_owned(),
            ..ResourceSchema::default()
        }
    }

    /// A type that passes its params through to the tool, so any param may
    /// be valid.
    pub fn open(name: &str, description: &str) -> Self {
        ResourceSchema {
            open: true,
            ..ResourceSchema::closed(name, description)
        }
    }

    pub fn states(mut self, states: &[&str], default: &str) -> Self {
        self.states = states.iter().map(|s| (*s).to_owned()).collect();
        self.default_state = default.to_owned();
        self
    }

    pub fn actions(mut self, actions: &[&str]) -> Self {
        self.actions = actions.iter().map(|a| (*a).to_owned()).collect();
        self
    }

    pub fn param(mut self, param: ParamSchema) -> Self {
        self.params.push(param);
        self
    }

    pub fn with_params(mut self, params: Vec<ParamSchema>) -> Self {
        self.params = params;
        self
    }

    /// Whether the type name is a pattern, such as `<provider>_<type>`.
    pub fn is_pattern(&self) -> bool {
        self.r#type.contains('<')
    }

    /// The param called `name`, if the type lists one.
    pub fn find_param(&self, name: &str) -> Option<&ParamSchema> {
        self.params.iter().find(|p| p.name == name)
    }

    /// The param at a path inside `params`, such as `["tags", "Name"]`, and
    /// whether unlisted keys are accepted where the path ends. `None` when
    /// a step names something the schema does not list; the second value
    /// is then meaningless. An empty path is the type itself: `Some(None)`.
    pub fn at_path(&self, path: &[&str]) -> Option<Option<&ParamSchema>> {
        let mut current: Option<&ParamSchema> = None;
        for step in path {
            let next = match current {
                None => self.find_param(step),
                Some(p) => p.find_param(step),
            };
            current = Some(next?);
        }
        Some(current)
    }

    /// Says where the schema was read from.
    pub fn from_source(mut self, source: &str) -> Self {
        self.source = source.to_owned();
        self
    }
}

/// Reads a [JSON Schema](https://json-schema.org/) object into the params
/// of a resource type. Many tools publish their types in it: DSC v3
/// resource manifests, the CloudFormation registry, Pulumi packages, and
/// Kubernetes once its OpenAPI document is resolved.
///
/// `schema` is the JSON Schema as a [`Value`] (see
/// [`decode_value`](crate::decode_value)). `resolve` answers a `$ref` with
/// the schema it points to, or `None` to leave it as any value; a tool
/// whose documents are self-contained passes `|_| None`. What this reads:
/// `properties`, `required`, `type`, `enum`, `description`, `items`,
/// `additionalProperties`, `deprecated`, `readOnly` (as computed) and
/// `writeOnly` (as sensitive), plus `allOf`, `oneOf` and `anyOf` when every
/// branch describes an object (their properties are joined). Nesting stops
/// at [`MAX_SCHEMA_DEPTH`], so a recursive schema ends.
pub fn params_from_json_schema<'a>(
    schema: &'a Value,
    resolve: &dyn Fn(&str) -> Option<&'a Value>,
) -> (Vec<ParamSchema>, bool) {
    let mut seen = Vec::new();
    json_object(schema, resolve, 0, &mut seen)
}

/// How many mappings inside mappings a JSON Schema is read down to.
pub const MAX_SCHEMA_DEPTH: usize = 12;

fn json_text(v: &Value, key: &str) -> String {
    v.as_map()
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn json_flag(v: &Value, key: &str) -> bool {
    matches!(v.as_map().and_then(|m| m.get(key)), Some(Value::Bool(true)))
}

/// Follows `$ref`s until a schema with content is found. A reference that
/// cannot be answered, or that loops, is treated as any value.
fn json_deref<'a>(
    schema: &'a Value,
    resolve: &dyn Fn(&str) -> Option<&'a Value>,
    seen: &mut Vec<String>,
) -> Option<&'a Value> {
    let mut current = schema;
    for _ in 0..MAX_SCHEMA_DEPTH {
        let Some(target) = current
            .as_map()
            .and_then(|m| m.get("$ref"))
            .and_then(Value::as_str)
        else {
            return Some(current);
        };
        if seen.iter().any(|s| s == target) {
            return None;
        }
        seen.push(target.to_owned());
        current = resolve(target)?;
    }
    None
}

/// The keys of an object schema, and whether it accepts other keys.
fn json_object<'a>(
    schema: &'a Value,
    resolve: &dyn Fn(&str) -> Option<&'a Value>,
    depth: usize,
    seen: &mut Vec<String>,
) -> (Vec<ParamSchema>, bool) {
    let mark = seen.len();
    let Some(schema) = json_deref(schema, resolve, seen) else {
        seen.truncate(mark);
        return (Vec::new(), true);
    };
    let Some(map) = schema.as_map() else {
        seen.truncate(mark);
        return (Vec::new(), true);
    };
    let mut params: Vec<ParamSchema> = Vec::new();
    let mut open = match map.get("additionalProperties") {
        Some(Value::Bool(false)) => false,
        // A schema that lists no properties and says nothing about others
        // is a free mapping.
        _ => !map.contains_key("properties"),
    };
    let required: Vec<&str> = map
        .get("required")
        .and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(props) = map.get("properties").and_then(Value::as_map) {
        for (name, prop) in props {
            let mut p = json_param(name, prop, resolve, depth + 1, seen);
            p.required = required.contains(&name.as_str());
            params.push(p);
        }
    }
    // A type made of several object schemas takes the keys of all of them.
    for combinator in ["allOf", "oneOf", "anyOf"] {
        let Some(branches) = map.get(combinator).and_then(Value::as_list) else {
            continue;
        };
        for branch in branches {
            if depth >= MAX_SCHEMA_DEPTH {
                break;
            }
            let (more, branch_open) = json_object(branch, resolve, depth + 1, seen);
            open |= branch_open && combinator != "allOf";
            for p in more {
                if !params.iter().any(|q| q.name == p.name) {
                    params.push(p);
                }
            }
        }
    }
    seen.truncate(mark);
    (params, open)
}

fn json_kind(map: &BTreeMap<String, Value>) -> ParamKind {
    let named = match map.get("type") {
        Some(Value::String(t)) => Some(t.as_str()),
        // `["string", "null"]`: the first kind that is not null.
        Some(Value::List(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null"),
        _ => None,
    };
    match named {
        Some("string") => ParamKind::String,
        Some("boolean") => ParamKind::Bool,
        Some("integer") => ParamKind::Integer,
        Some("number") => ParamKind::Number,
        Some("array") => ParamKind::List,
        Some("object") => ParamKind::Map,
        _ if map.contains_key("properties") => ParamKind::Map,
        _ if map.contains_key("items") => ParamKind::List,
        _ if map.contains_key("enum") => ParamKind::String,
        _ => ParamKind::Any,
    }
}

fn json_param<'a>(
    name: &str,
    schema: &'a Value,
    resolve: &dyn Fn(&str) -> Option<&'a Value>,
    depth: usize,
    seen: &mut Vec<String>,
) -> ParamSchema {
    let mark = seen.len();
    let mut p = ParamSchema::new(name, ParamKind::Any, "");
    let Some(schema) = json_deref(schema, resolve, seen) else {
        seen.truncate(mark);
        return p;
    };
    let Some(map) = schema.as_map() else {
        seen.truncate(mark);
        return p;
    };
    p.description = json_text(schema, "description");
    p.kind = json_kind(map);
    p.deprecated = json_flag(schema, "deprecated");
    p.computed = json_flag(schema, "readOnly");
    p.sensitive = json_flag(schema, "writeOnly");
    if let Some(values) = map.get("enum").and_then(Value::as_list) {
        p.values = values
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Int(i) => Some(i.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            })
            .collect();
    }
    if depth < MAX_SCHEMA_DEPTH {
        match p.kind {
            ParamKind::Map => {
                let (nested, open) = json_object(schema, resolve, depth, seen);
                p.params = nested;
                p.open = open;
            }
            ParamKind::List => {
                if let Some(items) = map.get("items") {
                    let (nested, open) = json_object(items, resolve, depth, seen);
                    p.params = nested;
                    p.open = open;
                }
            }
            _ => {}
        }
    }
    seen.truncate(mark);
    p
}

/// What each of the shared configuration management types is.
const SHARED_TYPES: &[(&str, &str)] = &[
    (
        "exec",
        "A command to run. The escape hatch for what no other type covers.",
    ),
    (
        "file",
        "A file or directory, with its content, owner and mode.",
    ),
    ("group", "A local group."),
    (
        "package",
        "A package installed through the system's package manager.",
    ),
    (
        "service",
        "A system service: whether it runs, and whether it starts at boot.",
    ),
    ("user", "A local user account."),
];

/// The params of the shared configuration management types: type, param,
/// kind, description. One table, so every target that takes these types
/// describes a param in the same words.
const SHARED_PARAMS: &[(&str, &str, ParamKind, &str)] = &[
    (
        "exec",
        "chdir",
        ParamKind::String,
        "The directory to run the command in.",
    ),
    (
        "exec",
        "command",
        ParamKind::String,
        "The command line to run.",
    ),
    (
        "exec",
        "creates",
        ParamKind::String,
        "A path the command creates. The command is skipped when the path already exists.",
    ),
    (
        "exec",
        "removes",
        ParamKind::String,
        "A path the command removes. The command is skipped when the path does not exist.",
    ),
    (
        "exec",
        "shell",
        ParamKind::Bool,
        "Run the command through a shell, so pipes and redirection work.",
    ),
    (
        "file",
        "append",
        ParamKind::Bool,
        "Add the content to the end of the file instead of replacing it.",
    ),
    (
        "file",
        "content",
        ParamKind::String,
        "The file's whole content.",
    ),
    (
        "file",
        "encoding",
        ParamKind::String,
        "How the content is encoded, such as `b64` or `gzip`.",
    ),
    (
        "file",
        "group",
        ParamKind::String,
        "The group that owns the file.",
    ),
    (
        "file",
        "mode",
        ParamKind::String,
        "The permissions, written as a quoted string such as \"0644\" so it is not read \
         as a number.",
    ),
    (
        "file",
        "owner",
        ParamKind::String,
        "The user that owns the file.",
    ),
    (
        "file",
        "path",
        ParamKind::String,
        "Where the file or directory is.",
    ),
    (
        "group",
        "gid",
        ParamKind::Integer,
        "The group's numeric id.",
    ),
    (
        "group",
        "members",
        ParamKind::List,
        "The users that belong to the group.",
    ),
    ("group", "name", ParamKind::String, "The group's name."),
    (
        "group",
        "system",
        ParamKind::Bool,
        "Create a system group, with an id from the system range.",
    ),
    ("package", "name", ParamKind::String, "The package's name."),
    (
        "package",
        "version",
        ParamKind::String,
        "The version to install.",
    ),
    (
        "service",
        "enabled",
        ParamKind::Bool,
        "Whether the service starts when the machine boots.",
    ),
    ("service", "name", ParamKind::String, "The service's name."),
    (
        "user",
        "gecos",
        ParamKind::String,
        "The account's full name or comment.",
    ),
    (
        "user",
        "groups",
        ParamKind::List,
        "The groups the user belongs to, beyond its primary group.",
    ),
    (
        "user",
        "home",
        ParamKind::String,
        "The user's home directory.",
    ),
    (
        "user",
        "lock_passwd",
        ParamKind::Bool,
        "Disable password login for the account.",
    ),
    ("user", "name", ParamKind::String, "The user's login name."),
    (
        "user",
        "primary_group",
        ParamKind::String,
        "The user's primary group.",
    ),
    (
        "user",
        "shell",
        ParamKind::String,
        "The user's login shell.",
    ),
    (
        "user",
        "ssh_authorized_keys",
        ParamKind::List,
        "The public keys allowed to log in as the user.",
    ),
    (
        "user",
        "sudo",
        ParamKind::Any,
        "The user's sudo rule, such as `ALL=(ALL) NOPASSWD:ALL`.",
    ),
    (
        "user",
        "system",
        ParamKind::Bool,
        "Create a system account, with an id from the system range.",
    ),
    ("user", "uid", ParamKind::Integer, "The user's numeric id."),
];

/// Checks a resource of one of the shared configuration management types
/// against [`SHARED_PARAMS`]: every param the table knows must hold a value
/// of the table's kind. A resource of any other type, and a param the table
/// does not list, pass untouched; whether a target accepts the param at all
/// is the target's own rule.
///
/// Every target that takes the shared types calls this first in its
/// transform stage, so the same document is accepted or refused the same way
/// whichever tool it is moved to. The refusal is the standard one: rule
/// `invalid-param`, with the plugin filling in its own target name.
///
/// The reading is the strict one: text where a name is wanted, true or
/// false where a flag is wanted, a list where several are wanted. A secret
/// stands for text the tool resolves, so it passes wherever text would.
pub fn check_shared(resource: &Resource) -> Result<(), CompileError> {
    let r#type = resource.r#type.as_str();
    if !SHARED_TYPES.iter().any(|(t, _)| *t == r#type) {
        return Ok(());
    }
    for (name, value) in &resource.params {
        let Some((_, _, kind, _)) = SHARED_PARAMS
            .iter()
            .find(|(t, p, _, _)| *t == r#type && p == name)
        else {
            continue;
        };
        let fits = match kind {
            ParamKind::Any => true,
            ParamKind::String => matches!(value, Value::String(_) | Value::Secret(_)),
            ParamKind::Bool => matches!(value, Value::Bool(_)),
            ParamKind::Integer => matches!(value, Value::Int(_) | Value::Uint(_)),
            ParamKind::Number => matches!(value, Value::Int(_) | Value::Uint(_) | Value::Float(_)),
            ParamKind::List => matches!(value, Value::List(_)),
            ParamKind::Map => matches!(value, Value::Map(_)),
        };
        if fits {
            continue;
        }
        let wanted = match kind {
            ParamKind::String => "text",
            ParamKind::Bool => "true or false",
            ParamKind::Integer => "a whole number",
            ParamKind::Number => "a number",
            ParamKind::List => "a list",
            ParamKind::Map => "a mapping",
            ParamKind::Any => "anything",
        };
        let mut msg = format!(
            "param {name:?} of a {type} must be {wanted}, not {}",
            value.describe()
        );
        match (r#type, name.as_str(), value) {
            ("file", "mode", Value::Int(_) | Value::Uint(_)) => {
                msg.push_str(&format!(
                    "; write it quoted, so it is not read as a number: \"{}\"",
                    value.canonical()
                ));
            }
            (_, _, Value::List(items)) if *kind == ParamKind::String => {
                msg.push_str(&format!(
                    "; declare one resource per item ({} here)",
                    items.len()
                ));
            }
            (_, _, Value::String(_)) if *kind == ParamKind::List => {
                msg.push_str("; write it as a list: [value]");
            }
            (_, _, Value::String(_)) if *kind == ParamKind::Bool => {
                msg.push_str("; write true or false without quotes");
            }
            _ => {}
        }
        return Err(CompileError::for_resource(resource, "invalid-param", msg));
    }
    Ok(())
}

/// Describes one of the shared configuration management types (`package`,
/// `service`, `file`, `user`, `group`, `exec`) as one target takes it.
///
/// The target passes what it accepts, straight from the table its transform
/// stage checks a resource against; the descriptions and value kinds come
/// from here. A param this module has no words for is still listed, with no
/// description, so a target can never accept something an editor hides.
pub fn shared(
    name: &str,
    params: &[&str],
    required: &[&str],
    states: &[&str],
    default_state: &str,
    actions: &[&str],
) -> ResourceSchema {
    let description = SHARED_TYPES
        .iter()
        .find(|(t, _)| *t == name)
        .map(|(_, d)| *d)
        .unwrap_or_default();
    let mut schema = ResourceSchema::closed(name, description)
        .states(states, default_state)
        .actions(actions);
    let mut names: Vec<&str> = params.to_vec();
    names.sort_unstable();
    names.dedup();
    for param in names {
        let (kind, description) = SHARED_PARAMS
            .iter()
            .find(|(t, p, _, _)| *t == name && *p == param)
            .map(|(_, _, kind, d)| (*kind, *d))
            .unwrap_or((ParamKind::Any, ""));
        let mut param_schema = ParamSchema::new(param, kind, description);
        param_schema.required = required.contains(&param);
        schema.params.push(param_schema);
    }
    schema.states.sort();
    schema.states.dedup();
    schema.actions.sort();
    schema.actions.dedup();
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(r#type: &str, params: &[(&str, Value)]) -> Resource {
        Resource {
            id: "a".to_owned(),
            r#type: r#type.to_owned(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            ..Resource::default()
        }
    }

    #[test]
    fn shared_params_are_held_to_their_kind() {
        let text = |s: &str| Value::String(s.to_owned());
        for (r#type, params) in [
            ("package", vec![("name", text("nginx"))]),
            (
                "service",
                vec![("name", text("nginx")), ("enabled", Value::Bool(true))],
            ),
            (
                "file",
                vec![("path", text("/etc/x")), ("mode", text("0644"))],
            ),
            (
                "user",
                vec![
                    ("name", text("deploy")),
                    ("uid", Value::Int(1500)),
                    ("groups", Value::List(vec![text("adm")])),
                ],
            ),
            ("user", vec![("sudo", Value::List(Vec::new()))]),
            ("group", vec![("gid", Value::Uint(7))]),
            // A secret stands for text.
            (
                "file",
                vec![("content", Value::Secret(meridian_value::SecretRef::new(1)))],
            ),
            // A type and a param the table does not know are not this check's.
            ("firewall", vec![("name", Value::Int(1))]),
            ("package", vec![("provider", Value::Int(1))]),
        ] {
            assert!(
                check_shared(&resource(r#type, &params)).is_ok(),
                "{type} {params:?}"
            );
        }
        let refused = |r#type: &str, params: &[(&str, Value)]| {
            let e = check_shared(&resource(r#type, params)).expect_err("refused");
            assert_eq!(e.rule, "invalid-param");
            assert_eq!(e.resource, "a");
            e.msg
        };
        assert_eq!(
            refused("package", &[("name", Value::Int(42))]),
            "param \"name\" of a package must be text, not a number"
        );
        assert_eq!(
            refused("package", &[("name", Value::List(vec![text("a"), text("b")]))]),
            "param \"name\" of a package must be text, not a list; declare one resource per item (2 here)"
        );
        assert_eq!(
            refused("service", &[("enabled", text("yes"))]),
            "param \"enabled\" of a service must be true or false, not a string; write true or false without quotes"
        );
        assert_eq!(
            refused("file", &[("mode", Value::Int(420))]),
            "param \"mode\" of a file must be text, not a number; write it quoted, so it is not read as a number: \"420\""
        );
        assert_eq!(
            refused("user", &[("groups", text("adm"))]),
            "param \"groups\" of a user must be a list, not a string; write it as a list: [value]"
        );
        assert_eq!(
            refused("user", &[("uid", text("1500"))]),
            "param \"uid\" of a user must be a whole number, not a string"
        );
    }

    #[test]
    fn a_shared_type_is_described_from_the_one_table() {
        let schema = shared(
            "service",
            &["name", "enabled"],
            &["name"],
            &["stopped", "running"],
            "running",
            &["restart"],
        );
        assert!(!schema.open);
        assert_eq!(schema.states, ["running", "stopped"]);
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["enabled", "name"]);
        let name = schema.find_param("name").unwrap();
        assert!(name.required && !name.description.is_empty());
        assert_eq!(schema.find_param("enabled").unwrap().kind, ParamKind::Bool);
    }

    #[test]
    fn a_param_without_words_is_still_listed() {
        let schema = shared("package", &["name", "flavour"], &[], &[], "", &[]);
        let odd = schema.find_param("flavour").unwrap();
        assert_eq!(odd.kind, ParamKind::Any);
        assert!(odd.description.is_empty());
    }

    #[test]
    fn a_json_schema_becomes_params() {
        let text = br##"{
            "type": "object",
            "required": ["name"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "description": "The name."},
                "size": {"type": "integer", "enum": [1, 2]},
                "tags": {"type": "object", "additionalProperties": {"type": "string"}},
                "spec": {"$ref": "#/defs/spec"},
                "ports": {"type": "array", "items": {"$ref": "#/defs/port"}},
                "arn": {"type": "string", "readOnly": true},
                "old": {"type": ["string", "null"], "deprecated": true}
            }
        }"##;
        let defs = meridian_value::decode_value(
            br##"{
                "spec": {"type": "object", "properties": {"replicas": {"type": "integer"}}},
                "port": {"type": "object", "properties": {"port": {"type": "integer"}, "next": {"$ref": "#/defs/port"}}}
            }"##,
        )
        .unwrap();
        let schema = meridian_value::decode_value(text).unwrap();
        let resolve = |r: &str| defs.as_map().unwrap().get(r.trim_start_matches("#/defs/"));
        let (params, open) = params_from_json_schema(&schema, &resolve);
        assert!(!open);
        let find = |n: &str| params.iter().find(|p| p.name == n).unwrap();
        assert!(find("name").required && find("name").kind == ParamKind::String);
        assert_eq!(find("name").description, "The name.");
        assert_eq!(find("size").values, ["1", "2"]);
        assert!(find("tags").kind == ParamKind::Map && find("tags").open);
        assert_eq!(find("spec").params[0].name, "replicas");
        assert_eq!(find("ports").kind, ParamKind::List);
        let item = |n: &str| {
            find("ports")
                .params
                .iter()
                .find(|p| p.name == n)
                .unwrap()
                .clone()
        };
        assert_eq!(item("port").kind, ParamKind::Integer);
        // The recursive reference ends instead of looping.
        assert!(item("next").params.is_empty());
        assert!(find("arn").computed);
        assert!(find("old").deprecated && find("old").kind == ParamKind::String);
        let schema = ResourceSchema::closed("t", "").with_params(params);
        assert_eq!(
            schema.at_path(&["spec", "replicas"]).unwrap().unwrap().kind,
            ParamKind::Integer
        );
        assert!(schema.at_path(&["spec", "nothing"]).is_none());
        assert!(schema.at_path(&[]).unwrap().is_none());
    }

    #[test]
    fn a_schema_survives_json() {
        let schema = ResourceSchema::open("aws_instance", "An instance.")
            .param(
                ParamSchema::new("ami", ParamKind::String, "The image.")
                    .required()
                    .values(&["a", "b"]),
            )
            .param(
                ParamSchema::new("tags", ParamKind::Map, "")
                    .with_params(vec![ParamSchema::new("Name", ParamKind::String, "")])
                    .open(),
            )
            .param(ParamSchema::new("arn", ParamKind::String, "").computed())
            .from_source("hashicorp/aws 5.60.0");
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("\"mapping\""), "{text}");
        assert!(
            !text.contains("deprecated"),
            "a false flag is left out: {text}"
        );
        let back: ResourceSchema = serde_json::from_str(&text).unwrap();
        assert_eq!(back, schema);
    }

    #[test]
    fn a_kind_survives_its_name() {
        for kind in [
            ParamKind::Any,
            ParamKind::String,
            ParamKind::Bool,
            ParamKind::Integer,
            ParamKind::Number,
            ParamKind::List,
            ParamKind::Map,
        ] {
            assert_eq!(ParamKind::from_name(kind.as_str()), kind);
        }
        assert_eq!(ParamKind::from_name("tensor"), ParamKind::Any);
    }
}
