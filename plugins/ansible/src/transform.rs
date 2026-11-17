//! Stage one: each IR resource becomes a native module call.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{CompileError, EdgeKind, Resource, ResourceGraph, ResourceSchema, Value};

use crate::{refuse, Playbook, Task};

const COPY: &str = "ansible.builtin.copy";

/// How one IR resource type becomes a native module call.
///
/// The table is explicit on purpose: an IR type with no entry is a hard
/// error, and a param with no entry is a hard error, so the target can never
/// silently drop something the author asked for.
struct Mapping {
    /// The native module, which may depend on the resource's params.
    module: fn(&Resource) -> &'static str,
    /// The module argument the IR `state` maps onto, given the module chosen.
    /// `None` means that module implies its own state and takes no argument.
    state_arg: fn(module: &str) -> Option<&'static str>,
    /// Every accepted IR state and its native equivalent. Empty means the
    /// type accepts no state at all.
    states: &'static [(&'static str, &'static str)],
    /// The native states a module may imply instead of emit. Any other state
    /// is an error on such a module, because dropping it would change what
    /// the resource means.
    states_without_arg: &'static [&'static str],
    /// The IR state assumed when a resource declares none.
    default_state: &'static str,
    /// Every accepted IR param and its native argument name, given the
    /// module. An empty native name marks a param that steers the mapping
    /// itself and never becomes an argument.
    params: fn(module: &str) -> &'static [(&'static str, &'static str)],
    /// IR params that must be present.
    required: &'static [&'static str],
    /// Each notify action and the IR state the synthesised handler applies.
    actions: &'static [(&'static str, &'static str)],
}

fn state_arg(_module: &str) -> Option<&'static str> {
    Some("state")
}

const PRESENT_ABSENT: &[(&str, &str)] = &[("absent", "absent"), ("present", "present")];

fn mapping(resource_type: &str) -> Option<Mapping> {
    Some(match resource_type {
        "package" => Mapping {
            module: |_| "ansible.builtin.package",
            state_arg,
            states: &[
                ("absent", "absent"),
                ("latest", "latest"),
                ("present", "present"),
            ],
            states_without_arg: &[],
            default_state: "present",
            params: |_| &[("name", "name")],
            required: &["name"],
            actions: &[],
        },
        "service" => Mapping {
            module: |_| "ansible.builtin.service",
            state_arg,
            states: &[
                ("reloaded", "reloaded"),
                ("restarted", "restarted"),
                ("running", "started"),
                ("started", "started"),
                ("stopped", "stopped"),
            ],
            states_without_arg: &[],
            default_state: "running",
            params: |_| &[("enabled", "enabled"), ("name", "name")],
            required: &["name"],
            actions: &[
                ("reload", "reloaded"),
                ("restart", "restarted"),
                ("start", "started"),
                ("stop", "stopped"),
            ],
        },
        "file" => Mapping {
            // A file with literal content is a copy, not a change to a file's
            // attributes; the two take different native argument names.
            module: |r| {
                if r.params.contains_key("content") {
                    COPY
                } else {
                    "ansible.builtin.file"
                }
            },
            // The copy module writes a file by definition and rejects a state
            // argument, so "file" is implied there and any other state
            // contradicts literal content.
            state_arg: |module| (module != COPY).then_some("state"),
            states: &[
                ("absent", "absent"),
                ("directory", "directory"),
                ("file", "file"),
                ("present", "file"),
            ],
            states_without_arg: &["file"],
            default_state: "present",
            params: |module| {
                if module == COPY {
                    &[
                        ("content", "content"),
                        ("group", "group"),
                        ("mode", "mode"),
                        ("owner", "owner"),
                        ("path", "dest"),
                    ]
                } else {
                    &[
                        ("group", "group"),
                        ("mode", "mode"),
                        ("owner", "owner"),
                        ("path", "path"),
                    ]
                }
            },
            required: &["path"],
            actions: &[],
        },
        "user" => Mapping {
            module: |_| "ansible.builtin.user",
            state_arg,
            states: PRESENT_ABSENT,
            states_without_arg: &[],
            default_state: "present",
            params: |_| {
                &[
                    ("groups", "groups"),
                    ("home", "home"),
                    ("name", "name"),
                    ("shell", "shell"),
                    ("system", "system"),
                    ("uid", "uid"),
                ]
            },
            required: &["name"],
            actions: &[],
        },
        "group" => Mapping {
            module: |_| "ansible.builtin.group",
            state_arg,
            states: PRESENT_ABSENT,
            states_without_arg: &[],
            default_state: "present",
            params: |_| &[("gid", "gid"), ("name", "name"), ("system", "system")],
            required: &["name"],
            actions: &[],
        },
        "exec" => Mapping {
            // exec is the documented escape hatch, so it takes no state:
            // there is no desired state to converge on, only a command to run.
            module: |r| {
                if r.params.get("shell") == Some(&Value::Bool(true)) {
                    "ansible.builtin.shell"
                } else {
                    "ansible.builtin.command"
                }
            },
            state_arg,
            states: &[],
            states_without_arg: &[],
            default_state: "",
            params: |_| {
                &[
                    ("chdir", "chdir"),
                    ("command", "cmd"),
                    ("creates", "creates"),
                    ("removes", "removes"),
                    ("shell", ""),
                ]
            },
            required: &["command"],
            actions: &[("run", "")],
        },
        _ => return None,
    })
}

/// Every IR resource type this target maps, sorted.
pub fn supported_resource_types() -> Vec<String> {
    ["exec", "file", "group", "package", "service", "user"]
        .map(str::to_owned)
        .to_vec()
}

/// What each resource type takes, read from the same table a resource is
/// checked against, so an editor is never told something `emit` refuses.
pub fn resource_schemas() -> Vec<ResourceSchema> {
    supported_resource_types()
        .iter()
        .filter_map(|name| {
            let m = mapping(name)?;
            // A type's params can depend on the module its params select;
            // an editor is offered every param some module takes.
            let mut params = Vec::new();
            for module in [COPY, ""] {
                params.extend((m.params)(module).iter().map(|(param, _)| *param));
            }
            let states: Vec<&str> = m.states.iter().map(|(state, _)| *state).collect();
            let actions: Vec<&str> = m.actions.iter().map(|(action, _)| *action).collect();
            Some(meridian_sdk::schema::shared(
                name,
                &params,
                m.required,
                &states,
                m.default_state,
                &actions,
            ))
        })
        .collect()
}

fn keys(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
}

fn lookup(pairs: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn unsupported_type(r: &Resource) -> CompileError {
    refuse(
        r,
        "unsupported-resource-type",
        format!(
            "unsupported resource type {:?} on this target (supported: {})",
            r.r#type,
            supported_resource_types().join(", ")
        ),
    )
}

/// Maps the target-agnostic tree onto native tasks and the handlers its
/// notify edges require. It does no ordering work: the resources arrive in
/// apply order and leave in the same order.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Playbook, CompileError> {
    let mut notify = handler_names(graph)?;
    let mut tasks = Vec::with_capacity(graph.resources.len());
    for resource in &graph.resources {
        let mut task = task_for(resource, &resource.state)?;
        task.notify = notify.remove(&resource.id).unwrap_or_default();
        tasks.push(task);
    }
    Ok(Playbook {
        name: graph.name.clone(),
        hosts: graph.host.clone(),
        tasks,
        handlers: handlers(graph)?,
    })
}

/// Maps one resource onto its native module call, in the given state.
fn task_for(r: &Resource, state: &str) -> Result<Task, CompileError> {
    let m = mapping(&r.r#type).ok_or_else(|| unsupported_type(r))?;
    let module = (m.module)(r);
    let allowed = (m.params)(module);

    let mut args = BTreeMap::new();
    for (name, value) in &r.params {
        let Some(native) = lookup(allowed, name) else {
            return Err(refuse(
                r,
                "unsupported-param",
                format!(
                    "resource type {:?} does not accept param {:?} on this target (accepted: {})",
                    r.r#type,
                    name,
                    keys(allowed)
                ),
            ));
        };
        if native.is_empty() {
            continue;
        }
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
        args.insert(native.to_owned(), value.clone());
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

    apply_state(r, state, &m, module, &mut args)?;

    Ok(Task {
        name: r.id.clone(),
        module: module.to_owned(),
        args,
        notify: Vec::new(),
        when: r.runtime_when.clone(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// Maps the IR state vocabulary onto the module's own. Every accepted value
/// is in the table, so an unrecognised one names the values that would have
/// worked.
fn apply_state(
    r: &Resource,
    state: &str,
    m: &Mapping,
    module: &str,
    args: &mut BTreeMap<String, Value>,
) -> Result<(), CompileError> {
    if m.states.is_empty() {
        if !state.is_empty() {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} accepts no state on this target, got {:?}",
                    r.r#type, state
                ),
            ));
        }
        return Ok(());
    }

    let state = if state.is_empty() {
        m.default_state
    } else {
        state
    };
    let Some(native) = lookup(m.states, state) else {
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
    match (m.state_arg)(module) {
        Some(arg) => {
            args.insert(arg.to_owned(), Value::String(native.to_owned()));
        }
        None if m.states_without_arg.contains(&native) => {}
        None => {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} cannot have state {:?} as compiled to {} on this target",
                    r.r#type, state, module
                ),
            ));
        }
    }
    Ok(())
}

/// The name a notify edge compiles to. It is derived from the action and the
/// notified resource, so two notifiers of the same change collapse onto one
/// handler, which is what Ansible's own dedupe expects.
fn handler_name(action: &str, resource: &str) -> String {
    format!("{action} {resource}")
}

/// Per notifying resource, the sorted handler names its task lists under
/// `notify:`.
fn handler_names(graph: &ResourceGraph) -> Result<BTreeMap<String, Vec<String>>, CompileError> {
    let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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
        handler_for(to, &edge.action)?;
        names
            .entry(edge.from.clone())
            .or_default()
            .insert(handler_name(&edge.action, &edge.to));
    }
    Ok(names
        .into_iter()
        .map(|(from, set)| (from, set.into_iter().collect()))
        .collect())
}

/// The deduped handler block for every distinct notify target and action.
fn handlers(graph: &ResourceGraph) -> Result<Vec<Task>, CompileError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for edge in graph.edges_of_kind(EdgeKind::Notify) {
        if !seen.insert(handler_name(&edge.action, &edge.to)) {
            continue;
        }
        if let Some(to) = graph.resource(&edge.to) {
            out.push(handler_for(to, &edge.action)?);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Synthesises the handler that performs one action on one resource. The
/// handler is the resource's own module call with the action's state
/// applied, not a shell-out, so the notified change stays idempotent.
fn handler_for(r: &Resource, action: &str) -> Result<Task, CompileError> {
    let m = mapping(&r.r#type).ok_or_else(|| unsupported_type(r))?;
    let Some(state) = lookup(m.actions, action) else {
        let msg = if m.actions.is_empty() {
            format!(
                "resource type {:?} cannot be notified on this target, got action {:?}",
                r.r#type, action
            )
        } else {
            format!(
                "resource type {:?} does not support notify action {:?} on this target (supported: {})",
                r.r#type,
                action,
                keys(m.actions)
            )
        };
        return Err(refuse(r, "unsupported-action", msg));
    };

    let mut task = task_for(r, state)?;
    task.name = handler_name(action, &r.id);
    // A handler must not carry the notified task's own runtime condition: it
    // runs because something changed, not because the condition still holds.
    task.when = String::new();
    Ok(task)
}
