//! Stage one: each IR resource becomes an entry in one of cloud-init's
//! modules, or, where cloud-init has no module for it, a first-boot command.
//!
//! Nothing is dropped. A type, state or param with no mapping is a refusal
//! that names what would have worked.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{CompileError, EdgeKind, Resource, ResourceGraph, Value, Warning};

use crate::shell::{quote, split};
use crate::{refuse, warning, Command, Group, Item, Package, Placed, User, WriteFile};

/// What the transform stage hands on.
pub(crate) struct Transformed {
    /// One entry per resource, in the tree's apply order.
    pub units: Vec<Placed<Item>>,
    /// The commands the notify edges amount to, in the order they run.
    pub handlers: Vec<Command>,
    pub warnings: Vec<Warning>,
}

/// Every IR resource type this target maps, sorted.
pub fn supported_resource_types() -> Vec<String> {
    ["exec", "file", "group", "package", "service", "user"]
        .map(str::to_owned)
        .to_vec()
}

/// What each resource type takes, for editors. The lists repeat what each
/// type's function accepts below; the tooling crate's schema conformance
/// test fails if the two ever disagree.
pub fn resource_schemas() -> Vec<meridian_sdk::ResourceSchema> {
    use meridian_sdk::schema::shared;
    vec![
        shared(
            "exec",
            &["chdir", "command", "creates", "removes", "shell"],
            &["command"],
            &[],
            "",
            &["run"],
        ),
        shared(
            "file",
            &[
                "append", "content", "encoding", "group", "mode", "owner", "path",
            ],
            &["path"],
            &["absent", "directory", "file", "present"],
            "present",
            &[],
        ),
        shared(
            "group",
            &["members", "name"],
            &["name"],
            &["present"],
            "present",
            &[],
        ),
        shared(
            "package",
            &["name", "version"],
            &["name"],
            &["present"],
            "present",
            &[],
        ),
        shared(
            "service",
            &["enabled", "name"],
            &["name"],
            &["reloaded", "restarted", "running", "started", "stopped"],
            "running",
            SERVICE_ACTIONS,
        ),
        shared(
            "user",
            &[
                "gecos",
                "groups",
                "home",
                "lock_passwd",
                "name",
                "primary_group",
                "shell",
                "ssh_authorized_keys",
                "sudo",
                "system",
                "uid",
            ],
            &["name"],
            &["present"],
            "present",
            &[],
        ),
    ]
}

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Transformed, CompileError> {
    let mut units = Vec::with_capacity(graph.resources.len());
    let mut services = Vec::new();
    for resource in &graph.resources {
        // cloud-init reads the whole document once and evaluates nothing per
        // entry. Compiling the condition away would apply the resource on
        // every machine the condition was meant to exclude.
        if !resource.runtime_when.is_empty() {
            return Err(refuse(
                resource,
                "unsupported-runtime-condition",
                format!(
                    "resource carries runtimeWhen {:?}, which this target cannot express: \
                     cloud-init applies user-data as written and has no per-resource \
                     condition to compile it into. Put the test inside an exec resource's \
                     command if only a command depends on it",
                    resource.runtime_when
                ),
            ));
        }
        if resource.r#type == "service" {
            services.push(format!("{:?}", resource.id));
        }
        units.push(Placed {
            id: resource.id.clone(),
            pos: resource.pos.clone(),
            item: item_for(resource)?,
        });
    }

    let handlers = handlers(graph)?;
    let mut warnings = Vec::new();
    if !services.is_empty() {
        warnings.push(warning(
            "",
            format!(
                "cloud-init has no module for services, so {} {} compiled to systemctl \
                 commands that run at the end of the first boot; the image must use systemd",
                if services.len() == 1 {
                    "resource"
                } else {
                    "resources"
                },
                services.join(", ")
            ),
        ));
    }
    if !handlers.is_empty() {
        warnings.push(warning(
            "",
            "cloud-init applies user-data once and never sees a later change, so notify is \
             emulated: every notified action runs once, after all other first-boot commands, \
             whether or not the notifying resource changed anything"
                .to_owned(),
        ));
    }
    Ok(Transformed {
        units,
        handlers,
        warnings,
    })
}

fn item_for(r: &Resource) -> Result<Item, CompileError> {
    match r.r#type.as_str() {
        "package" => package(r),
        "file" => file(r),
        "user" => user(r),
        "group" => group(r),
        "service" => service(r),
        "exec" => {
            accept(r, &["chdir", "command", "creates", "removes", "shell"])?;
            no_state(r)?;
            Ok(Item::Commands(vec![exec_command(r)?]))
        }
        _ => Err(refuse(
            r,
            "unsupported-resource-type",
            format!(
                "unsupported resource type {:?} on this target (supported: {})",
                r.r#type,
                supported_resource_types().join(", ")
            ),
        )),
    }
}

fn package(r: &Resource) -> Result<Item, CompileError> {
    accept(r, &["name", "version"])?;
    state(
        r,
        &["present"],
        "present",
        "cloud-init installs packages on a first boot; it cannot remove one or follow the latest",
    )?;
    Ok(Item::Package(Package {
        name: required(r, "name")?.to_owned(),
        version: text(r, "version")?.unwrap_or_default().to_owned(),
    }))
}

fn file(r: &Resource) -> Result<Item, CompileError> {
    accept(
        r,
        &[
            "append", "content", "encoding", "group", "mode", "owner", "path",
        ],
    )?;
    let state = state(
        r,
        &["absent", "directory", "file", "present"],
        "present",
        "",
    )?;
    let path = required(r, "path")?;
    if !path.starts_with('/') {
        return Err(refuse(
            r,
            "relative-path",
            format!(
                "path {path:?} is not absolute; a first boot has no working directory to \
                 resolve it against"
            ),
        ));
    }
    let owner = text(r, "owner")?.unwrap_or_default();
    let group = text(r, "group")?.unwrap_or_default();
    let mode = text(r, "mode")?.unwrap_or_default();

    // Only a file with content has a module of its own. The other two states
    // are one command each, and the params that describe content mean nothing
    // to them.
    let only_content = |state: &str| {
        for name in ["append", "content", "encoding"] {
            if r.params.contains_key(name) {
                return Err(refuse(
                    r,
                    "unsupported-param",
                    format!("param {name:?} does not apply to a file in state {state:?}"),
                ));
            }
        }
        Ok(())
    };
    match state {
        "directory" => {
            only_content(state)?;
            let mut argv = vec!["install", "-d"];
            for (flag, value) in [("-m", mode), ("-o", owner), ("-g", group)] {
                if !value.is_empty() {
                    argv.extend([flag, value]);
                }
            }
            argv.push(path);
            Ok(Item::Commands(vec![line(&argv)]))
        }
        "absent" => {
            only_content(state)?;
            for name in ["group", "mode", "owner"] {
                if r.params.contains_key(name) {
                    return Err(refuse(
                        r,
                        "unsupported-param",
                        format!("param {name:?} does not apply to a file in state \"absent\""),
                    ));
                }
            }
            if path.split('/').all(|part| part.is_empty() || part == ".") {
                return Err(refuse(
                    r,
                    "unsafe-path",
                    format!("refusing to compile the removal of {path:?}"),
                ));
            }
            Ok(Item::Commands(vec![line(&["rm", "-rf", "--", path])]))
        }
        _ => {
            let Some(content) = text(r, "content")? else {
                return Err(refuse(
                    r,
                    "missing-param",
                    format!(
                        "resource type \"file\" requires param \"content\" on this target: \
                         cloud-init writes whole files, and without content it would empty {path}"
                    ),
                ));
            };
            Ok(Item::File(WriteFile {
                path: path.to_owned(),
                content: content.to_owned(),
                owner: owner.to_owned(),
                group: group.to_owned(),
                permissions: mode.to_owned(),
                encoding: text(r, "encoding")?.unwrap_or_default().to_owned(),
                append: flag(r, "append")?.unwrap_or(false),
                defer: false,
            }))
        }
    }
}

fn user(r: &Resource) -> Result<Item, CompileError> {
    // Each IR param and cloud-init's own name for it.
    const PARAMS: [(&str, &str); 11] = [
        ("gecos", "gecos"),
        ("groups", "groups"),
        ("home", "homedir"),
        ("lock_passwd", "lock_passwd"),
        ("name", ""),
        ("primary_group", "primary_group"),
        ("shell", "shell"),
        ("ssh_authorized_keys", "ssh_authorized_keys"),
        ("sudo", "sudo"),
        ("system", "system"),
        ("uid", "uid"),
    ];
    accept(r, &PARAMS.map(|(name, _)| name))?;
    state(
        r,
        &["present"],
        "present",
        "cloud-init creates users on a first boot and cannot remove one; \
         use an exec resource to remove an account the image ships with",
    )?;
    let name = required(r, "name")?.to_owned();

    let mut fields = BTreeMap::new();
    for (param, native) in PARAMS {
        if native.is_empty() || !r.params.contains_key(param) {
            continue;
        }
        let value = match param {
            "groups" | "ssh_authorized_keys" => Value::List(
                texts(r, param)?
                    .into_iter()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            ),
            "lock_passwd" | "system" => Value::Bool(flag(r, param)?.unwrap_or(false)),
            "uid" => match &r.params[param] {
                value @ (Value::Int(_) | Value::Uint(_)) => value.clone(),
                other => return Err(wrong_shape(r, param, "a whole number", other)),
            },
            // One rule or several.
            "sudo" => match &r.params[param] {
                Value::List(_) => Value::List(
                    texts(r, param)?
                        .into_iter()
                        .map(Value::String)
                        .collect::<Vec<_>>(),
                ),
                _ => Value::String(text(r, param)?.unwrap_or_default().to_owned()),
            },
            _ => Value::String(text(r, param)?.unwrap_or_default().to_owned()),
        };
        fields.insert(native.to_owned(), value);
    }
    Ok(Item::User(User { name, fields }))
}

fn group(r: &Resource) -> Result<Item, CompileError> {
    accept(r, &["members", "name"])?;
    state(
        r,
        &["present"],
        "present",
        "cloud-init creates groups on a first boot and cannot remove one",
    )?;
    Ok(Item::Group(Group {
        name: required(r, "name")?.to_owned(),
        members: if r.params.contains_key("members") {
            texts(r, "members")?
        } else {
            Vec::new()
        },
    }))
}

const SERVICE_ACTIONS: &[&str] = &["reload", "restart", "start", "stop"];

fn service(r: &Resource) -> Result<Item, CompileError> {
    accept(r, &["enabled", "name"])?;
    let state = state(
        r,
        &["reloaded", "restarted", "running", "started", "stopped"],
        "running",
        "",
    )?;
    let name = required(r, "name")?;
    let enabled = flag(r, "enabled")?;
    let systemctl = |words: &[&str]| {
        let mut argv = vec!["systemctl"];
        argv.extend(words);
        argv.push(name);
        line(&argv)
    };

    let commands = match (state, enabled) {
        // Enabling and starting, or disabling and stopping, is one command.
        ("running" | "started", Some(true)) => vec![systemctl(&["enable", "--now"])],
        ("stopped", Some(false)) => vec![systemctl(&["disable", "--now"])],
        _ => {
            let mut commands = Vec::new();
            match enabled {
                Some(true) => commands.push(systemctl(&["enable"])),
                Some(false) => commands.push(systemctl(&["disable"])),
                None => {}
            }
            commands.push(systemctl(&[match state {
                "stopped" => "stop",
                "restarted" => "restart",
                "reloaded" => "reload",
                _ => "start",
            }]));
            commands
        }
    };
    Ok(Item::Commands(commands))
}

/// The words of a command as the line of shell that runs exactly them.
fn line<S: AsRef<str>>(words: &[S]) -> Command {
    words
        .iter()
        .map(|word| quote(word.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The one entry an exec resource is, with its guards compiled in.
fn exec_command(r: &Resource) -> Result<Command, CompileError> {
    let command = required(r, "command")?;
    let shell = flag(r, "shell")?.unwrap_or(false);
    let chdir = text(r, "chdir")?;

    let mut conditions = Vec::new();
    if let Some(path) = text(r, "creates")? {
        conditions.push(format!("[ ! -e {} ]", quote(path)));
    }
    if let Some(path) = text(r, "removes")? {
        conditions.push(format!("[ -e {} ]", quote(path)));
    }
    let conditions = conditions.join(" && ");

    // Every entry under runcmd ends up in one script, so a directory change
    // is kept in a subshell or it would outlive the command.
    if shell {
        // The author's own line of shell. The pieces around it are joined by
        // newlines because it may end in a comment, which would swallow
        // anything put after it on its line.
        let mut body = command.trim_end().to_owned();
        if let Some(dir) = chdir {
            body = format!("(\ncd {} || exit\n{body}\n)", quote(dir));
        }
        if !conditions.is_empty() {
            body = format!("if {conditions}; then\n{body}\nfi");
        }
        return Ok(body);
    }

    // No shell was asked for, so the command is split into words here and
    // each is quoted: the script runs those words and expands nothing.
    let argv = split(command).map_err(|why| {
        refuse(
            r,
            "invalid-command",
            format!(
                "command {command:?} cannot be split into words: {why}. \
                 Set shell: true if it is meant as a line of shell"
            ),
        )
    })?;
    if argv.is_empty() {
        return Err(refuse(r, "empty-param", empty("command", r)));
    }
    let mut body = line(&argv);
    if let Some(dir) = chdir {
        body = format!("( cd {} && {body} )", quote(dir));
    }
    if !conditions.is_empty() {
        body = format!("if {conditions}; then {body}; fi");
    }
    Ok(body)
}

/// The name a notify edge is deduplicated by: two notifiers of the same
/// action on the same resource run it once.
fn handler_name(action: &str, resource: &str) -> String {
    format!("{action} {resource}")
}

/// The commands the document's notify edges amount to, each once, in name
/// order so the same document always compiles to the same bytes.
fn handlers(graph: &ResourceGraph) -> Result<Vec<Command>, CompileError> {
    let mut seen = BTreeSet::new();
    let mut named = Vec::new();
    for edge in graph.edges_of_kind(EdgeKind::Notify) {
        let Some(to) = graph.resource(&edge.to) else {
            // The core already rejects an edge to an unknown resource; this
            // guards against a tree assembled some other way.
            let from = graph.resource(&edge.from).cloned().unwrap_or_default();
            return Err(refuse(
                &from,
                "unknown-notify-target",
                format!("notifies an unknown resource {:?}", edge.to),
            ));
        };
        let name = handler_name(&edge.action, &edge.to);
        if seen.insert(name.clone()) {
            named.push((name, handler_for(to, &edge.action)?));
        }
    }
    named.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(named.into_iter().map(|(_, command)| command).collect())
}

/// The command that performs one action on one resource.
fn handler_for(r: &Resource, action: &str) -> Result<Command, CompileError> {
    let unsupported = |supported: &[&str]| {
        refuse(
            r,
            "unsupported-action",
            format!(
                "resource type {:?} does not support notify action {:?} on this target (supported: {})",
                r.r#type,
                action,
                supported.join(", ")
            ),
        )
    };
    match r.r#type.as_str() {
        "service" => {
            if !SERVICE_ACTIONS.contains(&action) {
                return Err(unsupported(SERVICE_ACTIONS));
            }
            Ok(line(&["systemctl", action, required(r, "name")?]))
        }
        "exec" => {
            if action != "run" {
                return Err(unsupported(&["run"]));
            }
            exec_command(r)
        }
        _ => Err(refuse(
            r,
            "unsupported-action",
            format!(
                "resource type {:?} cannot be notified on this target, got action {:?}",
                r.r#type, action
            ),
        )),
    }
}

/// Refuses any param outside `accepted`, and any param with no value.
fn accept(r: &Resource, accepted: &[&str]) -> Result<(), CompileError> {
    for (name, value) in &r.params {
        if !accepted.contains(&name.as_str()) {
            return Err(refuse(
                r,
                "unsupported-param",
                format!(
                    "resource type {:?} does not accept param {:?} on this target (accepted: {})",
                    r.r#type,
                    name,
                    accepted.join(", ")
                ),
            ));
        }
        if *value == Value::Null {
            return Err(refuse(r, "empty-param", empty(name, r)));
        }
    }
    Ok(())
}

fn empty(name: &str, r: &Resource) -> String {
    format!(
        "param {:?} on resource type {:?} has no value",
        name, r.r#type
    )
}

fn wrong_shape(r: &Resource, name: &str, wanted: &str, got: &Value) -> CompileError {
    let hint = match got {
        Value::Int(_) | Value::Uint(_) | Value::Float(_) | Value::Bool(_)
            if wanted == "a string" =>
        {
            "; quote it if it is meant as text"
        }
        _ => "",
    };
    refuse(
        r,
        "invalid-param",
        format!(
            "param {:?} on resource type {:?} must be {wanted}, got {}{hint}",
            name,
            r.r#type,
            got.describe()
        ),
    )
}

/// A string param, if the resource has it.
fn text<'a>(r: &'a Resource, name: &str) -> Result<Option<&'a str>, CompileError> {
    match r.params.get(name) {
        None => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Err(refuse(r, "empty-param", empty(name, r))),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(wrong_shape(r, name, "a string", other)),
    }
}

/// A string param the resource must have.
fn required<'a>(r: &'a Resource, name: &str) -> Result<&'a str, CompileError> {
    text(r, name)?.ok_or_else(|| {
        refuse(
            r,
            "missing-param",
            format!(
                "resource type {:?} requires param {:?} on this target",
                r.r#type, name
            ),
        )
    })
}

fn flag(r: &Resource, name: &str) -> Result<Option<bool>, CompileError> {
    match r.params.get(name) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(wrong_shape(r, name, "true or false", other)),
    }
}

/// A param that is one string or a list of them.
fn texts(r: &Resource, name: &str) -> Result<Vec<String>, CompileError> {
    let wrong = |got: &Value| wrong_shape(r, name, "a string or a list of strings", got);
    match r.params.get(name) {
        None => Ok(Vec::new()),
        Some(Value::String(s)) if !s.is_empty() => Ok(vec![s.clone()]),
        Some(Value::List(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) if !s.is_empty() => Ok(s.clone()),
                other => Err(wrong(other)),
            })
            .collect(),
        Some(other) => Err(wrong(other)),
    }
}

/// The resource's state, checked against what the type accepts here. `why`
/// explains a short list to an author who expected more of it.
fn state<'a>(
    r: &'a Resource,
    accepted: &[&'static str],
    default: &'static str,
    why: &str,
) -> Result<&'a str, CompileError> {
    if r.state.is_empty() {
        return Ok(default);
    }
    if accepted.contains(&r.state.as_str()) {
        return Ok(&r.state);
    }
    let why = if why.is_empty() {
        String::new()
    } else {
        format!(": {why}")
    };
    Err(refuse(
        r,
        "unsupported-state",
        format!(
            "resource type {:?} does not accept state {:?} on this target (accepted: {}){why}",
            r.r#type,
            r.state,
            accepted.join(", ")
        ),
    ))
}

/// exec is the escape hatch: there is no state to converge on, only a
/// command to run.
fn no_state(r: &Resource) -> Result<(), CompileError> {
    if r.state.is_empty() {
        return Ok(());
    }
    Err(refuse(
        r,
        "unsupported-state",
        format!(
            "resource type {:?} accepts no state on this target, got {:?}",
            r.r#type, r.state
        ),
    ))
}
