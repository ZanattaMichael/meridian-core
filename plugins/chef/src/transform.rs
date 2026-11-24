//! Stage one: each IR resource becomes one Chef resource block, or two.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, ResourceSchema, TypeName, Value};

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
                ("link", "create"),
                ("present", "create"),
            ],
            default_state: "present",
            params: &[
                ("checksum", "checksum"),
                ("content", "content"),
                ("group", "group"),
                ("mode", "mode"),
                ("owner", "owner"),
                ("path", "path"),
                ("source", "source"),
                ("target", "to"),
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

/// The shared types, sorted.
const SHARED: &[&str] = &[
    "cron",
    "exec",
    "file",
    "group",
    "package",
    "packageRepository",
    "service",
    "user",
];

/// How an editor is told that any Chef resource is a type: its name, as a
/// recipe writes it.
pub const NATIVE_PATTERN: &str = "<resource>";

/// What each resource type takes, read from the same table a resource is
/// checked against, so an editor is never told something `emit` refuses.
pub fn resource_schemas() -> Vec<ResourceSchema> {
    let mut schemas = vec![ResourceSchema::open(
        NATIVE_PATTERN,
        "Any Chef resource, by the name a recipe writes: template, cron, apt_repository. \
         The params are the resource's properties, passed through as written; a state is \
         the action it takes. When notified it takes the action named. A resource whose \
         name is a shared type is written native/<name>: native/package is Chef's own \
         package. The document then compiles for Chef only.",
    )];
    schemas.extend(SHARED.iter().filter_map(|name| {
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
    }));
    meridian_sdk::schema::insert_repository_schema(&mut schemas);
    meridian_sdk::schema::insert_cron_schema(&mut schemas);
    schemas
}

/// Every IR resource type this target maps: the pattern every Chef resource
/// fits, then the shared six, sorted.
pub fn supported_resource_types() -> Vec<String> {
    let mut types = vec![NATIVE_PATTERN.to_owned()];
    types.extend(SHARED.iter().map(|t| (*t).to_owned()));
    types
}

/// The Chef resource a native type names, when the type is one: the bare
/// name under `native/`, which Chef has for five of the six (its command is
/// `execute`, which has no shared name to collide with), or the name as
/// written, which is a resource name when it is one lower-case word.
pub(crate) fn native_resource(r: &Resource) -> Result<Option<&str>, CompileError> {
    match TypeName::read(&r.r#type) {
        TypeName::Shared(_) => Ok(None),
        TypeName::NativeNamedLikeShared("exec") => Err(refuse(
            r,
            &rules::UNSUPPORTED_RESOURCE_TYPE,
            format!(
                "resource type {:?} names no Chef resource: Chef runs a command with execute, \
                 which is written as it is",
                r.r#type
            ),
        )),
        TypeName::NativeNamedLikeShared(bare) if meridian_sdk::schema::is_shared_type(bare) => {
            Ok(Some(bare))
        }
        // The prefix is for the names a shared type has taken; any other
        // resource has one spelling.
        TypeName::NativeNamedLikeShared(_) => Err(refuse(
            r,
            &rules::UNSUPPORTED_RESOURCE_TYPE,
            format!(
                "resource type {:?} carries the native/ prefix, which reaches only a Chef \
                 resource whose name is a shared type ({}); every other resource is written \
                 as it is",
                r.r#type,
                SHARED.join(", ")
            ),
        )),
        TypeName::Native(name) if is_word(name) => Ok(Some(name)),
        TypeName::Native(_) => Err(unsupported_type(r)),
    }
}

/// Whether `name` is shaped like a Chef resource or action name: one
/// lower-case word, as Ruby writes a method.
fn is_word(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn unsupported_type(r: &Resource) -> CompileError {
    refuse(
        r,
        &rules::UNSUPPORTED_RESOURCE_TYPE,
        format!(
            "unsupported resource type {:?} on this target: it is not one of the shared types \
             ({}), and a Chef resource is written as a recipe names it, such as template or \
             apt_repository",
            r.r#type,
            SHARED.join(", ")
        ),
    )
}

/// A resource whose type is Chef's own: the params are the resource's
/// properties as written, and a state is the action it takes. Nothing is
/// checked against a list of what the resource takes; a property the
/// resource does not have is Chef's to refuse when the recipe is compiled,
/// until the resource's schema is synced and the editor reports it in
/// place.
fn native_block(r: &Resource, resource: &str) -> Result<Block, CompileError> {
    let mut properties = BTreeMap::new();
    for (name, value) in &r.params {
        if *value == Value::Null {
            return Err(refuse(
                r,
                &rules::EMPTY_PARAM,
                format!("param {name:?} on resource {resource} has no value"),
            ));
        }
        properties.insert(name.clone(), value.clone());
    }
    let mut actions = Vec::new();
    if !r.state.is_empty() {
        if r.params.contains_key("action") {
            return Err(refuse(
                r,
                &rules::PARAM_CONFLICTS_WITH_STATE,
                format!(
                    "state {:?} and param \"action\" both set the resource's action; write one \
                     of them",
                    r.state
                ),
            ));
        }
        if !is_word(&r.state) {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
                format!(
                    "state {:?} is not a Chef action: on resource {resource} the state is the \
                     action the resource takes, one lower-case word such as create or upgrade",
                    r.state
                ),
            ));
        }
        actions.push(r.state.clone());
    }
    Ok(Block {
        resource: resource.to_owned(),
        name: r.id.clone(),
        properties,
        actions,
        only_if: r.runtime_when.clone(),
        notifies: Vec::new(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// A `cron` job: Chef's `cron`.
fn cron_block(r: &Resource) -> Result<Block, CompileError> {
    let job = meridian_sdk::schema::cron_job(r).map_err(|e| CompileError {
        target: NAME.to_owned(),
        ..e
    })?;
    let text = |s: &str| Value::String(s.to_owned());
    let properties = BTreeMap::from([
        ("command".to_owned(), text(&job.command)),
        ("user".to_owned(), text(&job.user)),
        ("minute".to_owned(), text(&job.minute)),
        ("hour".to_owned(), text(&job.hour)),
        ("day".to_owned(), text(&job.day)),
        ("month".to_owned(), text(&job.month)),
        ("weekday".to_owned(), text(&job.weekday)),
    ]);
    Ok(Block {
        resource: "cron".to_owned(),
        name: job.name,
        properties,
        actions: vec![if job.present { "create" } else { "delete" }.to_owned()],
        only_if: r.runtime_when.clone(),
        notifies: Vec::new(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// A `packageRepository`: `apt_repository` or `yum_repository`.
fn repository_block(r: &Resource) -> Result<Block, CompileError> {
    use meridian_sdk::schema::RepositoryFormat;
    let repo = meridian_sdk::schema::package_repository(r).map_err(|e| CompileError {
        target: NAME.to_owned(),
        ..e
    })?;
    let text = |s: &str| Value::String(s.to_owned());
    let mut properties = BTreeMap::new();
    let (resource, action) = match &repo.format {
        RepositoryFormat::Apt {
            distribution,
            components,
        } => {
            properties.insert("repo_name".to_owned(), text(&repo.name));
            properties.insert("uri".to_owned(), text(&repo.url));
            properties.insert("distribution".to_owned(), text(distribution));
            properties.insert(
                "components".to_owned(),
                Value::List(components.iter().map(|c| text(c)).collect()),
            );
            if let Some(key) = &repo.key {
                properties.insert("key".to_owned(), Value::List(vec![text(key)]));
            }
            (
                "apt_repository",
                if repo.present { "add" } else { "remove" },
            )
        }
        RepositoryFormat::Yum => {
            properties.insert("repositoryid".to_owned(), text(&repo.name));
            properties.insert("description".to_owned(), text(&repo.name));
            properties.insert("baseurl".to_owned(), text(&repo.url));
            properties.insert("gpgcheck".to_owned(), Value::Bool(repo.key.is_some()));
            if let Some(key) = &repo.key {
                properties.insert("gpgkey".to_owned(), text(key));
            }
            (
                "yum_repository",
                if repo.present { "create" } else { "remove" },
            )
        }
    };
    Ok(Block {
        resource: resource.to_owned(),
        name: r.id.clone(),
        properties,
        actions: vec![action.to_owned()],
        only_if: r.runtime_when.clone(),
        notifies: Vec::new(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
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
        // The shared types are checked the same way on every configuration
        // management target, so a document moves between tools without the
        // type rules changing under it.
        meridian_sdk::schema::check_shared(resource).map_err(|e| CompileError {
            target: NAME.to_owned(),
            ..e
        })?;
        blocks.extend(blocks_for(resource)?);
    }
    Ok(Cookbook {
        name: cookbook_name(&graph.name)?,
        document: graph.name.clone(),
        blocks,
        secrets: graph.secrets.clone(),
    })
}

fn blocks_for(r: &Resource) -> Result<Vec<Block>, CompileError> {
    if let Some(resource) = native_resource(r)? {
        return Ok(vec![native_block(r, resource)?]);
    }
    if r.r#type == "packageRepository" {
        return Ok(vec![repository_block(r)?]);
    }
    if r.r#type == "cron" {
        return Ok(vec![cron_block(r)?]);
    }
    let m = mapping(&r.r#type).ok_or_else(|| unsupported_type(r))?;

    let mut properties = BTreeMap::new();
    for (name, value) in &r.params {
        let Some(native) = lookup(m.params, name) else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_PARAM,
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
                &rules::EMPTY_PARAM,
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
                &rules::MISSING_PARAM,
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
                &rules::UNSUPPORTED_STATE,
                format!(
                    "resource type {:?} accepts no state on this target, got {:?}",
                    r.r#type, r.state
                ),
            ));
        }
        actions.push("run".to_owned());
    } else {
        let Some(action) = lookup(m.states, state) else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
                format!(
                    "resource type {:?} does not accept state {:?} on this target (accepted: {})",
                    r.r#type,
                    state,
                    keys(m.states)
                ),
            ));
        };
        actions.push(action.to_owned());
    }

    let mut chef = m.chef;
    match r.r#type.as_str() {
        "service" => match r.params.get("enabled") {
            None => {}
            Some(Value::Bool(true)) => actions.insert(0, "enable".to_owned()),
            Some(Value::Bool(false)) => actions.insert(0, "disable".to_owned()),
            Some(_) => {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    "param \"enabled\" must be true or false".to_owned(),
                ))
            }
        },
        "file" if state == "directory" => {
            if r.params.contains_key("content") {
                return Err(refuse(
                    r,
                    &rules::PARAM_CONFLICTS_WITH_STATE,
                    "a directory has no content; remove param \"content\" or the state".to_owned(),
                ));
            }
            chef = "directory";
        }
        // A link is a resource of its own in Chef, named by the link's path.
        "file" if state == "link" => {
            if let Some(path) = properties.remove("path") {
                properties.insert("target_file".to_owned(), path);
            }
            chef = "link";
        }
        // A download is a remote_file; Chef takes the bare sha256 digest.
        "file" if r.params.contains_key("source") => {
            if let Some(Value::String(sum)) = properties.get("checksum") {
                if let Some(hex) = meridian_sdk::schema::file_checksum_hex(sum) {
                    properties.insert("checksum".to_owned(), Value::String(hex));
                }
            }
            chef = "remote_file";
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
        resource: chef.to_owned(),
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
                &rules::INVALID_PARAM,
                "param \"groups\" must be a list of group names".to_owned(),
            )
        };
        for group in groups.as_list().ok_or_else(invalid)? {
            let name = group.as_str().ok_or_else(invalid)?;
            memberships.push(Block {
                resource: "group".to_owned(),
                name: format!("{}:{name}", r.id),
                properties: BTreeMap::from([
                    ("append".to_owned(), Value::Bool(true)),
                    ("group_name".to_owned(), group.clone()),
                    ("members".to_owned(), Value::List(vec![user.clone()])),
                ]),
                actions: vec!["modify".to_owned()],
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
            rule: rules::INVALID_NAME.name.to_owned(),
            hint: rules::INVALID_NAME.hint.to_owned(),
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
