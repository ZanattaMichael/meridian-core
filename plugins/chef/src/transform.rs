//! Stage one: each IR resource becomes one Chef resource block, or two.

use std::collections::BTreeMap;

use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, Block, Cookbook, NAME};

/// How one IR resource type becomes a Chef resource.
///
/// The table is explicit on purpose: an IR type with no entry is a hard
/// error, and a param with no entry is a hard error, so the target can never
/// silently drop something the author asked for.
pub(crate) struct Mapping {
    /// The Chef resource.
    pub chef: &'static str,
    /// Every accepted IR state and the Chef action it selects, sorted by
    /// state. Empty means the type takes no state.
    states: &'static [(&'static str, &'static str)],
    /// The IR state assumed when a resource declares none.
    default_state: &'static str,
    /// Every accepted IR param and Chef's property for it, sorted by param.
    /// An empty property steers the mapping and is not written as one.
    params: &'static [(&'static str, &'static str)],
    /// IR params that must be present.
    required: &'static [&'static str],
    /// The actions a notify may ask this type to take, sorted.
    pub actions: &'static [&'static str],
}

const CREATE_REMOVE: &[(&str, &str)] = &[("absent", "remove"), ("present", "create")];

pub(crate) fn mapping(resource_type: &str) -> Option<Mapping> {
    Some(match resource_type {
        "package" => Mapping {
            chef: "package",
            states: &[
                ("absent", "remove"),
                ("latest", "upgrade"),
                ("present", "install"),
            ],
            default_state: "present",
            params: &[("name", "package_name")],
            required: &["name"],
            actions: &[],
        },
        "service" => Mapping {
            chef: "service",
            states: &[
                ("running", "start"),
                ("started", "start"),
                ("stopped", "stop"),
            ],
            default_state: "running",
            // `enabled` is an action in Chef, not a property.
            params: &[("enabled", ""), ("name", "service_name")],
            required: &["name"],
            actions: &["reload", "restart", "start", "stop"],
        },
        "file" => Mapping {
            chef: "file",
            // A directory is a resource of its own in Chef.
            states: &[
                ("absent", "delete"),
                ("directory", "create"),
                ("file", "create"),
                ("present", "create"),
            ],
            default_state: "present",
            params: &[
                ("content", "content"),
                ("group", "group"),
                ("mode", "mode"),
                ("owner", "owner"),
                ("path", "path"),
            ],
            required: &["path"],
            actions: &[],
        },
        "user" => Mapping {
            chef: "user",
            states: CREATE_REMOVE,
            default_state: "present",
            // Chef's user has a primary group only; `groups` becomes a
            // membership block for each group.
            params: &[
                ("groups", ""),
                ("home", "home"),
                ("name", "username"),
                ("shell", "shell"),
                ("system", "system"),
                ("uid", "uid"),
            ],
            required: &["name"],
            actions: &[],
        },
        "group" => Mapping {
            chef: "group",
            states: CREATE_REMOVE,
            default_state: "present",
            params: &[("gid", "gid"), ("name", "group_name"), ("system", "system")],
            required: &["name"],
            actions: &[],
        },
        "exec" => Mapping {
            chef: "execute",
            states: &[],
            default_state: "",
            // Chef hands a command that holds shell syntax to the shell and
            // runs a plain one directly, so `shell` has nothing to select.
            params: &[
                ("chdir", "cwd"),
                ("command", "command"),
                ("creates", "creates"),
                ("shell", ""),
            ],
            required: &["command"],
            actions: &["run"],
        },
        _ => return None,
    })
}

const TYPES: [&str; 6] = ["exec", "file", "group", "package", "service", "user"];

/// What each resource type takes, read from the same table a resource is
/// checked against, so an editor is never told something `emit` refuses.
pub fn resource_schemas() -> Vec<meridian_sdk::ResourceSchema> {
    supported_resource_types()
        .iter()
        .filter_map(|name| {
            let m = mapping(name)?;
            let params: Vec<&str> = m.params.iter().map(|(param, _)| *param).collect();
            let states: Vec<&str> = m.states.iter().map(|(state, _)| *state).collect();
            Some(meridian_sdk::schema::shared(
                name,
                &params,
                m.required,
                &states,
                m.default_state,
                m.actions,
            ))
        })
        .collect()
}

/// Every IR resource type this target maps, sorted.
pub fn supported_resource_types() -> Vec<String> {
    TYPES.map(str::to_owned).to_vec()
}

fn keys(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
}

fn lookup(pairs: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Maps the target-agnostic tree onto Chef resource blocks, in the tree's
/// apply order. Notifications are the sort stage's job.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Cookbook, CompileError> {
    let mut blocks = Vec::with_capacity(graph.resources.len());
    for resource in &graph.resources {
        blocks.extend(blocks_for(resource)?);
    }
    Ok(Cookbook {
        name: cookbook_name(&graph.name)?,
        document: graph.name.clone(),
        blocks,
    })
}

fn blocks_for(r: &Resource) -> Result<Vec<Block>, CompileError> {
    let Some(m) = mapping(&r.r#type) else {
        return Err(refuse(
            r,
            "unsupported-resource-type",
            format!(
                "unsupported resource type {:?} on this target (supported: {})",
                r.r#type,
                TYPES.join(", ")
            ),
        ));
    };

    let mut properties = BTreeMap::new();
    for (name, value) in &r.params {
        let Some(native) = lookup(m.params, name) else {
            return Err(refuse(
                r,
                "unsupported-param",
                format!(
                    "resource type {:?} does not accept param {:?} on this target (accepted: {})",
                    r.r#type,
                    name,
                    keys(m.params)
                ),
            ));
        };
        if *value == Value::Null {
            return Err(refuse(
                r,
                "empty-param",
                format!(
                    "param {:?} on resource type {:?} has no value",
                    name, r.r#type
                ),
            ));
        }
        if !native.is_empty() {
            properties.insert(native.to_owned(), value.clone());
        }
    }
    for need in m.required {
        if !r.params.contains_key(*need) {
            return Err(refuse(
                r,
                "missing-param",
                format!(
                    "resource type {:?} requires param {:?} on this target",
                    r.r#type, need
                ),
            ));
        }
    }

    let state = if r.state.is_empty() {
        m.default_state
    } else {
        &r.state
    };
    let mut actions = Vec::new();
    if m.states.is_empty() {
        if !r.state.is_empty() {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} accepts no state on this target, got {:?}",
                    r.r#type, r.state
                ),
            ));
        }
        actions.push("run");
    } else {
        let Some(action) = lookup(m.states, state) else {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} does not accept state {:?} on this target (accepted: {})",
                    r.r#type,
                    state,
                    keys(m.states)
                ),
            ));
        };
        actions.push(action);
    }

    let mut chef = m.chef;
    match r.r#type.as_str() {
        "service" => match r.params.get("enabled") {
            None => {}
            Some(Value::Bool(true)) => actions.insert(0, "enable"),
            Some(Value::Bool(false)) => actions.insert(0, "disable"),
            Some(_) => {
                return Err(refuse(
                    r,
                    "invalid-param",
                    "param \"enabled\" must be true or false".to_owned(),
                ))
            }
        },
        "file" if state == "directory" => {
            if r.params.contains_key("content") {
                return Err(refuse(
                    r,
                    "invalid-param",
                    "a directory has no content; remove param \"content\" or the state".to_owned(),
                ));
            }
            chef = "directory";
        }
        // A document declares a group, not its whole membership. Without
        // `append`, Chef takes the missing member list for an empty one and
        // removes every member each time it converges, including the users
        // this document puts in the group.
        "group" if state == "present" => {
            properties.insert("append".to_owned(), Value::Bool(true));
        }
        _ => {}
    }

    let block = Block {
        resource: chef,
        name: r.id.clone(),
        properties,
        actions,
        only_if: r.runtime_when.clone(),
        notifies: Vec::new(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    };

    // Supplementary groups: one block a group, each adding the user to a
    // group that already exists and leaving its other members alone.
    let mut memberships = Vec::new();
    if let (Some(groups), "present") = (r.params.get("groups"), state) {
        let user = r.params.get("name").cloned().unwrap_or(Value::Null);
        let invalid = || {
            refuse(
                r,
                "invalid-param",
                "param \"groups\" must be a list of group names".to_owned(),
            )
        };
        for group in groups.as_list().ok_or_else(invalid)? {
            let name = group.as_str().ok_or_else(invalid)?;
            memberships.push(Block {
                resource: "group",
                name: format!("{}:{name}", r.id),
                properties: BTreeMap::from([
                    ("append".to_owned(), Value::Bool(true)),
                    ("group_name".to_owned(), group.clone()),
                    ("members".to_owned(), Value::List(vec![user.clone()])),
                ]),
                actions: vec!["modify"],
                only_if: r.runtime_when.clone(),
                ..block.clone()
            });
        }
    }

    let mut blocks = vec![block];
    blocks.extend(memberships);
    Ok(blocks)
}

/// Derives the cookbook's name from the document name. A cookbook name holds
/// lower-case letters, digits, hyphens and underscores, so a document name
/// that cannot become one is refused instead of silently rewritten.
pub(crate) fn cookbook_name(document: &str) -> Result<String, CompileError> {
    if let Some(c) = document
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'))
    {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: "invalid-cookbook-name".to_owned(),
            msg: format!(
                "document name {document:?} cannot become a Chef cookbook name: {:?} is not \
                 allowed, and a cookbook name may hold only lower-case letters, digits, hyphens \
                 and underscores",
                c.to_string()
            ),
            ..CompileError::default()
        });
    }
    Ok(format!("meridian_{document}"))
}
