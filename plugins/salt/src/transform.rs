//! Stage one: each IR resource becomes a native state, and each notify edge
//! becomes a requisite.

use std::collections::{BTreeMap, HashMap};

use meridian_sdk::{CompileError, EdgeKind, Resource, ResourceGraph, Value, Warning};

use crate::{refuse, Reference, Sls, State, NAME};

/// What one IR state of a resource type compiles to.
struct Shape {
    /// The function of the state module, for example `installed`.
    function: &'static str,
    /// The IR params that mean something to this function, each with its
    /// native argument name. An empty native name marks a param that steers
    /// the mapping itself and never becomes an argument.
    params: &'static [(&'static str, &'static str)],
    /// IR params that describe a thing which will no longer exist. They are
    /// left out with a warning and not refused, so a resource can be switched
    /// to absent without first being stripped.
    moot: &'static [&'static str],
    /// Arguments the function is always given, beyond what the author wrote.
    fixed: &'static [(&'static str, bool)],
}

/// How a notify action reaches a resource.
#[derive(Clone, Copy, PartialEq)]
enum Reaction {
    /// The resource's own state watches the notifier: a running service
    /// restarts when something it watches changes.
    Watch,
    /// A further state of the given IR state, tied to the notifier by a
    /// `watch` requisite, with the fixed arguments added.
    WatchingState(&'static str, &'static [(&'static str, bool)]),
    /// A further state of the given IR state that runs only when the
    /// notifier changed.
    OnChanges(&'static str),
}

/// How one IR resource type becomes a native state.
///
/// The table is explicit on purpose: an IR type with no entry is a hard
/// error, and a param with no entry is a hard error, so the target can never
/// silently drop something the author asked for.
struct Mapping {
    /// The state module, which is also what a requisite names it by.
    module: &'static str,
    /// Every accepted IR state and what it compiles to. Empty state names the
    /// one shape of a type that accepts no state at all.
    states: &'static [(&'static str, Shape)],
    /// The IR state assumed when a resource declares none.
    default_state: &'static str,
    /// IR params that must be present.
    required: &'static [&'static str],
    /// Each notify action and how it is carried out.
    actions: &'static [(&'static str, Reaction)],
    /// The native function a resource must compile to for it to be notified
    /// with [`Reaction::Watch`] or [`Reaction::WatchingState`], and why.
    watchable: Option<(&'static str, &'static str)>,
}

const PACKAGE_INSTALLED: Shape = Shape {
    function: "installed",
    params: &[("name", "name"), ("version", "version")],
    moot: &[],
    fixed: &[],
};
const SERVICE_RUNNING: Shape = Shape {
    function: "running",
    params: &[("enabled", "enable"), ("name", "name")],
    moot: &[],
    fixed: &[],
};
const FILE_MANAGED: Shape = Shape {
    function: "managed",
    params: &[
        ("content", "contents"),
        ("group", "group"),
        ("mode", "mode"),
        ("owner", "user"),
        ("path", "name"),
    ],
    moot: &[],
    fixed: &[],
};
const USER_PARAMS: &[(&str, &str)] = &[
    ("groups", "groups"),
    ("home", "home"),
    ("name", "name"),
    ("shell", "shell"),
    ("system", "system"),
    ("uid", "uid"),
];
const GROUP_PARAMS: &[(&str, &str)] = &[("gid", "gid"), ("name", "name"), ("system", "system")];

fn mapping(resource_type: &str) -> Option<Mapping> {
    Some(match resource_type {
        "package" => Mapping {
            module: "pkg",
            states: &[
                (
                    "absent",
                    Shape {
                        function: "removed",
                        params: &[("name", "name")],
                        moot: &[],
                        fixed: &[],
                    },
                ),
                (
                    "latest",
                    Shape {
                        function: "latest",
                        params: &[("name", "name")],
                        moot: &[],
                        fixed: &[],
                    },
                ),
                ("present", PACKAGE_INSTALLED),
            ],
            default_state: "present",
            required: &["name"],
            actions: &[],
            watchable: None,
        },
        "service" => Mapping {
            module: "service",
            states: &[
                ("running", SERVICE_RUNNING),
                ("started", SERVICE_RUNNING),
                (
                    "stopped",
                    Shape {
                        function: "dead",
                        params: &[("enabled", "enable"), ("name", "name")],
                        moot: &[],
                        fixed: &[],
                    },
                ),
            ],
            default_state: "running",
            required: &["name"],
            actions: &[
                (
                    "reload",
                    Reaction::WatchingState("running", &[("reload", true)]),
                ),
                ("restart", Reaction::Watch),
                ("start", Reaction::OnChanges("running")),
                ("stop", Reaction::OnChanges("stopped")),
            ],
            watchable: Some((
                "running",
                "Salt restarts or reloads a service through the state that keeps it running, \
                 and this one is to be stopped",
            )),
        },
        "file" => Mapping {
            module: "file",
            states: &[
                (
                    "absent",
                    Shape {
                        function: "absent",
                        params: &[("path", "name")],
                        moot: &["group", "mode", "owner"],
                        fixed: &[],
                    },
                ),
                (
                    "directory",
                    Shape {
                        function: "directory",
                        params: &[
                            ("group", "group"),
                            ("mode", "mode"),
                            ("owner", "user"),
                            ("path", "name"),
                        ],
                        moot: &[],
                        // Salt refuses to create a directory whose parent is
                        // missing unless told to; a directory resource means
                        // the whole path.
                        fixed: &[("makedirs", true)],
                    },
                ),
                ("file", FILE_MANAGED),
                ("present", FILE_MANAGED),
            ],
            default_state: "present",
            required: &["path"],
            actions: &[],
            watchable: None,
        },
        "user" => Mapping {
            module: "user",
            states: &[
                (
                    "absent",
                    Shape {
                        function: "absent",
                        params: &[("name", "name")],
                        moot: &["groups", "home", "shell", "system", "uid"],
                        fixed: &[],
                    },
                ),
                (
                    "present",
                    Shape {
                        function: "present",
                        params: USER_PARAMS,
                        moot: &[],
                        fixed: &[],
                    },
                ),
            ],
            default_state: "present",
            required: &["name"],
            actions: &[],
            watchable: None,
        },
        "group" => Mapping {
            module: "group",
            states: &[
                (
                    "absent",
                    Shape {
                        function: "absent",
                        params: &[("name", "name")],
                        moot: &["gid", "system"],
                        fixed: &[],
                    },
                ),
                (
                    "present",
                    Shape {
                        function: "present",
                        params: GROUP_PARAMS,
                        moot: &[],
                        fixed: &[],
                    },
                ),
            ],
            default_state: "present",
            required: &["name"],
            actions: &[],
            watchable: None,
        },
        "exec" => Mapping {
            // exec is the documented escape hatch, so it takes no state:
            // there is no desired state to converge on, only a command to run.
            module: "cmd",
            states: &[(
                "",
                Shape {
                    function: "run",
                    params: &[
                        ("chdir", "cwd"),
                        ("command", "name"),
                        ("creates", "creates"),
                        ("removes", "onlyif"),
                        ("shell", ""),
                    ],
                    moot: &[],
                    fixed: &[],
                },
            )],
            default_state: "",
            required: &["command"],
            actions: &[("run", Reaction::OnChanges(""))],
            watchable: None,
        },
        _ => return None,
    })
}

/// What each resource type takes, read from the same table a resource is
/// checked against, so an editor is never told something `emit` refuses.
pub fn resource_schemas() -> Vec<meridian_sdk::ResourceSchema> {
    supported_resource_types()
        .iter()
        .filter_map(|name| {
            let m = mapping(name)?;
            // Which params a type takes depends on its state; an editor is
            // offered every param some state takes.
            let mut params = Vec::new();
            for (_, shape) in m.states {
                params.extend(shape.params.iter().map(|(param, _)| *param));
                params.extend(shape.moot.iter().copied());
            }
            let states: Vec<&str> = m
                .states
                .iter()
                .map(|(state, _)| *state)
                .filter(|state| !state.is_empty())
                .collect();
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

/// Every IR resource type this target maps, sorted.
pub fn supported_resource_types() -> Vec<String> {
    ["exec", "file", "group", "package", "service", "user"]
        .map(str::to_owned)
        .to_vec()
}

fn keys<T>(pairs: &[(&str, T)]) -> String {
    pairs.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
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

/// Maps the target-agnostic tree onto native states and turns its notify
/// edges into requisites. It does no ordering work: the resources arrive in
/// apply order and leave in the same order, and dependency edges are the sort
/// stage's.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<(Sls, Vec<Warning>), CompileError> {
    let mut warnings = Vec::new();
    let mut states = Vec::with_capacity(graph.resources.len());
    for resource in &graph.resources {
        states.push(state_for(resource, &resource.state, Some(&mut warnings))?);
    }
    let reactions = notifications(graph, &mut states)?;
    Ok((
        Sls {
            name: graph.name.clone(),
            host: graph.host.clone(),
            states,
            reactions,
        },
        warnings,
    ))
}

/// Maps one resource onto its native state, in the given IR state. Warnings
/// are collected only when the caller wants them: a reaction built from a
/// resource would otherwise repeat what its own state already said.
fn state_for(
    r: &Resource,
    state: &str,
    mut warnings: Option<&mut Vec<Warning>>,
) -> Result<State, CompileError> {
    let m = mapping(&r.r#type).ok_or_else(|| unsupported_type(r))?;
    let shape = shape_for(r, state, &m)?;
    let shown_state = if state.is_empty() {
        m.default_state
    } else {
        state
    };

    let mut args = BTreeMap::new();
    for (name, value) in &r.params {
        let Some((_, native)) = shape.params.iter().find(|(k, _)| k == name) else {
            if shape.moot.contains(&name.as_str()) {
                if let Some(warnings) = warnings.as_deref_mut() {
                    warnings.push(Warning {
                        target: NAME.to_owned(),
                        resource: r.id.clone(),
                        msg: format!(
                            "param {name:?} means nothing for state {shown_state:?} and was left out"
                        ),
                    });
                }
                continue;
            }
            return Err(unaccepted_param(r, name, shown_state, &m, shape));
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
        args.insert((*native).to_owned(), value.clone());
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

    for (name, value) in shape.fixed {
        args.insert((*name).to_owned(), Value::Bool(*value));
    }
    match r.r#type.as_str() {
        "exec" => exec_details(r, &mut args)?,
        // With no content to write, only the file's existence and attributes
        // are managed. Saying so keeps Salt from ever touching what an
        // existing file holds.
        "file" if shape.function == "managed" && !args.contains_key("contents") => {
            args.insert("replace".to_owned(), Value::Bool(false));
        }
        _ => {}
    }

    // `name` leads, as a person would write it; the rest keep their sorted
    // order, so the same resource always renders to the same bytes.
    let mut ordered = Vec::with_capacity(args.len());
    if let Some(name) = args.remove("name") {
        ordered.push(("name".to_owned(), name));
    }
    ordered.extend(args);

    Ok(State {
        id: r.id.clone(),
        module: m.module,
        function: shape.function,
        args: ordered,
        require: Vec::new(),
        watch: Vec::new(),
        onchanges: Vec::new(),
        when: r.runtime_when.clone(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// Finds what an IR state compiles to. Every accepted value is in the table,
/// so an unrecognised one names the values that would have worked.
fn shape_for<'m>(r: &Resource, state: &str, m: &'m Mapping) -> Result<&'m Shape, CompileError> {
    if let [("", shape)] = m.states {
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
        return Ok(shape);
    }
    let wanted = if state.is_empty() {
        m.default_state
    } else {
        state
    };
    m.states
        .iter()
        .find(|(name, _)| *name == wanted)
        .map(|(_, shape)| shape)
        .ok_or_else(|| {
            refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} does not accept state {:?} on this target (accepted: {})",
                    r.r#type,
                    wanted,
                    keys(m.states)
                ),
            )
        })
}

/// A param the chosen function does not take is one of two mistakes: a param
/// the type never has, or one that only makes sense in another state.
fn unaccepted_param(
    r: &Resource,
    name: &str,
    state: &str,
    m: &Mapping,
    shape: &Shape,
) -> CompileError {
    let elsewhere: Vec<&str> = m
        .states
        .iter()
        .filter(|(_, other)| other.params.iter().any(|(k, _)| *k == name))
        .map(|(s, _)| *s)
        .collect();
    if elsewhere.is_empty() {
        let mut accepted: Vec<&str> = m
            .states
            .iter()
            .flat_map(|(_, s)| s.params.iter().map(|(k, _)| *k))
            .collect();
        accepted.sort_unstable();
        accepted.dedup();
        return refuse(
            r,
            "unsupported-param",
            format!(
                "resource type {:?} does not accept param {:?} on this target (accepted: {})",
                r.r#type,
                name,
                accepted.join(", ")
            ),
        );
    }
    refuse(
        r,
        "param-conflicts-with-state",
        format!(
            "param {:?} cannot be used with state {:?}, which compiles to {}.{} \
             (it is accepted with state: {})",
            name,
            state,
            m.module,
            shape.function,
            elsewhere.join(", ")
        ),
    )
}

/// Characters a shell gives a meaning to. Quotes are left out: a quoted word
/// reads the same with a shell as without one.
const SHELL_SYNTAX: &[char] = &[
    '|', '&', ';', '<', '>', '(', ')', '$', '`', '\\', '*', '?', '[', ']', '{', '}', '~', '#', '\n',
];

/// The two exec params that are not a rename.
fn exec_details(r: &Resource, args: &mut BTreeMap<String, Value>) -> Result<(), CompileError> {
    // Salt's cmd.run always hands the command to a shell. A document that did
    // not ask for one would have its command reinterpreted, so a command that
    // a shell would read differently has to say a shell is what it wants.
    if r.params.get("shell") != Some(&Value::Bool(true)) {
        if let Some(command) = r.params.get("command").and_then(Value::as_str) {
            if let Some(found) = command.chars().find(|c| SHELL_SYNTAX.contains(c)) {
                return Err(refuse(
                    r,
                    "shell-required",
                    format!(
                        "the command contains {found:?}, which a shell gives a meaning to, and \
                         Salt always runs a command through a shell; set param \"shell\" to true \
                         to say that is intended"
                    ),
                ));
            }
        }
    }
    // cmd.run has no `removes`. The same test, "run only if this path is
    // there", is an `onlyif` that asks Salt's own file module, so no shell
    // quoting is involved and it reads the same on every platform.
    if let Some(path) = args.remove("onlyif") {
        let test = BTreeMap::from([
            ("fun".to_owned(), Value::String("file.access".to_owned())),
            ("mode".to_owned(), Value::String("f".to_owned())),
            ("path".to_owned(), path),
        ]);
        args.insert("onlyif".to_owned(), Value::List(vec![Value::Map(test)]));
    }
    Ok(())
}

/// The ID of the state a notify edge compiles to, when it needs one of its
/// own. It is derived from the action and the notified resource, so two
/// notifiers of the same change share one state.
fn reaction_id(action: &str, resource: &str) -> String {
    format!("{action} {resource}")
}

/// Turns every notify edge into a requisite: on the notified resource's own
/// state where Salt reacts there, and otherwise on a further state, which is
/// returned sorted by ID.
fn notifications(graph: &ResourceGraph, states: &mut [State]) -> Result<Vec<State>, CompileError> {
    let index: HashMap<String, usize> = states
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let mut reactions: BTreeMap<String, State> = BTreeMap::new();

    for edge in graph.edges_of_kind(EdgeKind::Notify) {
        let (Some(to), Some(&to_at)) = (graph.resource(&edge.to), index.get(&edge.to)) else {
            // The core already rejects an edge to an unknown resource; this
            // guards against a tree assembled some other way.
            let from = graph.resource(&edge.from).cloned().unwrap_or_default();
            return Err(refuse(
                &from,
                "unknown-notify-target",
                format!("notifies an unknown resource {:?}", edge.to),
            ));
        };
        let Some(&from_at) = index.get(&edge.from) else {
            return Err(refuse(
                to,
                "unknown-notifier",
                format!("is notified by an unknown resource {:?}", edge.from),
            ));
        };
        let notifier = states[from_at].reference();

        let m = mapping(&to.r#type).ok_or_else(|| unsupported_type(to))?;
        let found = m.actions.iter().find(|(a, _)| *a == edge.action);
        let Some(mut reaction) = found.map(|(_, reaction)| *reaction) else {
            let msg = if m.actions.is_empty() {
                format!(
                    "resource type {:?} cannot be notified on this target, got action {:?}",
                    to.r#type, edge.action
                )
            } else {
                format!(
                    "resource type {:?} does not support notify action {:?} on this target (supported: {})",
                    to.r#type,
                    edge.action,
                    keys(m.actions)
                )
            };
            return Err(refuse(to, "unsupported-action", msg));
        };

        if matches!(reaction, Reaction::Watch | Reaction::WatchingState(..)) {
            if let Some((function, why)) = m.watchable {
                if states[to_at].function != function {
                    let state = if to.state.is_empty() {
                        m.default_state
                    } else {
                        &to.state
                    };
                    return Err(refuse(
                        to,
                        "unsupported-action",
                        format!(
                            "cannot be notified with action {:?} while its state is {:?}: {why}",
                            edge.action, state
                        ),
                    ));
                }
            }
        }
        // A watch on the resource's own state orders the notifier first. When
        // the notifier is applied later than the resource, that would turn
        // the order round, so the reaction gets a state of its own instead,
        // placed after both.
        if reaction == Reaction::Watch && from_at > to_at {
            reaction = Reaction::WatchingState(m.default_state, &[]);
        }

        let (ir_state, extra, watching) = match reaction {
            Reaction::Watch => {
                push_unique(&mut states[to_at].watch, notifier);
                continue;
            }
            Reaction::WatchingState(ir_state, extra) => (ir_state, extra, true),
            Reaction::OnChanges(ir_state) => (ir_state, &[][..], false),
        };
        let id = reaction_id(&edge.action, &to.id);
        if !reactions.contains_key(&id) {
            let mut state = state_for(to, ir_state, None)?;
            state.id.clone_from(&id);
            for (name, value) in extra {
                state.args.push(((*name).to_owned(), Value::Bool(*value)));
            }
            // It reacts to a change, so it must not carry the notified
            // resource's own runtime condition: it runs because something
            // changed, not because the condition still holds.
            state.when = String::new();
            // After the resource it acts on, whatever the file order says.
            state.require.push(states[to_at].reference());
            reactions.insert(id.clone(), state);
        }
        let state = reactions.get_mut(&id).expect("just inserted");
        if watching {
            push_unique(&mut state.watch, notifier);
        } else {
            push_unique(&mut state.onchanges, notifier);
        }
    }
    Ok(reactions.into_values().collect())
}

fn push_unique(list: &mut Vec<Reference>, reference: Reference) {
    if !list.contains(&reference) {
        list.push(reference);
    }
}
