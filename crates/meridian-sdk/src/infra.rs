//! The common infrastructure resource types: `network`, `subnet`, `firewall`,
//! `address`, `vm`, `disk` and `objectStore`.
//!
//! Most of what an infrastructure document describes is the same on every
//! cloud: a network, a subnet in it, a firewall, a fixed address, a machine.
//! The common types say those things once, and each infrastructure as code
//! target that takes them maps every one to its tool's own resource for the
//! cloud the document is aimed at (issue 151). They are the infrastructure
//! counterpart of the six configuration management types in
//! [`crate::schema`], and they follow the same rules: the vocabulary lives
//! here, in one place, so no target can drift; a param no mapping takes is
//! refused, never dropped; and a target that cannot express something says
//! so at compile time.
//!
//! A common type is used beside the tool's own types, not instead of them. A
//! document mixes `vm` with an `aws_instance` where it needs something the
//! common type does not have, and a common resource is still referred to by
//! its id (`${web.id}`), so the tool's own attributes stay reachable; what
//! is promised on every target are the outputs listed here.
//!
//! How a target uses this module: its transform stage calls
//! [`check_common`] on each resource, so the vocabulary is read the same way
//! everywhere, then [`lower`] with a function that turns each common
//! resource into the native ones it becomes on the chosen cloud. `lower`
//! replaces the common resources in the tree, rewrites every reference to a
//! common output (`${web.publicAddress}`) to the tool's own expression, and
//! hands back a tree the target's existing stages compile as if the author
//! had written the native resources themselves.

use std::collections::BTreeMap;

use crate::expr::{self, Part};
use crate::rules;
use crate::types::{CompileError, Edge, EdgeKind, Resource, ResourceGraph};
use crate::{ParamKind, ParamSchema, ResourceSchema, Value};

/// What each common infrastructure type is, sorted by name.
const COMMON_TYPES: &[(&str, &str)] = &[
    (
        "address",
        "A fixed public IP address, which a vm takes with its `address` param.",
    ),
    ("disk", "A data disk, attached to a vm with its `vm` param."),
    (
        "firewall",
        "A set of rules saying which traffic may reach, or leave, the machines it is attached to.",
    ),
    (
        "network",
        "A private network with its own address range: a VPC, a virtual network.",
    ),
    (
        "objectStore",
        "A store of files reached over HTTPS: an S3 bucket, an Azure storage account with one \
         blob container, a Cloud Storage bucket.",
    ),
    (
        "subnet",
        "A range of addresses inside a network, where machines are placed.",
    ),
    (
        "vm",
        "A virtual machine in a subnet, from an image, of a size, reachable with a key.",
    ),
];

/// The params of the common types: type, param, kind, required, description.
/// One table, so every target describes a param in the same words and
/// checks its kind the same way.
const COMMON_PARAMS: &[(&str, &str, ParamKind, bool, &str)] = &[
    ("address", "location", ParamKind::String, false, LOCATION),
    ("address", "name", ParamKind::String, false, NAME),
    ("address", "tags", ParamKind::Map, false, TAGS),
    (
        "disk",
        "location",
        ParamKind::String,
        false,
        "On AWS, the availability zone, needed only when there is no `vm`. On Azure, as \
         `location` of any type. On Google Cloud, the zone; the vm's zone unless given.",
    ),
    ("disk", "name", ParamKind::String, false, NAME),
    (
        "disk",
        "sizeGb",
        ParamKind::Integer,
        true,
        "The disk's size, in gigabytes.",
    ),
    ("disk", "tags", ParamKind::Map, false, TAGS),
    (
        "disk",
        "vm",
        ParamKind::String,
        false,
        "The vm the disk is attached to, as a reference: `${web_vm}`. Disks of one vm are \
         attached in document order.",
    ),
    ("firewall", "location", ParamKind::String, false, LOCATION),
    ("firewall", "name", ParamKind::String, false, NAME),
    (
        "firewall",
        "network",
        ParamKind::String,
        true,
        "The network the firewall belongs to, as a reference: `${net}`.",
    ),
    (
        "firewall",
        "rules",
        ParamKind::List,
        false,
        "The traffic allowed, one mapping per rule: `direction` (`in`, the default, or `out`), \
         `protocol` (`tcp`, the default, `udp`, `icmp` or `any`), `port` (one port, `80`, or a \
         range, `8000-9000`; left out for every port) and `cidr` (where the traffic comes from, \
         or goes to; `0.0.0.0/0` for anywhere). Everything not allowed is refused. A firewall \
         with no `out` rule lets everything out, which is every cloud's default.",
    ),
    ("firewall", "tags", ParamKind::Map, false, TAGS),
    (
        "network",
        "cidr",
        ParamKind::String,
        true,
        "The network's address range, such as `10.0.0.0/16`.",
    ),
    ("network", "location", ParamKind::String, false, LOCATION),
    ("network", "name", ParamKind::String, false, NAME),
    ("network", "tags", ParamKind::Map, false, TAGS),
    (
        "objectStore",
        "location",
        ParamKind::String,
        false,
        "Refused on AWS, where the provider's or the stack's region decides. On Azure, as \
         `location` of any type. On Google Cloud, the bucket's location; \
         `spec.settings.location` unless given.",
    ),
    (
        "objectStore",
        "name",
        ParamKind::String,
        false,
        "The store's name in the cloud, unique across the cloud. The resource's id unless \
         given; on Azure, the id with only its lower-case letters and digits.",
    ),
    ("objectStore", "tags", ParamKind::Map, false, TAGS),
    (
        "objectStore",
        "versioning",
        ParamKind::Bool,
        false,
        "Whether an overwritten or deleted file is kept as an older version. Off unless given.",
    ),
    (
        "subnet",
        "cidr",
        ParamKind::String,
        true,
        "The subnet's address range, inside the network's, such as `10.0.1.0/24`.",
    ),
    (
        "subnet",
        "location",
        ParamKind::String,
        false,
        "On AWS, the availability zone the subnet is in, such as `eu-west-1a`. Refused on \
         Azure, where a subnet is wherever its network is.",
    ),
    ("subnet", "name", ParamKind::String, false, NAME),
    (
        "subnet",
        "network",
        ParamKind::String,
        true,
        "The network the subnet is in, as a reference: `${net}`.",
    ),
    (
        "subnet",
        "public",
        ParamKind::Bool,
        false,
        "Whether a machine in the subnet gets a public address of its own and can reach the \
         internet. Off unless given.",
    ),
    ("subnet", "tags", ParamKind::Map, false, TAGS),
    (
        "vm",
        "address",
        ParamKind::String,
        false,
        "A fixed public address to attach, as a reference to an `address` resource: `${ip}`.",
    ),
    (
        "vm",
        "firewalls",
        ParamKind::List,
        false,
        "The firewalls that apply to the machine, as references: `[${web_fw}]`.",
    ),
    (
        "vm",
        "image",
        ParamKind::Any,
        true,
        "What the machine boots: a mapping with `family` (`ubuntu` or `debian`) and `version` \
         (`24.04`, `22.04`, `12`), which the target turns into the cloud's own image and shows in \
         the compiled file; or the cloud's own image written as text, which is passed through.",
    ),
    ("vm", "location", ParamKind::String, false, LOCATION),
    ("vm", "name", ParamKind::String, false, NAME),
    (
        "vm",
        "size",
        ParamKind::Any,
        true,
        "How big the machine is: a mapping with `cpus` and `memoryGb`, which the target turns \
         into the smallest of the cloud's own sizes that has at least that and shows in the \
         compiled file; or the cloud's own size written as text, which is passed through.",
    ),
    (
        "vm",
        "sshKey",
        ParamKind::String,
        false,
        "The public key that may log in, as text or as a secret.",
    ),
    (
        "vm",
        "subnet",
        ParamKind::String,
        true,
        "The subnet the machine is in, as a reference: `${web}`.",
    ),
    ("vm", "tags", ParamKind::Map, false, TAGS),
    (
        "vm",
        "user",
        ParamKind::String,
        false,
        "The login the key is for. Where a cloud decides the login from the image, the param is \
         refused rather than ignored.",
    ),
];

const LOCATION: &str =
    "Where the resource is placed, as an Azure location such as `westeurope`. Unless \
     given: `spec.settings.location` on Terraform, the resource group's location on Bicep. \
     Refused on AWS, where a resource is in the region its provider is configured for.";
const NAME: &str = "The resource's name in the cloud. The resource's id unless given.";
const TAGS: &str = "Labels on the resource, as a mapping of text to text.";

/// The outputs every target promises for a common type: type, output,
/// description. A document reads them as `${id.output}`; the target rewrites
/// each to the tool's own expression.
const COMMON_OUTPUTS: &[(&str, &str, &str)] = &[
    ("address", "address", "The IP address."),
    ("address", "id", "The cloud's own id for the address."),
    ("disk", "id", "The cloud's own id for the disk."),
    ("firewall", "id", "The cloud's own id for the firewall."),
    ("network", "id", "The cloud's own id for the network."),
    (
        "objectStore",
        "id",
        "The cloud's own id for the store: the bucket, or the Azure storage account.",
    ),
    ("subnet", "id", "The cloud's own id for the subnet."),
    ("vm", "id", "The cloud's own id for the machine."),
    (
        "vm",
        "privateAddress",
        "The machine's address inside its network.",
    ),
    (
        "vm",
        "publicAddress",
        "The machine's address from the internet: the `address` attached to it, or the one the \
         subnet gave it.",
    ),
];

/// The image families a `vm` may ask for by name, and the versions of each.
/// A target keeps the cloud's own image for every pair.
pub const IMAGE_FAMILIES: &[(&str, &[&str])] =
    &[("debian", &["12", "11"]), ("ubuntu", &["24.04", "22.04"])];

/// The directions a firewall rule may have, and the one assumed.
pub const DIRECTIONS: &[&str] = &["in", "out"];
/// The protocols a firewall rule may name, and the one assumed.
pub const PROTOCOLS: &[&str] = &["any", "icmp", "tcp", "udp"];

/// The names of the common infrastructure types, sorted.
pub fn common_type_names() -> Vec<&'static str> {
    COMMON_TYPES.iter().map(|(name, _)| *name).collect()
}

/// Whether `name` is one of the common infrastructure types.
pub fn is_common_type(name: &str) -> bool {
    COMMON_TYPES.iter().any(|(t, _)| *t == name)
}

/// The params of a common type, sorted: name, whether it is required.
pub fn common_params(name: &str) -> Vec<(&'static str, bool)> {
    COMMON_PARAMS
        .iter()
        .filter(|(t, _, _, _, _)| *t == name)
        .map(|(_, p, _, required, _)| (*p, *required))
        .collect()
}

/// The outputs of a common type, sorted.
pub fn common_outputs(name: &str) -> Vec<&'static str> {
    COMMON_OUTPUTS
        .iter()
        .filter(|(t, _, _)| *t == name)
        .map(|(_, o, _)| *o)
        .collect()
}

/// Describes a common type as one target takes it, for editors.
///
/// `params` is what the target's mapping accepts, from the same table its
/// transform stage checks a resource against; descriptions, kinds and
/// which params are required come from here. A param this module has no
/// words for is still listed, so a target can never accept something an
/// editor hides.
pub fn common_schema(name: &str, params: &[&str]) -> ResourceSchema {
    let description = COMMON_TYPES
        .iter()
        .find(|(t, _)| *t == name)
        .map(|(_, d)| *d)
        .unwrap_or_default();
    let outputs = common_outputs(name);
    let description = if outputs.is_empty() {
        description.to_owned()
    } else {
        format!(
            "{description} Reads on every target: {}.",
            outputs
                .iter()
                .map(|o| format!("`${{id.{o}}}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut schema = ResourceSchema::closed(name, &description).states(&["present"], "present");
    let mut names: Vec<&str> = params.to_vec();
    names.sort_unstable();
    names.dedup();
    for param in names {
        let (kind, required, description) = COMMON_PARAMS
            .iter()
            .find(|(t, p, _, _, _)| *t == name && *p == param)
            .map(|(_, _, kind, required, d)| (*kind, *required, *d))
            .unwrap_or((ParamKind::Any, false, ""));
        let mut param_schema = ParamSchema::new(param, kind, description);
        param_schema.required = required;
        match (name, param) {
            ("vm", "size") => {
                param_schema.params = vec![
                    ParamSchema::new("cpus", ParamKind::Integer, "How many processors."),
                    ParamSchema::new(
                        "memoryGb",
                        ParamKind::Number,
                        "How much memory, in gigabytes.",
                    ),
                ];
            }
            ("vm", "image") => {
                let families: Vec<&str> = IMAGE_FAMILIES.iter().map(|(f, _)| *f).collect();
                param_schema.params = vec![
                    ParamSchema::new("family", ParamKind::String, "The operating system.")
                        .values(&families),
                    ParamSchema::new("version", ParamKind::String, "Its release."),
                ];
            }
            ("firewall", "rules") => {
                param_schema.params = vec![
                    ParamSchema::new(
                        "direction",
                        ParamKind::String,
                        "`in` (the default) or `out`.",
                    )
                    .values(DIRECTIONS),
                    ParamSchema::new("protocol", ParamKind::String, "`tcp` unless given.")
                        .values(PROTOCOLS),
                    ParamSchema::new(
                        "port",
                        ParamKind::String,
                        "One port, or a range `from-to`; every port when left out.",
                    ),
                    ParamSchema::new(
                        "cidr",
                        ParamKind::String,
                        "Where the traffic comes from, or goes to.",
                    )
                    .required(),
                ];
            }
            _ => {}
        }
        schema.params.push(param_schema);
    }
    schema
}

/// One rule of a `firewall`, as [`check_common`] read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirewallRule {
    /// `in` or `out`.
    pub direction: String,
    /// `tcp`, `udp`, `icmp` or `any`.
    pub protocol: String,
    /// The first and last port, or `None` for every port.
    pub ports: Option<(u16, u16)>,
    /// The address range the traffic comes from (`in`) or goes to (`out`).
    pub cidr: String,
}

/// Reads the `rules` of a firewall resource that [`check_common`] accepted.
/// A resource that did not pass the check may hold anything, so this is
/// only called after it.
pub fn firewall_rules(resource: &Resource) -> Vec<FirewallRule> {
    let Some(Value::List(items)) = resource.params.get("rules") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_map)
        .map(|rule| {
            let text = |key: &str, default: &str| {
                rule.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or(default)
                    .to_owned()
            };
            FirewallRule {
                direction: text("direction", "in"),
                protocol: text("protocol", "tcp"),
                ports: rule.get("port").and_then(port_range),
                cidr: text("cidr", ""),
            }
        })
        .collect()
}

/// `80` or `8000-9000` as a pair of ports; `None` for anything else.
fn port_range(value: &Value) -> Option<(u16, u16)> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Uint(u) => u.to_string(),
        _ => return None,
    };
    let (from, to) = match text.split_once('-') {
        Some((from, to)) => (from, to),
        None => (text.as_str(), text.as_str()),
    };
    let (from, to) = (from.parse::<u16>().ok()?, to.parse::<u16>().ok()?);
    (from <= to).then_some((from, to))
}

/// The id a value refers to, when the value is exactly one reference to a
/// resource and nothing more: `${net}`.
pub fn referenced_id(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let parts = expr::parse(text).ok()?;
    let [Part::Expr(expression)] = parts.as_slice() else {
        return None;
    };
    let (name, rest) = expr::head(expression);
    (!name.is_empty() && rest.is_empty()).then(|| name.to_owned())
}

/// Checks a resource of one of the common infrastructure types against the
/// vocabulary: every param must be one the type has, of the kind the table
/// gives; every required param must be present; the state, if any, must be
/// `present`; and the mappings and lists with a fixed shape (`size`,
/// `image`, `rules`, `firewalls`, `tags`) must have it. A resource of any
/// other type passes untouched.
///
/// Every target that takes the common types calls this first, so the same
/// document is accepted or refused the same way whichever cloud it is aimed
/// at. Which of the vocabulary a target can express is the target's own
/// rule, checked after this one. The refusal is the standard one, with the
/// target filling in its own name.
pub fn check_common(resource: &Resource) -> Result<(), CompileError> {
    let r#type = resource.r#type.as_str();
    if !is_common_type(r#type) {
        return Ok(());
    }
    let refuse = |rule: &rules::Rule, msg: String| CompileError::for_resource(resource, rule, msg);
    match resource.state.as_str() {
        "" | "present" => {}
        other => {
            return Err(refuse(
                &rules::UNSUPPORTED_STATE,
                format!(
                    "a {type} takes no state but present, not {other:?}: what the document no \
                     longer declares, the tool removes"
                ),
            ))
        }
    }
    let params = common_params(r#type);
    for (name, value) in &resource.params {
        let Some((_, _, kind, _, _)) = COMMON_PARAMS
            .iter()
            .find(|(t, p, _, _, _)| *t == r#type && p == name)
        else {
            return Err(refuse(
                &rules::UNSUPPORTED_PARAM,
                format!(
                    "a {type} takes no param {name:?} (accepted: {}); for what the common type \
                     does not cover, write the tool's own resource type beside it",
                    params
                        .iter()
                        .map(|(p, _)| *p)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        };
        if matches!(value, Value::Null) {
            return Err(refuse(
                &rules::EMPTY_PARAM,
                format!("param {name:?} of the {type} is given with no value"),
            ));
        }
        let fits = match kind {
            ParamKind::Any => true,
            ParamKind::String => matches!(value, Value::String(_) | Value::Secret(_)),
            ParamKind::Bool => matches!(value, Value::Bool(_)),
            ParamKind::Integer => matches!(value, Value::Int(_) | Value::Uint(_)),
            ParamKind::Number => matches!(value, Value::Int(_) | Value::Uint(_) | Value::Float(_)),
            ParamKind::List => matches!(value, Value::List(_)),
            ParamKind::Map => matches!(value, Value::Map(_)),
        };
        if !fits {
            let wanted = match kind {
                ParamKind::String => "text",
                ParamKind::Bool => "true or false",
                ParamKind::Integer => "a whole number",
                ParamKind::Number => "a number",
                ParamKind::List => "a list",
                ParamKind::Map => "a mapping",
                ParamKind::Any => "anything",
            };
            return Err(refuse(
                &rules::INVALID_PARAM,
                format!(
                    "param {name:?} of a {type} must be {wanted}, not {}",
                    value.describe()
                ),
            ));
        }
        check_shape(resource, r#type, name, value)?;
    }
    for (name, required) in params {
        if required && !resource.params.contains_key(name) {
            return Err(refuse(
                &rules::MISSING_PARAM,
                format!("a {type} needs the param {name:?}"),
            ));
        }
    }
    Ok(())
}

/// The fixed shapes inside the vocabulary.
fn check_shape(
    resource: &Resource,
    r#type: &str,
    name: &str,
    value: &Value,
) -> Result<(), CompileError> {
    let invalid = |msg: String| CompileError::for_resource(resource, &rules::INVALID_PARAM, msg);
    let is_text = |v: &Value| matches!(v, Value::String(_) | Value::Secret(_));
    match (r#type, name) {
        (_, "tags") => {
            if let Some(map) = value.as_map() {
                if let Some((key, v)) = map.iter().find(|(_, v)| !is_text(v)) {
                    return Err(invalid(format!(
                        "tag {key:?} must be text, not {}",
                        v.describe()
                    )));
                }
            }
        }
        (_, "network") | ("vm", "subnet") | ("vm", "address") | ("disk", "vm") => {
            if referenced_id(value).is_none() {
                return Err(invalid(format!(
                    "param {name:?} of a {type} must be a reference to one resource of this \
                     document, written ${{id}}, not {}",
                    value.canonical()
                )));
            }
        }
        ("disk", "sizeGb") => {
            if !matches!(value, Value::Int(i) if *i > 0)
                && !matches!(value, Value::Uint(u) if *u > 0)
            {
                return Err(invalid(format!(
                    "sizeGb must be a whole number above zero, not {}",
                    value.canonical()
                )));
            }
        }
        ("vm", "firewalls") => {
            if let Some(items) = value.as_list() {
                if let Some(item) = items.iter().find(|v| referenced_id(v).is_none()) {
                    return Err(invalid(format!(
                        "each item of \"firewalls\" must be a reference to one resource of this \
                         document, written ${{id}}, not {}",
                        item.canonical()
                    )));
                }
            }
        }
        ("vm", "size") => match value {
            Value::String(_) | Value::Secret(_) => {}
            Value::Map(map) => {
                let whole = |key: &str| match map.get(key) {
                    Some(Value::Int(i)) if *i > 0 => Ok(()),
                    Some(Value::Uint(u)) if *u > 0 => Ok(()),
                    Some(Value::Float(f)) if *f > 0.0 && key == "memoryGb" => Ok(()),
                    Some(other) => Err(invalid(format!(
                        "size.{key} must be a number above zero, not {}",
                        other.canonical()
                    ))),
                    None => Err(invalid(format!(
                        "a size written as a mapping needs {key} (and {})",
                        if key == "cpus" { "memoryGb" } else { "cpus" }
                    ))),
                };
                whole("cpus")?;
                whole("memoryGb")?;
                if let Some(key) = map.keys().find(|k| *k != "cpus" && *k != "memoryGb") {
                    return Err(invalid(format!(
                        "a size written as a mapping takes cpus and memoryGb, not {key:?}; \
                         for the cloud's own size, write it as text"
                    )));
                }
            }
            other => {
                return Err(invalid(format!(
                    "size must be a mapping of cpus and memoryGb, or the cloud's own size as \
                     text, not {}",
                    other.describe()
                )))
            }
        },
        ("vm", "image") => match value {
            Value::String(_) | Value::Secret(_) => {}
            Value::Map(map) => {
                let family = map.get("family").and_then(Value::as_str).ok_or_else(|| {
                    invalid(
                        "an image written as a mapping needs family and version as text".to_owned(),
                    )
                })?;
                let version = map.get("version").and_then(Value::as_str).ok_or_else(|| {
                    invalid(
                        "an image written as a mapping needs family and version as text; \
                         quote the version (\"24.04\") so it is not read as a number"
                            .to_owned(),
                    )
                })?;
                let Some((_, versions)) = IMAGE_FAMILIES.iter().find(|(f, _)| *f == family) else {
                    return Err(invalid(format!(
                        "image.family {family:?} is not one the common vm knows (accepted: {}); \
                         for any other image, write the cloud's own image as text",
                        IMAGE_FAMILIES
                            .iter()
                            .map(|(f, _)| *f)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                };
                if !versions.contains(&version) {
                    return Err(invalid(format!(
                        "image.version {version:?} is not a {family} release the common vm \
                         knows (accepted: {})",
                        versions.join(", ")
                    )));
                }
                if let Some(key) = map.keys().find(|k| *k != "family" && *k != "version") {
                    return Err(invalid(format!(
                        "an image written as a mapping takes family and version, not {key:?}"
                    )));
                }
            }
            other => {
                return Err(invalid(format!(
                    "image must be a mapping of family and version, or the cloud's own image as \
                     text, not {}",
                    other.describe()
                )))
            }
        },
        ("firewall", "rules") => {
            let Some(items) = value.as_list() else {
                return Ok(());
            };
            for (i, item) in items.iter().enumerate() {
                let Some(rule) = item.as_map() else {
                    return Err(invalid(format!(
                        "rules[{i}] must be a mapping with direction, protocol, port and cidr, \
                         not {}",
                        item.describe()
                    )));
                };
                for (key, v) in rule {
                    match key.as_str() {
                        "direction" => {
                            if !v.as_str().is_some_and(|d| DIRECTIONS.contains(&d)) {
                                return Err(invalid(format!(
                                    "rules[{i}].direction must be one of {}, not {}",
                                    DIRECTIONS.join(", "),
                                    v.canonical()
                                )));
                            }
                        }
                        "protocol" => {
                            if !v.as_str().is_some_and(|p| PROTOCOLS.contains(&p)) {
                                return Err(invalid(format!(
                                    "rules[{i}].protocol must be one of {}, not {}",
                                    PROTOCOLS.join(", "),
                                    v.canonical()
                                )));
                            }
                        }
                        "port" => {
                            if port_range(v).is_none() {
                                return Err(invalid(format!(
                                    "rules[{i}].port must be one port, such as \"80\", or a range \
                                     from low to high, such as \"8000-9000\", not {}",
                                    v.canonical()
                                )));
                            }
                        }
                        "cidr" => {
                            if !v.as_str().is_some_and(|c| !c.is_empty()) {
                                return Err(invalid(format!(
                                    "rules[{i}].cidr must be an address range as text, such as \
                                     \"0.0.0.0/0\", not {}",
                                    v.canonical()
                                )));
                            }
                        }
                        other => {
                            return Err(invalid(format!(
                                "rules[{i}] takes direction, protocol, port and cidr, not \
                                 {other:?}"
                            )))
                        }
                    }
                }
                if !rule.contains_key("cidr") {
                    return Err(invalid(format!("rules[{i}] needs a cidr")));
                }
                if rule.get("protocol").and_then(Value::as_str) == Some("icmp")
                    && rule.contains_key("port")
                {
                    return Err(invalid(format!(
                        "rules[{i}]: an icmp rule has no port; remove it"
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// What one common resource becomes on a target.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expansion {
    /// The native resources, in the order the target wants them. The one
    /// that carries the common resource's id stands for it: `${id}` and
    /// `dependsOn` keep working, and the tool's own attributes of that
    /// resource stay readable as `${id.attribute}`. Any other resource takes
    /// an id of its own, derived from the common one.
    pub resources: Vec<Resource>,
    /// Each common output the target maps, and the expression that reads it,
    /// such as `${ip.public_ip}`. An output a target cannot read is left
    /// out, and a document that reads it is refused.
    pub outputs: BTreeMap<String, String>,
    /// Orders the tool cannot infer from a reference, as (first, then): the
    /// resource `first` is applied before `then`. Each becomes a
    /// `dependsOn` edge, placed at the common resource.
    pub order: Vec<(String, String)>,
}

/// Replaces every common resource of the tree with what `expand` makes of
/// it, and rewrites every read of a common output (`${web.publicAddress}`),
/// in params and in the document's outputs, to the expression the expansion
/// gave. Resources of other types pass through unchanged; a read of
/// anything else on a common resource (`${web.arn}`) is left for the tool,
/// as it is on a native resource.
///
/// `expand` is called once per common resource, in document order, with the
/// whole tree beside it so a mapping can see what a reference points at.
pub fn lower(
    graph: &ResourceGraph,
    mut expand: impl FnMut(&Resource, &ResourceGraph) -> Result<Expansion, CompileError>,
) -> Result<ResourceGraph, CompileError> {
    let mut out = graph.clone();
    out.resources.clear();
    let mut reads: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for resource in &graph.resources {
        if !is_common_type(&resource.r#type) {
            out.resources.push(resource.clone());
            continue;
        }
        let expansion = expand(resource, graph)?;
        debug_assert!(
            expansion.resources.iter().any(|r| r.id == resource.id),
            "an expansion keeps the common resource's id"
        );
        reads.insert(resource.id.clone(), expansion.outputs);
        out.edges
            .extend(expansion.order.into_iter().map(|(from, to)| Edge {
                from,
                to,
                kind: EdgeKind::HardOrder,
                action: String::new(),
                pos: resource.pos.clone(),
            }));
        out.resources.extend(expansion.resources);
    }
    if reads.is_empty() {
        return Ok(out);
    }
    for resource in &mut out.resources {
        let at = Resource {
            params: BTreeMap::new(),
            ..resource.clone()
        };
        for (name, value) in &mut resource.params {
            *value = rewrite(value, &reads).map_err(|msg| {
                CompileError::for_resource(
                    &at,
                    &rules::INVALID_REFERENCE,
                    format!("param {name:?}: {msg}"),
                )
            })?;
        }
    }
    for output in &mut out.outputs {
        let value = Value::String(output.value.clone());
        output.value = match rewrite(&value, &reads).map_err(|msg| {
            CompileError::at(
                "",
                &output.pos,
                &rules::INVALID_REFERENCE,
                format!("output {:?}: {msg}", output.name),
            )
        })? {
            Value::String(text) => text,
            _ => unreachable!("a string is rewritten to a string"),
        };
    }
    Ok(out)
}

fn rewrite(
    value: &Value,
    reads: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => Value::String(rewrite_text(text, reads)?),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| rewrite(item, reads))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, reads)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn rewrite_text(
    text: &str,
    reads: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<String, String> {
    if !text.contains("${") {
        return Ok(text.to_owned());
    }
    // Text the reference syntax cannot read is left for the target, whose
    // own check names the problem.
    let Ok(parts) = expr::parse(text) else {
        return Ok(text.to_owned());
    };
    let mut out = String::with_capacity(text.len());
    for part in parts {
        match part {
            Part::Text(literal) => out.push_str(&literal.replace("${", "$${")),
            Part::Expr(expression) => {
                let (name, rest) = expr::head(&expression);
                match reads.get(name) {
                    Some(outputs) => {
                        let read = rest.strip_prefix('.').unwrap_or("");
                        let is_word = !read.is_empty()
                            && read.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                        match outputs.get(read) {
                            Some(mapped) => out.push_str(mapped),
                            None if is_word && is_common_output(read) => {
                                return Err(format!(
                                    "${{{expression}}} reads {read:?} of {name:?}, which this \
                                     target cannot read from what a {} becomes here (it reads: {})",
                                    "common resource",
                                    outputs.keys().cloned().collect::<Vec<_>>().join(", ")
                                ))
                            }
                            None => out.push_str(&format!("${{{expression}}}")),
                        }
                    }
                    None => out.push_str(&format!("${{{expression}}}")),
                }
            }
        }
    }
    Ok(out)
}

/// Whether a word is an output some common type promises.
fn is_common_output(word: &str) -> bool {
    COMMON_OUTPUTS.iter().any(|(_, o, _)| *o == word)
}

/// A helper resource of an expansion: the same place in the document as the
/// common resource, under an id derived from it.
pub fn helper(
    common: &Resource,
    suffix: &str,
    r#type: &str,
    params: BTreeMap<String, Value>,
) -> Resource {
    Resource {
        id: format!("{}_{suffix}", common.id),
        r#type: r#type.to_owned(),
        state: String::new(),
        params,
        runtime_when: String::new(),
        adopt: String::new(),
        index: common.index,
        pos: common.pos.clone(),
    }
}

/// The native resource that stands for the common one: its id, its place,
/// and its `runtimeWhen`, which the target then judges as it would on any
/// resource.
pub fn primary(common: &Resource, r#type: &str, params: BTreeMap<String, Value>) -> Resource {
    Resource {
        id: common.id.clone(),
        r#type: r#type.to_owned(),
        state: String::new(),
        params,
        runtime_when: common.runtime_when.clone(),
        adopt: common.adopt.clone(),
        index: common.index,
        pos: common.pos.clone(),
    }
}

/// The name a common resource has in the cloud: its `name` param, or its id.
pub fn cloud_name(resource: &Resource) -> Value {
    resource
        .params
        .get("name")
        .cloned()
        .unwrap_or_else(|| Value::String(resource.id.clone()))
}

/// The name of an Azure storage account for an `objectStore`: its `name`,
/// or its id with only the lower-case letters and digits Azure allows.
pub fn storage_account_name(resource: &Resource) -> Value {
    match resource.params.get("name") {
        Some(name) => name.clone(),
        None => Value::String(
            resource
                .id
                .to_ascii_lowercase()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect(),
        ),
    }
}

/// Whether an `objectStore` keeps older versions of its files.
pub fn versioning(resource: &Resource) -> bool {
    matches!(resource.params.get("versioning"), Some(Value::Bool(true)))
}

/// Picks the smallest size of a cloud's table that has at least the asked
/// processors and memory: the table's rows are (size, cpus, memory in GB),
/// in the order to try. `None` when nothing is big enough.
pub fn closest_size(
    table: &[(&'static str, u32, f64)],
    cpus: u32,
    memory_gb: f64,
) -> Option<&'static str> {
    table
        .iter()
        .filter(|(_, c, m)| *c >= cpus && *m >= memory_gb)
        .min_by(|a, b| {
            (a.1, a.2)
                .partial_cmp(&(b.1, b.2))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(size, _, _)| *size)
}

/// The processors and memory a `size` written as a mapping asks for, once
/// [`check_common`] has accepted it; `None` for a size written as text.
pub fn asked_size(value: &Value) -> Option<(u32, f64)> {
    let map = value.as_map()?;
    let number = |key: &str| match map.get(key)? {
        Value::Int(i) => Some(*i as f64),
        Value::Uint(u) => Some(*u as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    };
    Some((number("cpus")? as u32, number("memoryGb")?))
}

/// The family and version an `image` written as a mapping asks for, once
/// [`check_common`] has accepted it; `None` for an image written as text.
pub fn asked_image(value: &Value) -> Option<(&str, &str)> {
    let map = value.as_map()?;
    Some((map.get("family")?.as_str()?, map.get("version")?.as_str()?))
}

/// The disks attached to `vm`, in document order, as their ids. A target
/// numbers a disk's attachment (its LUN, its device name) by its place here.
pub fn disks_of<'a>(graph: &'a ResourceGraph, vm: &str) -> Vec<&'a Resource> {
    graph
        .resources
        .iter()
        .filter(|r| {
            r.r#type == "disk" && r.params.get("vm").and_then(referenced_id).as_deref() == Some(vm)
        })
        .collect()
}

/// The size a `disk` asks for, in gigabytes. Only called after
/// [`check_common`] accepted the disk.
pub fn disk_size(resource: &Resource) -> u64 {
    match resource.params.get("sizeGb") {
        Some(Value::Int(i)) => (*i).max(0) as u64,
        Some(Value::Uint(u)) => *u,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;

    fn resource(r#type: &str, params: &[(&str, Value)]) -> Resource {
        Resource {
            id: "a".to_owned(),
            r#type: r#type.to_owned(),
            state: String::new(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            runtime_when: String::new(),
            adopt: String::new(),
            index: 0,
            pos: Position::default(),
        }
    }

    #[test]
    fn the_tables_are_sorted_and_every_param_belongs_to_a_type() {
        let names: Vec<&str> = COMMON_TYPES.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let keys: Vec<(&str, &str)> = COMMON_PARAMS
            .iter()
            .map(|(t, p, _, _, _)| (*t, *p))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            keys, sorted,
            "params are sorted by type then name, and unique"
        );
        for (t, _) in &keys {
            assert!(is_common_type(t), "{t}");
        }
        for (t, _, _) in COMMON_OUTPUTS {
            assert!(is_common_type(t), "{t}");
        }
    }

    #[test]
    fn a_param_outside_the_vocabulary_is_refused() {
        let r = resource(
            "network",
            &[
                ("cidr", Value::from("10.0.0.0/16")),
                ("dns", Value::Bool(true)),
            ],
        );
        let err = check_common(&r).unwrap_err();
        assert_eq!(err.rule, "unsupported-param");
        assert!(
            err.msg.contains("accepted: cidr, location, name, tags"),
            "{}",
            err.msg
        );
    }

    #[test]
    fn a_missing_required_param_is_refused() {
        let err = check_common(&resource("network", &[])).unwrap_err();
        assert_eq!(err.rule, "missing-param");
        assert_eq!(err.msg, "a network needs the param \"cidr\"");
    }

    #[test]
    fn a_reference_param_must_be_one_reference() {
        let r = resource(
            "subnet",
            &[
                ("cidr", Value::from("10.0.1.0/24")),
                ("network", Value::from("net")),
            ],
        );
        let err = check_common(&r).unwrap_err();
        assert_eq!(err.rule, "invalid-param");
        assert!(err.msg.contains("written ${id}"), "{}", err.msg);
        let r = resource(
            "subnet",
            &[
                ("cidr", Value::from("10.0.1.0/24")),
                ("network", Value::from("${net}")),
            ],
        );
        check_common(&r).unwrap();
    }

    #[test]
    fn a_size_and_an_image_are_a_mapping_or_text() {
        let base = |size: Value, image: Value| {
            resource(
                "vm",
                &[
                    ("subnet", Value::from("${s}")),
                    ("size", size),
                    ("image", image),
                ],
            )
        };
        let mapping = |pairs: &[(&str, Value)]| {
            Value::Map(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            )
        };
        check_common(&base(
            mapping(&[("cpus", Value::Int(2)), ("memoryGb", Value::Int(4))]),
            mapping(&[
                ("family", Value::from("ubuntu")),
                ("version", Value::from("24.04")),
            ]),
        ))
        .unwrap();
        check_common(&base(Value::from("t3.small"), Value::from("ami-123"))).unwrap();
        let err = check_common(&base(
            mapping(&[("cpus", Value::Int(2))]),
            Value::from("ami-123"),
        ))
        .unwrap_err();
        assert!(err.msg.contains("needs memoryGb"), "{}", err.msg);
        let err = check_common(&base(
            Value::from("t3.small"),
            mapping(&[
                ("family", Value::from("windows")),
                ("version", Value::from("11")),
            ]),
        ))
        .unwrap_err();
        assert!(err.msg.contains("accepted: debian, ubuntu"), "{}", err.msg);
        let err = check_common(&base(
            Value::from("t3.small"),
            mapping(&[
                ("family", Value::from("ubuntu")),
                ("version", Value::Float(24.04)),
            ]),
        ))
        .unwrap_err();
        assert!(err.msg.contains("quote the version"), "{}", err.msg);
    }

    #[test]
    fn firewall_rules_have_a_fixed_shape() {
        let rule = |pairs: &[(&str, Value)]| {
            Value::Map(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            )
        };
        let fw = |rules: Vec<Value>| {
            resource(
                "firewall",
                &[
                    ("network", Value::from("${net}")),
                    ("rules", Value::List(rules)),
                ],
            )
        };
        let good = fw(vec![
            rule(&[
                ("port", Value::from("80")),
                ("cidr", Value::from("0.0.0.0/0")),
            ]),
            rule(&[
                ("direction", Value::from("out")),
                ("protocol", Value::from("udp")),
                ("port", Value::Int(53)),
                ("cidr", Value::from("10.0.0.0/8")),
            ]),
        ]);
        check_common(&good).unwrap();
        let rules = firewall_rules(&good);
        assert_eq!(rules[0].ports, Some((80, 80)));
        assert_eq!(rules[0].direction, "in");
        assert_eq!(rules[1].ports, Some((53, 53)));
        assert_eq!(rules[1].protocol, "udp");
        for (bad, text) in [
            (
                rule(&[
                    ("cidr", Value::from("x")),
                    ("port", Value::from("9000-8000")),
                ]),
                "low to high",
            ),
            (rule(&[("port", Value::from("80"))]), "needs a cidr"),
            (
                rule(&[
                    ("cidr", Value::from("x")),
                    ("direction", Value::from("ingress")),
                ]),
                "in, out",
            ),
            (
                rule(&[
                    ("cidr", Value::from("x")),
                    ("protocol", Value::from("icmp")),
                    ("port", Value::from("8")),
                ]),
                "no port",
            ),
            (
                rule(&[("cidr", Value::from("x")), ("from", Value::from("a"))]),
                "not \"from\"",
            ),
        ] {
            let err = check_common(&fw(vec![bad])).unwrap_err();
            assert_eq!(err.rule, "invalid-param");
            assert!(err.msg.contains(text), "{}", err.msg);
        }
    }

    #[test]
    fn a_state_other_than_present_is_refused() {
        let mut r = resource("address", &[]);
        r.state = "absent".to_owned();
        assert_eq!(check_common(&r).unwrap_err().rule, "unsupported-state");
    }

    #[test]
    fn the_closest_size_is_the_smallest_that_fits() {
        let table = &[("small", 2, 2.0), ("medium", 2, 4.0), ("large", 4, 16.0)];
        assert_eq!(closest_size(table, 2, 4.0), Some("medium"));
        assert_eq!(closest_size(table, 1, 1.0), Some("small"));
        assert_eq!(closest_size(table, 3, 1.0), Some("large"));
        assert_eq!(closest_size(table, 8, 1.0), None);
    }

    #[test]
    fn a_read_of_a_common_output_is_rewritten_and_the_rest_is_left() {
        let mut reads = BTreeMap::new();
        reads.insert(
            "web".to_owned(),
            BTreeMap::from([("publicAddress".to_owned(), "${web_ip.public_ip}".to_owned())]),
        );
        assert_eq!(
            rewrite_text("http://${web.publicAddress}/ $${x}", &reads).unwrap(),
            "http://${web_ip.public_ip}/ $${x}"
        );
        assert_eq!(rewrite_text("${web.arn}", &reads).unwrap(), "${web.arn}");
        assert_eq!(rewrite_text("${web}", &reads).unwrap(), "${web}");
        assert_eq!(
            rewrite_text("${other.publicAddress}", &reads).unwrap(),
            "${other.publicAddress}"
        );
        let err = rewrite_text("${web.privateAddress}", &reads).unwrap_err();
        assert!(err.contains("cannot read"), "{err}");
    }

    #[test]
    fn every_type_has_a_schema_with_its_outputs_in_the_description() {
        for name in common_type_names() {
            let params: Vec<&str> = common_params(name).iter().map(|(p, _)| *p).collect();
            let schema = common_schema(name, &params);
            assert_eq!(schema.params.len(), params.len(), "{name}");
            assert!(schema.description.contains("${id.id}"), "{name}");
            assert!(!schema.open);
        }
    }
}
