//! Stage one: each IR resource becomes a native declaration.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{CompileError, Edge, EdgeKind, Resource, ResourceGraph, Value};

use crate::{refuse, Attr, Declaration, Manifest, Reference, NAME};

/// How one IR resource type becomes a native Puppet declaration.
///
/// The table is explicit on purpose: an IR type with no entry is a hard
/// error, and a param with no entry is a hard error, so the target can never
/// silently drop something the author asked for.
pub(crate) struct Mapping {
    /// The native resource type as it is written in a declaration.
    pub puppet_type: &'static str,
    /// Every accepted IR state and its native ensure value, sorted by state.
    /// Empty means the type takes no ensure at all.
    ensure: &'static [(&'static str, &'static str)],
    /// The IR state assumed when a resource declares none.
    default_state: &'static str,
    /// Every accepted IR param and its native attribute, sorted by param. An
    /// empty native name steers the mapping and never becomes an attribute.
    params: &'static [(&'static str, &'static str)],
    /// IR params that must be present.
    required: &'static [&'static str],
    /// The native attribute Puppet identifies the managed thing by. Two
    /// resources of one type sharing it manage the same thing under two
    /// titles, which Puppet only discovers at apply time.
    pub namevar: &'static str,
    /// Whether Puppet can do anything at all when this type is notified.
    refreshable: bool,
    /// The notify actions this type performs natively, sorted. Puppet's
    /// refresh carries no argument, so an action is either something the type
    /// already does on refresh or something this target must refuse.
    actions: &'static [&'static str],
    /// Why Puppet cannot express each refused action.
    refused_actions: &'static [(&'static str, &'static str)],
}

const PRESENT_ABSENT: &[(&str, &str)] = &[("absent", "absent"), ("present", "present")];

pub(crate) fn mapping(resource_type: &str) -> Option<Mapping> {
    Some(match resource_type {
        "package" => Mapping {
            puppet_type: "package",
            ensure: &[
                ("absent", "absent"),
                ("latest", "latest"),
                ("present", "present"),
            ],
            default_state: "present",
            params: &[("name", "name")],
            required: &["name"],
            namevar: "name",
            refreshable: false,
            actions: &[],
            refused_actions: &[],
        },
        "service" => Mapping {
            puppet_type: "service",
            // A catalog describes desired state, and "restarted" is not a
            // state a node can be left in. Ansible accepts it because a task
            // is an action; here the equivalent is a notifies edge.
            ensure: &[
                ("running", "running"),
                ("started", "running"),
                ("stopped", "stopped"),
            ],
            default_state: "running",
            params: &[("enabled", "enable"), ("name", "name")],
            required: &["name"],
            namevar: "name",
            refreshable: true,
            actions: &["refresh", "restart"],
            refused_actions: &[
                (
                    "reload",
                    "Puppet's refresh restarts a service; it takes no argument and \
                     cannot be told to reload instead. Emitting a restart here would do \
                     more than the document asked for. Set the service's own restart \
                     attribute in a Puppet module if a reload command exists for it",
                ),
                (
                    "start",
                    "a service that should be running is already declared with \
                     state: running; Puppet converges on that without being notified",
                ),
                (
                    "stop",
                    "Puppet refreshes a service by restarting it and has no way to \
                     stop one in response to a change; declare state: stopped instead",
                ),
            ],
        },
        "file" => Mapping {
            puppet_type: "file",
            ensure: &[
                ("absent", "absent"),
                ("directory", "directory"),
                ("file", "file"),
                ("present", "file"),
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
            namevar: "path",
            refreshable: false,
            actions: &[],
            refused_actions: &[],
        },
        "user" => Mapping {
            puppet_type: "user",
            ensure: PRESENT_ABSENT,
            default_state: "present",
            params: &[
                ("groups", "groups"),
                ("home", "home"),
                ("name", "name"),
                ("shell", "shell"),
                ("system", "system"),
                ("uid", "uid"),
            ],
            required: &["name"],
            namevar: "name",
            refreshable: false,
            actions: &[],
            refused_actions: &[],
        },
        "group" => Mapping {
            puppet_type: "group",
            ensure: PRESENT_ABSENT,
            default_state: "present",
            params: &[("gid", "gid"), ("name", "name"), ("system", "system")],
            required: &["name"],
            namevar: "name",
            refreshable: false,
            actions: &[],
            refused_actions: &[],
        },
        "exec" => Mapping {
            // exec is the documented escape hatch, so it takes no ensure:
            // there is no desired state to converge on, only a command to run.
            puppet_type: "exec",
            ensure: &[],
            default_state: "",
            params: &[
                ("chdir", "cwd"),
                ("command", "command"),
                ("creates", "creates"),
                ("shell", ""),
            ],
            required: &["command"],
            namevar: "command",
            refreshable: true,
            actions: &["run"],
            refused_actions: &[],
        },
        _ => return None,
    })
}

/// What each resource type takes, read from the same table a resource is
/// checked against, so an editor is never told something `emit` refuses.
pub fn resource_schemas() -> Vec<meridian_sdk::ResourceSchema> {
    TYPES
        .iter()
        .filter_map(|name| {
            let m = mapping(name)?;
            let params: Vec<&str> = m.params.iter().map(|(param, _)| *param).collect();
            let states: Vec<&str> = m.ensure.iter().map(|(state, _)| *state).collect();
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

const TYPES: [&str; 6] = ["exec", "file", "group", "package", "service", "user"];

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

/// Maps the target-agnostic tree onto native declarations and the notify
/// metaparameters its notifies edges require. It does no ordering work:
/// `require` is the sort stage's job, because whether one is needed depends
/// on what notify already implies.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Manifest, CompileError> {
    let class = class_name(&graph.name)?;
    let mut resources = Vec::with_capacity(graph.resources.len());
    for resource in &graph.resources {
        resources.push(declaration_for(resource)?);
    }
    let mut notify = notifications(graph, &resources)?;
    for declaration in &mut resources {
        declaration.notify = notify.remove(&declaration.resource_id).unwrap_or_default();
    }
    Ok(Manifest {
        class,
        host: graph.host.clone(),
        resources,
    })
}

fn declaration_for(r: &Resource) -> Result<Declaration, CompileError> {
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

    // A catalog is compiled first and applied later, so there is no moment at
    // which an apply-time expression could be evaluated. Compiling it away
    // would produce a manifest that does the wrong thing on every node the
    // condition was meant to exclude.
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            "unsupported-runtime-condition",
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: \
                 a Puppet catalog is compiled before it reaches the node, so there is \
                 no apply-time conditional to compile it into",
                r.runtime_when
            ),
        ));
    }

    // Puppet would take the manifest and fail on the node: a directory has
    // no content to manage.
    if r.r#type == "file" && r.state == "directory" && r.params.contains_key("content") {
        return Err(refuse(
            r,
            "param-conflicts-with-state",
            "param \"content\" cannot be used with state \"directory\": a directory has no \
             content; remove the param or the state"
                .to_owned(),
        ));
    }

    let mut attrs = BTreeMap::new();
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
        if native.is_empty() {
            // The param steers the mapping itself: exec's shell selects
            // Puppet's shell provider.
            if name == "shell" && *value == Value::Bool(true) {
                attrs.insert("provider".to_owned(), Attr::Bare("shell"));
            }
            continue;
        }
        attrs.insert(native.to_owned(), Attr::Value(value.clone()));
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

    if m.ensure.is_empty() {
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
    } else {
        let state = if r.state.is_empty() {
            m.default_state
        } else {
            &r.state
        };
        let Some(native) = lookup(m.ensure, state) else {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} does not accept state {:?} on this target (accepted: {})",
                    r.r#type,
                    state,
                    keys(m.ensure)
                ),
            ));
        };
        attrs.insert("ensure".to_owned(), Attr::Bare(native));
    }

    Ok(Declaration {
        r#type: m.puppet_type.to_owned(),
        title: r.id.clone(),
        attrs,
        require: Vec::new(),
        notify: Vec::new(),
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// Per notifying resource, the sorted references its declaration lists under
/// `notify`.
///
/// Puppet's notify does double duty: it refreshes the target and it orders
/// the notifier before it. Only the refresh is checked here; the ordering it
/// implies is the sort stage's concern.
fn notifications(
    graph: &ResourceGraph,
    declarations: &[Declaration],
) -> Result<BTreeMap<String, Vec<Reference>>, CompileError> {
    let mut out: BTreeMap<String, BTreeSet<(String, Reference)>> = BTreeMap::new();
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
        let Some(from) = graph.resource(&edge.from) else {
            return Err(CompileError {
                target: NAME.to_owned(),
                resource: edge.from.clone(),
                rule: "unknown-notifier".to_owned(),
                pos: edge.pos.clone(),
                msg: format!(
                    "a notifies edge starts at resource {:?}, which this document does not declare",
                    edge.from
                ),
            });
        };
        check_action(from, to, edge)?;

        let native = declarations
            .iter()
            .find(|d| d.resource_id == edge.to)
            .map(|d| d.r#type.clone())
            .unwrap_or_default();
        let reference = Reference {
            r#type: native,
            title: edge.to.clone(),
        };
        // Two edges to one resource collapse onto a single notify: Puppet's
        // refresh happens once however many times it was triggered. The set
        // is ordered by the reference as it is written.
        out.entry(edge.from.clone())
            .or_default()
            .insert((reference.to_string(), reference));
    }
    Ok(out
        .into_iter()
        .map(|(from, set)| (from, set.into_iter().map(|(_, r)| r).collect()))
        .collect())
}

/// Decides whether Puppet can perform one notify action on one resource.
///
/// The failure is reported against the resource that declared the notifies
/// edge, at the edge's own position, and not against the resource being
/// notified. The author wrote the action on the notifier, so that is the line
/// they have to change.
fn check_action(from: &Resource, to: &Resource, edge: &Edge) -> Result<(), CompileError> {
    let fail = |rule: &str, msg: String| {
        Err(CompileError {
            target: NAME.to_owned(),
            resource: from.id.clone(),
            rule: rule.to_owned(),
            msg,
            pos: if edge.pos.known() {
                edge.pos.clone()
            } else {
                from.pos.clone()
            },
        })
    };

    let Some(m) = mapping(&to.r#type) else {
        return fail(
            "unsupported-resource-type",
            format!(
                "notifies resource {:?}, whose type {:?} this target does not support (supported: {})",
                to.id,
                to.r#type,
                TYPES.join(", ")
            ),
        );
    };
    if !m.refreshable {
        return fail(
            "unsupported-action",
            format!(
                "notifies resource {:?}, which this target cannot refresh: Puppet refreshes \
                 only the types that define a refresh, and {} is not one of them",
                to.id, m.puppet_type
            ),
        );
    }
    if m.actions.contains(&edge.action.as_str()) {
        return Ok(());
    }
    if let Some(why) = lookup(m.refused_actions, &edge.action) {
        return fail(
            "unsupported-action",
            format!(
                "notifies resource {:?} with action {:?}, which this target cannot express: {}",
                to.id, edge.action, why
            ),
        );
    }
    fail(
        "unsupported-action",
        format!(
            "notifies resource {:?} with action {:?}, which resource type {:?} does not support \
             on this target (supported: {})",
            to.id,
            edge.action,
            to.r#type,
            m.actions.join(", ")
        ),
    )
}

/// Derives the manifest's class name from the document name. Puppet names a
/// class with a restricted vocabulary, so a document name that cannot become
/// one is refused instead of silently rewritten: a class whose name does not
/// match the document is a class an operator cannot find.
pub(crate) fn class_name(document: &str) -> Result<String, CompileError> {
    let name = format!("meridian_{document}");
    for (i, c) in name.chars().enumerate() {
        let ok = c == '_' || c.is_ascii_lowercase() || (i > 0 && c.is_ascii_digit());
        if !ok {
            return Err(CompileError {
                target: NAME.to_owned(),
                rule: "invalid-class-name".to_owned(),
                msg: format!(
                    "document name {:?} cannot become a Puppet class name: \
                     {:?} is not allowed, and a class name may hold only lower-case \
                     letters, digits and underscores",
                    document,
                    c.to_string()
                ),
                ..CompileError::default()
            });
        }
    }
    Ok(name)
}
