//! Stage one: each IR resource becomes a native module call.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::rules;
use meridian_sdk::{
    CompileError, EdgeKind, Resource, ResourceGraph, ResourceSchema, TypeName, Value,
};

use crate::{refuse, Playbook, Task, NAME};

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
                ("link", "link"),
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
                        ("target", "src"),
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

/// The six shared types, sorted.
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

/// How an editor is told that any module's full name is a type: a
/// namespace, a collection and a module, joined by dots.
pub const NATIVE_PATTERN: &str = "<namespace>.<collection>.<module>";

/// Every IR resource type this target maps, sorted: the shared six, and the
/// pattern every module's fully qualified collection name fits.
pub fn supported_resource_types() -> Vec<String> {
    let mut types = vec![NATIVE_PATTERN.to_owned()];
    types.extend(SHARED.iter().map(|t| (*t).to_owned()));
    types
}

/// The module a native type names, when the type is one. A module is
/// addressed by its full name, `namespace.collection.module`, each part
/// lower-case letters, digits and underscores, as Ansible itself requires
/// of a collection name; anything else is not a module and is refused.
///
/// `native/<name>` is read on every configuration management target, but
/// Ansible has no bare module names to reach that way: `native/file` is
/// refused with the full name to write instead.
pub(crate) fn native_module(r: &Resource) -> Result<Option<&str>, CompileError> {
    match TypeName::read(&r.r#type) {
        TypeName::Shared(_) => Ok(None),
        TypeName::NativeNamedLikeShared(bare) => Err(refuse(
            r,
            &rules::UNSUPPORTED_RESOURCE_TYPE,
            format!(
                "resource type {:?} is not how a module is reached on this target: an Ansible \
                 module is written by its full name, ansible.builtin.{bare}",
                r.r#type
            ),
        )),
        TypeName::Native(name) if is_module_name(name) => Ok(Some(name)),
        TypeName::Native(_) => Err(unsupported_type(r)),
    }
}

/// Whether `name` is shaped like a module's fully qualified collection name.
fn is_module_name(name: &str) -> bool {
    let word = |w: &str| {
        !w.is_empty()
            && !w.starts_with(|c: char| c.is_ascii_digit())
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| word(p))
}

/// What each resource type takes, read from the same table a resource is
/// checked against, so an editor is never told something `emit` refuses.
pub fn resource_schemas() -> Vec<ResourceSchema> {
    let mut schemas = vec![ResourceSchema::open(
        NATIVE_PATTERN,
        "Any Ansible module, by its fully qualified collection name, such as \
         ansible.builtin.lineinfile. The params are the module's arguments, passed through \
         as written; a state is written as the module's state argument. When notified it \
         runs again. The document then compiles for Ansible only.",
    )
    .actions(&["run"])];
    schemas.extend(SHARED.iter().filter_map(|name| {
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
    }));
    meridian_sdk::schema::insert_repository_schema(&mut schemas);
    meridian_sdk::schema::insert_cron_schema(&mut schemas);
    schemas
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
        &rules::UNSUPPORTED_RESOURCE_TYPE,
        format!(
            "unsupported resource type {:?} on this target: it is not one of the shared types \
             ({}), and an Ansible module is written by its full name, such as \
             ansible.builtin.lineinfile",
            r.r#type,
            SHARED.join(", ")
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
        // The shared types are checked the same way on every configuration
        // management target, so a document moves between tools without the
        // type rules changing under it.
        meridian_sdk::schema::check_shared(resource).map_err(|e| CompileError {
            target: NAME.to_owned(),
            ..e
        })?;
        let mut task = task_for(resource, &resource.state)?;
        task.notify = notify.remove(&resource.id).unwrap_or_default();
        tasks.push(task);
    }
    // The host is written into inventory.ini, where a space starts a host
    // variable and a line break a section. The core holds a host to what a
    // name can be before a plugin sees it; this is the plugin's own line of
    // defence, for a tree that did not come through the core.
    if !graph.host.is_empty() {
        meridian_sdk::check_host(&graph.host).map_err(|why| CompileError {
            target: NAME.to_owned(),
            resource: String::new(),
            rule: rules::INVALID_HOST.name.to_owned(),
            hint: rules::INVALID_HOST.hint.to_owned(),
            msg: format!("the target host cannot be written into the inventory: {why}"),
            pos: Default::default(),
        })?;
    }
    Ok(Playbook {
        name: graph.name.clone(),
        hosts: graph.host.clone(),
        inventory_vars: inventory_vars(graph.credential.as_ref())?,
        tasks,
        handlers: handlers(graph)?,
    })
}

/// What the inventory says about reaching the host as the credential: the
/// user. The credential's secret fields are not here; the runner hands them
/// to ansible-playbook privately. What ansible-playbook cannot take is
/// refused with the reason, never dropped.
fn inventory_vars(
    credential: Option<&meridian_sdk::Credential>,
) -> Result<Vec<(String, String)>, CompileError> {
    let Some(credential) = credential else {
        return Ok(Vec::new());
    };
    let refuse = |msg: String| CompileError {
        target: NAME.to_owned(),
        resource: String::new(),
        rule: rules::UNSUPPORTED_CREDENTIAL.name.to_owned(),
        hint: rules::UNSUPPORTED_CREDENTIAL.hint.to_owned(),
        msg: format!("the credential {:?}: {msg}", credential.name),
        pos: Default::default(),
    };
    match credential.kind.as_str() {
        "ssh" | "usernamePassword" => {}
        other => {
            return Err(refuse(format!(
                "ansible-playbook reaches a host as an ssh or usernamePassword credential, \
                 not {other}"
            )))
        }
    }
    if credential.has_secret("passphrase") {
        return Err(refuse(
            "ansible-playbook takes no passphrase for a private key; add the key to an SSH \
             agent and set agent: true instead"
                .to_owned(),
        ));
    }
    if credential.text("certificate").is_some() {
        return Err(refuse(
            "ansible-playbook takes no certificate for a key; put it beside the key in the \
             SSH agent and set agent: true"
                .to_owned(),
        ));
    }
    if credential.text("domain").is_some() {
        return Err(refuse(
            "ansible-playbook over SSH takes no domain; it is for WinRM hosts, which this \
             plugin does not reach"
                .to_owned(),
        ));
    }
    let mut vars = Vec::new();
    if let Some(user) = credential.text("user") {
        // A user is written after the host in inventory.ini, where a space
        // starts the next variable and a line break the next host.
        if user.is_empty()
            || user
                .chars()
                .any(|c| c.is_whitespace() || c == '=' || c == '[' || c == '#')
        {
            return Err(refuse(format!(
                "the user {user:?} cannot be written into the inventory: a user name holds \
                 no whitespace, `=`, `[` or `#`"
            )));
        }
        vars.push((crate::USER_VARIABLE.to_owned(), user.to_owned()));
    }
    Ok(vars)
}

/// Maps one resource onto its native module call, in the given state.
fn task_for(r: &Resource, state: &str) -> Result<Task, CompileError> {
    if let Some(module) = native_module(r)? {
        return native_task(r, module, state);
    }
    if r.r#type == "packageRepository" {
        return repository_task(r);
    }
    if r.r#type == "cron" {
        return cron_task(r);
    }
    let m = mapping(&r.r#type).ok_or_else(|| unsupported_type(r))?;
    let module = (m.module)(r);
    let allowed = (m.params)(module);

    let mut args = BTreeMap::new();
    for (name, value) in &r.params {
        let Some(native) = lookup(allowed, name) else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_PARAM,
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
                &rules::EMPTY_PARAM,
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
                &rules::MISSING_PARAM,
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
        native: false,
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// A `cron` job: `ansible.builtin.cron`.
fn cron_task(r: &Resource) -> Result<Task, CompileError> {
    let job = meridian_sdk::schema::cron_job(r).map_err(|e| CompileError {
        target: NAME.to_owned(),
        ..e
    })?;
    let text = |s: &str| Value::String(s.to_owned());
    let args = BTreeMap::from([
        ("name".to_owned(), text(&job.name)),
        ("job".to_owned(), text(&job.command)),
        ("user".to_owned(), text(&job.user)),
        ("minute".to_owned(), text(&job.minute)),
        ("hour".to_owned(), text(&job.hour)),
        ("day".to_owned(), text(&job.day)),
        ("month".to_owned(), text(&job.month)),
        ("weekday".to_owned(), text(&job.weekday)),
        (
            "state".to_owned(),
            text(if job.present { "present" } else { "absent" }),
        ),
    ]);
    Ok(Task {
        name: r.id.clone(),
        module: "ansible.builtin.cron".to_owned(),
        args,
        notify: Vec::new(),
        when: r.runtime_when.clone(),
        native: false,
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// A `packageRepository`: `deb822_repository` for apt, `yum_repository`
/// for yum.
fn repository_task(r: &Resource) -> Result<Task, CompileError> {
    use meridian_sdk::schema::RepositoryFormat;
    let repo = meridian_sdk::schema::package_repository(r).map_err(|e| CompileError {
        target: NAME.to_owned(),
        ..e
    })?;
    let text = |s: &str| Value::String(s.to_owned());
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), text(&repo.name));
    let state = if repo.present { "present" } else { "absent" };
    args.insert("state".to_owned(), text(state));
    let module = match &repo.format {
        RepositoryFormat::Apt {
            distribution,
            components,
        } => {
            args.insert("types".to_owned(), Value::List(vec![text("deb")]));
            args.insert("uris".to_owned(), text(&repo.url));
            args.insert("suites".to_owned(), text(distribution));
            args.insert(
                "components".to_owned(),
                Value::List(components.iter().map(|c| text(c)).collect()),
            );
            if let Some(key) = &repo.key {
                args.insert("signed_by".to_owned(), text(key));
            }
            "ansible.builtin.deb822_repository"
        }
        RepositoryFormat::Yum => {
            args.insert("description".to_owned(), text(&repo.name));
            args.insert("baseurl".to_owned(), text(&repo.url));
            args.insert("gpgcheck".to_owned(), Value::Bool(repo.key.is_some()));
            if let Some(key) = &repo.key {
                args.insert("gpgkey".to_owned(), text(key));
            }
            "ansible.builtin.yum_repository"
        }
    };
    Ok(Task {
        name: r.id.clone(),
        module: module.to_owned(),
        args,
        notify: Vec::new(),
        when: r.runtime_when.clone(),
        native: false,
        resource_id: r.id.clone(),
        pos: r.pos.clone(),
    })
}

/// A resource whose type is a module's own name: the params are the
/// module's arguments as written, and a state is the module's `state`
/// argument. Nothing is checked against a list of what the module takes;
/// an argument the module does not have is Ansible's to refuse when the
/// play runs, as a provider's is Terraform's, until the module's schema is
/// synced and the editor reports it in place.
fn native_task(r: &Resource, module: &str, state: &str) -> Result<Task, CompileError> {
    let mut args = BTreeMap::new();
    for (name, value) in &r.params {
        if *value == Value::Null {
            return Err(refuse(
                r,
                &rules::EMPTY_PARAM,
                format!("param {name:?} on module {module} has no value"),
            ));
        }
        args.insert(name.clone(), value.clone());
    }
    if !state.is_empty() {
        if r.params.contains_key("state") {
            return Err(refuse(
                r,
                &rules::PARAM_CONFLICTS_WITH_STATE,
                format!(
                    "state {state:?} and param \"state\" both set the module's state argument; \
                     write one of them"
                ),
            ));
        }
        args.insert("state".to_owned(), Value::String(state.to_owned()));
    }
    Ok(Task {
        name: r.id.clone(),
        module: module.to_owned(),
        args,
        notify: Vec::new(),
        when: r.runtime_when.clone(),
        native: true,
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
                &rules::UNSUPPORTED_STATE,
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
            &rules::UNSUPPORTED_STATE,
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
        // A file with content is a copy, and a copy has no state: the
        // content is what conflicts, so the refusal is the same one every
        // configuration management target gives a directory with content.
        None if module == COPY => {
            return Err(refuse(
                r,
                &rules::PARAM_CONFLICTS_WITH_STATE,
                format!(
                    "param \"content\" cannot be used with state {state:?}: a file with content \
                     is written as it is given, and only a file in state \"file\" or \"present\" \
                     has content"
                ),
            ));
        }
        None => {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
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
                &rules::UNKNOWN_NOTIFY_TARGET,
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
    // A handler is a task that runs when notified, so a module's own task
    // is run again as it stands. That is the one thing a notification can
    // ask of it: an action is a change of state, and what a state means is
    // the module's own business.
    if let Some(module) = native_module(r)? {
        if action != "run" {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "resource type {module:?} is a module, which can only be run again when \
                     notified (action \"run\"), got action {action:?}"
                ),
            ));
        }
        let mut task = native_task(r, module, &r.state)?;
        task.name = handler_name(action, &r.id);
        task.when = String::new();
        return Ok(task);
    }
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
        return Err(refuse(r, &rules::UNSUPPORTED_ACTION, msg));
    };

    let mut task = task_for(r, state)?;
    task.name = handler_name(action, &r.id);
    // A handler must not carry the notified task's own runtime condition: it
    // runs because something changed, not because the condition still holds.
    task.when = String::new();
    Ok(task)
}
