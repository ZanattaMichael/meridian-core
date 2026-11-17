//! Importing an existing Ansible playbook.
//!
//! A playbook says far more than this target's six resource types do, so
//! this importer is the one that has the most to report. What it reads:
//!
//! - each play becomes a `ResourceSet`, and a file of tasks with no play
//!   around it becomes one with no host;
//! - a task that calls a module one of the resource types compiles to
//!   becomes that resource: the package modules a `package`, `service` and
//!   `systemd` a `service`, `file` and a `copy` with literal content a
//!   `file`, `user`, `group`, and `command` and `shell` an `exec`;
//! - `when` becomes `runtimeWhen`, and a `notify` whose handler restarts,
//!   reloads, starts or stops a service the play also manages becomes
//!   `notifies` on that service;
//! - a `loop` over a literal list becomes one resource for each item, a
//!   `block` becomes its tasks, and a play variable with a plain value is
//!   written in wherever a task uses it.
//!
//! Everything else is reported and marked, never dropped: a module with no
//! resource type, a role, an argument or a keyword the resource cannot carry.

use std::collections::{BTreeMap, BTreeSet};

use meridian_import::yaml::{self, Kind, Node, Rules};
use meridian_import::{literal, slug, unique_id, Notes};
use meridian_sdk::{
    DocumentKind, ImportError, ImportFormat, ImportSource, Imported, ImportedDocument,
    ImportedNotify, ImportedResource, Importer, Value,
};

use crate::NAME;

/// Reads playbooks into documents for this target.
pub struct AnsibleImporter;

impl Importer for AnsibleImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        vec![ImportFormat::new(
            "playbook",
            "Ansible playbooks and task files",
            &[
                "playbook.yml",
                "playbook.yaml",
                "site.yml",
                "site.yaml",
                "*playbook*.yml",
                "*playbook*.yaml",
                "*.yml",
                "*.yaml",
            ],
        )]
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        if source.files.is_empty() {
            return Err(ImportError::new("there is no playbook to import"));
        }
        // Every play of every file first, so a document's name can depend on
        // how many there are.
        let mut notes = Notes::new();
        let mut plays: Vec<Play> = Vec::new();
        let mut parsed = Vec::new();
        for (file, text) in &source.files {
            parsed.push((file, yaml::read(file, text, Rules::Yaml11)?));
        }
        for (file, documents) in &parsed {
            for document in documents {
                read_document(file, document, &mut plays, &mut notes);
            }
        }
        if plays.is_empty() {
            return Err(ImportError::new(
                "there is no playbook to import: a playbook is a list of plays, each with \
                 `hosts`, and a task file is a list of tasks",
            ));
        }

        let base = if source.name.is_empty() {
            None
        } else {
            Some(source.name.as_str())
        };
        let several = plays.len() > 1;
        let mut names = BTreeSet::new();
        let mut documents = Vec::new();
        for play in &plays {
            let own = slug(play.name.as_deref().unwrap_or("play"), '-');
            let name = match (base, several) {
                (Some(base), false) => base.to_owned(),
                (Some(base), true) => format!("{base}-{own}"),
                (None, _) if play.name.is_some() => own,
                (None, _) => "imported".to_owned(),
            };
            let name = unique_name(&name, &mut names);
            notes.in_document(&name);
            documents.push(import_play(play, name, &mut notes));
        }

        Ok(Imported {
            documents,
            notes: notes.into_vec(),
        })
    }
}

fn unique_name(base: &str, taken: &mut BTreeSet<String>) -> String {
    if taken.insert(base.to_owned()) {
        return base.to_owned();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// One play, or a list of tasks standing in for one.
struct Play<'a> {
    file: &'a str,
    line: usize,
    name: Option<String>,
    /// `None` for a task file, which names no host.
    hosts: Option<&'a Node>,
    /// The play's own keys other than its task lists, in the order written.
    keywords: Vec<(&'a str, &'a Node)>,
    tasks: Vec<&'a Node>,
    handlers: Vec<&'a Node>,
    vars: Option<&'a Node>,
}

/// Sorts one YAML document into plays, a task file, or neither.
fn read_document<'a>(
    file: &'a str,
    document: &'a Node,
    plays: &mut Vec<Play<'a>>,
    notes: &mut Notes,
) {
    let origin = format!("{file}:{}", document.line);
    let Some(items) = document.as_seq() else {
        notes.untranslated(
            &origin,
            "file",
            "",
            format!(
                "a playbook is a list of plays, and this is {}; it was left out. Variable \
                 files and inventories are not imported",
                document.describe()
            ),
        );
        return;
    };
    let is_play =
        |item: &Node| item.get("hosts").is_some() || item.get("import_playbook").is_some();
    if !items.iter().any(is_play) {
        // A list of tasks with no play around it, as a role's tasks file is.
        if items.iter().all(|item| item.as_map().is_some()) && !items.is_empty() {
            plays.push(Play {
                file,
                line: document.line,
                name: None,
                hosts: None,
                keywords: Vec::new(),
                tasks: items.iter().collect(),
                handlers: Vec::new(),
                vars: None,
            });
        } else {
            notes.untranslated(
                &origin,
                "file",
                "",
                "this is neither a list of plays nor a list of tasks; it was left out",
            );
        }
        return;
    }
    for item in items {
        let origin = format!("{file}:{}", item.line);
        if let Some(other) = item.get("import_playbook") {
            notes.untranslated(
                &origin,
                "import_playbook",
                "",
                format!(
                    "the playbook {} is imported by name and was not read; import it as well",
                    other.scalar_text().unwrap_or_default()
                ),
            );
            continue;
        }
        let Some(entries) = item.as_map() else {
            notes.untranslated(
                &origin,
                "play",
                "",
                "a play is a mapping; this was left out",
            );
            continue;
        };
        let mut play = Play {
            file,
            line: item.line,
            name: item.get("name").and_then(Node::scalar_text),
            hosts: item.get("hosts"),
            keywords: Vec::new(),
            tasks: Vec::new(),
            handlers: Vec::new(),
            vars: item.get("vars"),
        };
        // Ansible runs a play's lists in this order whatever order they are
        // written in.
        for list in ["pre_tasks", "tasks", "post_tasks"] {
            if let Some(tasks) = item.get(list).and_then(Node::as_seq) {
                play.tasks.extend(tasks);
            }
        }
        if let Some(handlers) = item.get("handlers").and_then(Node::as_seq) {
            play.handlers.extend(handlers);
        }
        for (key, value) in entries {
            if !matches!(
                key.as_str(),
                "name" | "hosts" | "pre_tasks" | "tasks" | "post_tasks" | "handlers" | "vars"
            ) {
                play.keywords.push((key, value));
            }
        }
        plays.push(play);
    }
}

/// The keys of a task that are not its module.
const TASK_KEYWORDS: &[&str] = &[
    "action",
    "always",
    "any_errors_fatal",
    "args",
    "async",
    "become",
    "become_exe",
    "become_flags",
    "become_method",
    "become_user",
    "block",
    "changed_when",
    "check_mode",
    "collections",
    "connection",
    "debugger",
    "delay",
    "delegate_facts",
    "delegate_to",
    "diff",
    "environment",
    "failed_when",
    "ignore_errors",
    "ignore_unreachable",
    "listen",
    "local_action",
    "loop",
    "loop_control",
    "module_defaults",
    "name",
    "no_log",
    "notify",
    "poll",
    "port",
    "register",
    "remote_user",
    "rescue",
    "retries",
    "run_once",
    "tags",
    "throttle",
    "timeout",
    "until",
    "vars",
    "when",
];

/// The task keywords a resource carries; every other one is reported.
const CARRIED: &[&str] = &["args", "block", "loop", "name", "notify", "when", "listen"];

/// A task's module and its arguments.
fn module_of(task: &Node) -> Option<(&str, &Node)> {
    task.as_map()?
        .iter()
        .find(|(key, _)| !TASK_KEYWORDS.contains(&key.as_str()) && !key.starts_with("with_"))
        .map(|(key, value)| (key.as_str(), value))
}

/// A module's name without the collection it is addressed through.
fn short(module: &str) -> &str {
    module
        .strip_prefix("ansible.builtin.")
        .or_else(|| module.strip_prefix("ansible.legacy."))
        .or_else(|| module.strip_prefix("ansible.posix."))
        .or_else(|| module.strip_prefix("community.general."))
        .unwrap_or(module)
}

const PACKAGE_MODULES: &[&str] = &[
    "apk", "apt", "dnf", "dnf5", "package", "pacman", "yum", "zypper",
];
const SERVICE_MODULES: &[&str] = &["service", "systemd", "systemd_service"];

/// What a handler does, when it is something `notifies` can say.
struct Handler {
    name: String,
    listen: Vec<String>,
    origin: String,
    /// The service it acts on and the action, or why it cannot be notified.
    does: Result<(Value, &'static str), String>,
    notified: bool,
}

struct Importing<'a, 'n> {
    file: &'a str,
    notes: &'n mut Notes,
    vars: BTreeMap<String, Value>,
    /// Play variables whose value could not be written in.
    unusable_vars: BTreeSet<String>,
    taken: BTreeSet<String>,
    resources: Vec<ImportedResource>,
    /// For each resource, the handler names its task notified, with where.
    notify: Vec<(usize, String, String)>,
}

fn import_play(play: &Play, name: String, notes: &mut Notes) -> ImportedDocument {
    let origin = format!("{}:{}", play.file, play.line);
    let subject = match &play.name {
        Some(name) => format!("play {name:?}"),
        None => "tasks".to_owned(),
    };

    let host = play_host(play, &origin, &subject, notes);

    let mut lost = Vec::new();
    for (key, value) in &play.keywords {
        match *key {
            "roles" => notes.untranslated(
                &origin,
                &subject,
                "",
                format!(
                    "the play applies the role(s) {}, and a role's tasks live in other files. \
                     Import each role's tasks file and add its resources here",
                    role_names(value).join(", ")
                ),
            ),
            "gather_facts" => notes.info(
                &origin,
                &subject,
                "",
                "gather_facts is not carried: the compiled play leaves it to Ansible's \
                 default, which gathers facts",
            ),
            other => lost.push(other),
        }
    }
    if !lost.is_empty() {
        notes.untranslated(
            &origin,
            &subject,
            "",
            format!(
                "a document has nothing for these play keywords, so they were left out: {}. \
                 Connection settings such as become and remote_user belong in the runner's \
                 MERIDIAN_ANSIBLE_ARGS",
                lost.join(", ")
            ),
        );
    }

    let mut importing = Importing {
        file: play.file,
        notes,
        vars: BTreeMap::new(),
        unusable_vars: BTreeSet::new(),
        taken: BTreeSet::new(),
        resources: Vec::new(),
        notify: Vec::new(),
    };
    if let Some(vars) = play.vars {
        importing.read_vars(vars, &origin, &subject);
    }
    for task in &play.tasks {
        importing.task(task, &[], &BTreeMap::new());
    }
    importing.handlers(&play.handlers);

    ImportedDocument {
        kind: DocumentKind::ResourceSet,
        name,
        target: NAME.to_owned(),
        host,
        resources: importing.resources,
        ..ImportedDocument::default()
    }
}

fn role_names(roles: &Node) -> Vec<String> {
    roles
        .as_seq()
        .unwrap_or_default()
        .iter()
        .filter_map(|role| {
            role.scalar_text().or_else(|| {
                role.get("role")
                    .or_else(|| role.get("name"))
                    .and_then(Node::scalar_text)
            })
        })
        .collect()
}

/// The one host a play configures, when `hosts` names one.
fn play_host(play: &Play, origin: &str, subject: &str, notes: &mut Notes) -> String {
    let Some(hosts) = play.hosts else {
        return String::new();
    };
    let text = match &hosts.kind {
        Kind::Seq(items) if items.len() == 1 => items[0].scalar_text().unwrap_or_default(),
        Kind::Seq(items) => items
            .iter()
            .filter_map(Node::scalar_text)
            .collect::<Vec<_>>()
            .join(","),
        _ => hosts.scalar_text().unwrap_or_default(),
    };
    let is_pattern = text.is_empty()
        || text == "all"
        || text == "ungrouped"
        || text.contains([',', ':', '*', '!', '&', '[', '{', ' ']);
    if is_pattern {
        notes.untranslated(
            origin,
            subject,
            "",
            format!(
                "hosts is {text:?}, which selects hosts from an inventory, and a document \
                 configures one host that it names. Write one document for each host, or take \
                 the host from data"
            ),
        );
        return String::new();
    }
    let certainly_a_host =
        text == "localhost" || text.contains('.') || text.parse::<std::net::IpAddr>().is_ok();
    if !certainly_a_host {
        notes.changed(
            origin,
            subject,
            "",
            format!(
                "hosts is {text:?}, which was taken as the name of a host. If it is an \
                 inventory group, write one document for each host in it"
            ),
        );
    }
    text
}

impl Importing<'_, '_> {
    fn read_vars(&mut self, vars: &Node, origin: &str, subject: &str) {
        let Some(entries) = vars.as_map() else {
            return;
        };
        for (name, value) in entries {
            let plain = match &value.kind {
                Kind::Seq(_) | Kind::Map(_) | Kind::Null => false,
                Kind::Str(s) => !s.contains("{{") && !s.contains("{%"),
                _ => true,
            };
            if plain {
                self.vars.insert(name.clone(), value.value());
            } else {
                self.unusable_vars.insert(name.clone());
            }
        }
        if !self.vars.is_empty() {
            self.notes.info(
                origin,
                subject,
                "",
                format!(
                    "a document has no play variables, so the value of each of these was \
                     written in wherever a task uses it: {}",
                    self.vars.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            );
        }
        if !self.unusable_vars.is_empty() {
            self.notes.untranslated(
                origin,
                subject,
                "",
                format!(
                    "these play variables are lists, mappings or templates, and a document \
                     has no play variables to hold them: {}. A task that uses one is marked",
                    self.unusable_vars
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
    }

    /// Imports one entry of a task list. `conditions` are the `when`s of the
    /// blocks around it, and `item` is the loop variable when there is one.
    fn task(&mut self, task: &Node, conditions: &[String], item: &BTreeMap<String, Value>) {
        let origin = format!("{}:{}", self.file, task.line);
        let Some(entries) = task.as_map() else {
            self.notes.untranslated(
                &origin,
                "task",
                "",
                "a task is a mapping; this was left out",
            );
            return;
        };
        let task_name = task.get("name").and_then(Node::scalar_text);
        let subject = match &task_name {
            Some(name) => format!("task {name:?}"),
            None => "task".to_owned(),
        };

        let mut conditions = conditions.to_vec();
        if let Some(when) = task.get("when") {
            conditions.extend(when_text(when));
        }

        // What neither a resource nor a block can carry.
        let mut lost: Vec<&str> = entries
            .iter()
            .map(|(key, _)| key.as_str())
            .filter(|key| {
                (TASK_KEYWORDS.contains(key) && !CARRIED.contains(key)) || key.starts_with("with_")
            })
            .filter(|key| *key != "with_items")
            .collect();

        if let Some(block) = task.get("block") {
            for part in ["rescue", "always"] {
                if task.get(part).is_some() {
                    self.notes.untranslated(
                        &origin,
                        &subject,
                        "",
                        format!(
                            "the block's `{part}` tasks were left out: a document has no way \
                             to run something only when something else failed or finished"
                        ),
                    );
                }
            }
            lost.retain(|key| !matches!(*key, "rescue" | "always"));
            if !lost.is_empty() {
                self.notes.untranslated(
                    &origin,
                    &subject,
                    "",
                    format!(
                        "the block's tasks were imported without these keywords, which a \
                         resource cannot carry: {}",
                        lost.join(", ")
                    ),
                );
            }
            for child in block.as_seq().unwrap_or_default() {
                self.task(child, &conditions, item);
            }
            return;
        }

        // A loop over a list written out in the playbook is one resource for
        // each item.
        if item.is_empty() {
            if let Some(over) = task.get("loop").or_else(|| task.get("with_items")) {
                let items: Option<Vec<Value>> = over.as_seq().and_then(|items| {
                    items
                        .iter()
                        .map(|i| match &i.kind {
                            Kind::Seq(_) | Kind::Map(_) | Kind::Null => None,
                            Kind::Str(s) if s.contains("{{") => None,
                            _ => Some(i.value()),
                        })
                        .collect()
                });
                let Some(items) = items else {
                    self.notes.untranslated(
                        &origin,
                        &subject,
                        "",
                        "the task loops over something other than a list of plain values \
                         written in the playbook, so it was left out. Write one resource for \
                         each item",
                    );
                    return;
                };
                for value in items {
                    let item = BTreeMap::from([("item".to_owned(), value)]);
                    self.task(
                        task,
                        &conditions[..conditions.len() - when_count(task)],
                        &item,
                    );
                }
                return;
            }
        }

        let Some((module, args)) = module_of(task) else {
            let what = if task.get("action").is_some() || task.get("local_action").is_some() {
                "the task names its module through `action`, which is not read; write the \
                 module as the task's key"
            } else {
                "the task calls no module; it was left out"
            };
            self.notes.untranslated(&origin, &subject, "", what);
            return;
        };

        if matches!(
            short(module),
            "include" | "include_tasks" | "import_tasks" | "include_role" | "import_role"
        ) {
            let what = args
                .scalar_text()
                .or_else(|| {
                    ["file", "name"]
                        .iter()
                        .find_map(|key| args.get(key).and_then(Node::scalar_text))
                })
                .unwrap_or_default();
            self.notes.untranslated(
                &origin,
                &subject,
                "",
                format!(
                    "{module} brings in {what:?} by name, and it was not read. Import that \
                     file as well and add its resources here"
                ),
            );
            return;
        }

        let mut call = match self.arguments(module, args, task.get("args")) {
            Ok(call) => call,
            Err(why) => {
                self.notes.untranslated(&origin, &subject, "", why);
                return;
            }
        };
        call.substitute(item);
        call.substitute(&self.vars);
        let mapped = match map_module(short(module), &mut call) {
            Ok(mapped) => mapped,
            Err(why) => {
                self.notes.untranslated(&origin, &subject, "", why);
                return;
            }
        };

        let base = match (&task_name, item.get("item")) {
            (Some(name), None) => slug(name, '_'),
            (Some(name), Some(value)) => slug(&format!("{name} {}", text_of(value)), '_'),
            (None, _) => slug(&format!("{} {}", mapped.r#type, mapped.label), '_'),
        };
        let id = unique_id(&base, &mut self.taken);

        // Play variables: written in where the value is plain, and reported
        // where it is not or where an expression computes with one.
        let mut params = BTreeMap::new();
        let mut used = BTreeSet::new();
        for (key, value) in mapped.params {
            mentions(&value, &self.vars, &self.unusable_vars, &mut used);
            params.insert(key, literal(&value));
        }
        let runtime_when = match conditions.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            many => many
                .iter()
                .map(|c| format!("({c})"))
                .collect::<Vec<_>>()
                .join(" and "),
        };
        for name in self.vars.keys().chain(&self.unusable_vars) {
            if has_word(&runtime_when, name) {
                used.insert(name.clone());
            }
        }
        if !used.is_empty() {
            self.notes.untranslated(
                &origin,
                &subject,
                &id,
                format!(
                    "this uses the play variable(s) {} in a way that could not be written in. \
                     A document has no play variables, so Ansible will find them undefined: \
                     write the value where it is used",
                    used.into_iter().collect::<Vec<_>>().join(", ")
                ),
            );
        }

        if !call.rest.is_empty() {
            self.notes.untranslated(
                &origin,
                &subject,
                &id,
                format!(
                    "the {} resource type has no param for these arguments of {module}, so \
                     they were left out: {}",
                    mapped.r#type,
                    call.rest.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            );
        }
        if !lost.is_empty() {
            self.notes.untranslated(
                &origin,
                &subject,
                &id,
                format!(
                    "a resource cannot carry these task keywords, so they were left out: {}",
                    lost.join(", ")
                ),
            );
        }
        for note in mapped.changed {
            self.notes.changed(&origin, &subject, &id, note);
        }

        let index = self.resources.len();
        if let Some(notify) = task.get("notify") {
            let names: Vec<String> = match &notify.kind {
                Kind::Seq(items) => items.iter().filter_map(Node::scalar_text).collect(),
                _ => notify.scalar_text().into_iter().collect(),
            };
            for name in names {
                self.notify.push((index, name, origin.clone()));
            }
        }
        self.resources.push(ImportedResource {
            id,
            r#type: mapped.r#type.to_owned(),
            state: mapped.state,
            params,
            runtime_when: runtime_when.replace("${", "$${"),
            origin,
            ..ImportedResource::default()
        });
    }

    /// A module's arguments as a mapping, whichever way the task wrote them.
    fn arguments(&self, module: &str, args: &Node, extra: Option<&Node>) -> Result<Call, String> {
        let mut call = Call::default();
        match &args.kind {
            Kind::Map(entries) => {
                for (key, value) in entries {
                    call.rest.insert(key.clone(), value.value());
                }
            }
            Kind::Null => {}
            Kind::Str(text) if matches!(short(module), "command" | "shell" | "raw" | "script") => {
                call.free_form = Some(text.clone());
            }
            Kind::Str(text) => {
                // The old `key=value` form, where it is simple enough to read
                // without Ansible's own splitter.
                if text.contains(['"', '\'']) || text.contains("{{") {
                    return Err(format!(
                        "the arguments of {module} are written as one `key=value` string with \
                         quoting or a template in it, which is not read. Write them as a \
                         mapping and import again"
                    ));
                }
                for word in text.split_whitespace() {
                    let Some((key, value)) = word.split_once('=') else {
                        return Err(format!(
                            "the arguments of {module} are a string that is not `key=value` \
                             pairs; the task was left out"
                        ));
                    };
                    call.rest
                        .insert(key.to_owned(), Value::String(value.to_owned()));
                }
            }
            _ => {
                return Err(format!(
                    "the arguments of {module} are {}, not a mapping; the task was left out",
                    args.describe()
                ))
            }
        }
        if let Some(entries) = extra.and_then(Node::as_map) {
            for (key, value) in entries {
                call.rest
                    .entry(key.clone())
                    .or_insert_with(|| value.value());
            }
        }
        Ok(call)
    }

    /// Turns each `notify` into `notifies` where the handler is something a
    /// resource of this play can be told to do.
    fn handlers(&mut self, handlers: &[&Node]) {
        let mut known: Vec<Handler> = handlers.iter().map(|h| self.handler(h)).collect();
        let notify = std::mem::take(&mut self.notify);
        for (index, wanted, origin) in notify {
            let id = self.resources[index].id.clone();
            let subject = format!("notify {wanted:?}");
            let mut found = false;
            for handler in known
                .iter_mut()
                .filter(|h| h.name == wanted || h.listen.contains(&wanted))
            {
                found = true;
                handler.notified = true;
                let (service, action) = match &handler.does {
                    Ok(does) => does,
                    Err(why) => {
                        self.notes.untranslated(
                            &origin,
                            &subject,
                            &id,
                            format!("the handler {:?} ({}) {why}", handler.name, handler.origin),
                        );
                        continue;
                    }
                };
                let target = self
                    .resources
                    .iter()
                    .find(|r| r.r#type == "service" && r.params.get("name") == Some(service))
                    .map(|r| r.id.clone());
                let Some(target) = target else {
                    self.notes.untranslated(
                        &origin,
                        &subject,
                        &id,
                        format!(
                            "the handler {:?} ({}) would {action} the service {}, and no task \
                             of the play manages that service. In a document a notification \
                             goes to a resource: add a service resource for it and notify that",
                            handler.name,
                            handler.origin,
                            text_of(service)
                        ),
                    );
                    continue;
                };
                let notifies = ImportedNotify {
                    resource: target,
                    action: (*action).to_owned(),
                };
                let resource = &mut self.resources[index];
                if resource.id != notifies.resource && !resource.notifies.contains(&notifies) {
                    resource.notifies.push(notifies);
                }
            }
            if !found {
                self.notes.untranslated(
                    &origin,
                    &subject,
                    &id,
                    "no handler of the play has that name or listens for it, so the \
                     notification was left out",
                );
            }
        }
        for handler in known.iter().filter(|h| !h.notified) {
            self.notes.info(
                &handler.origin,
                &format!("handler {:?}", handler.name),
                "",
                "no imported task notifies this handler, so it was left out",
            );
        }
    }

    fn handler(&self, node: &Node) -> Handler {
        let name = node
            .get("name")
            .and_then(Node::scalar_text)
            .unwrap_or_default();
        let listen = match node.get("listen").map(|l| &l.kind) {
            Some(Kind::Seq(items)) => items.iter().filter_map(Node::scalar_text).collect(),
            Some(_) => node
                .get("listen")
                .and_then(Node::scalar_text)
                .into_iter()
                .collect(),
            None => Vec::new(),
        };
        let does = (|| {
            let (module, args) = module_of(node).ok_or("calls no module".to_owned())?;
            if !SERVICE_MODULES.contains(&short(module)) {
                return Err(format!(
                    "calls {module}, and a notification can only restart, reload, start or \
                     stop a service. Write what it does as a resource of its own"
                ));
            }
            let call = self.arguments(module, args, node.get("args"))?;
            let service = call
                .rest
                .get("name")
                .map(|name| substitute(name, &self.vars))
                .ok_or("names no service".to_owned())?;
            let action = match call.rest.get("state").and_then(Value::as_str) {
                Some("restarted") => "restart",
                Some("reloaded") => "reload",
                Some("started") => "start",
                Some("stopped") => "stop",
                _ => {
                    return Err(
                        "does not restart, reload, start or stop its service, which is all a \
                         notification can ask for"
                            .to_owned(),
                    )
                }
            };
            Ok((service, action))
        })();
        Handler {
            name,
            listen,
            origin: format!("{}:{}", self.file, node.line),
            does,
            notified: false,
        }
    }
}

/// How many conditions a task's own `when` adds.
fn when_count(task: &Node) -> usize {
    task.get("when").map_or(0, |when| when_text(when).len())
}

/// A `when` as the expressions it holds: a list of them all have to hold.
fn when_text(when: &Node) -> Vec<String> {
    match &when.kind {
        Kind::Seq(items) => items.iter().filter_map(Node::scalar_text).collect(),
        _ => when.scalar_text().into_iter().collect(),
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.canonical(),
    }
}

/// A module call being mapped: the arguments not yet claimed by a param.
#[derive(Default)]
struct Call {
    rest: BTreeMap<String, Value>,
    /// The command of a `command` or `shell` task written as a plain string.
    free_form: Option<String>,
}

impl Call {
    fn take(&mut self, names: &[&str]) -> Option<Value> {
        names.iter().find_map(|name| self.rest.remove(*name))
    }

    fn substitute(&mut self, vars: &BTreeMap<String, Value>) {
        if vars.is_empty() {
            return;
        }
        for value in self.rest.values_mut() {
            *value = substitute(value, vars);
        }
        if let Some(text) = &self.free_form {
            self.free_form = Some(text_of(&substitute(&Value::String(text.clone()), vars)));
        }
    }
}

/// One task as the resource it becomes.
struct Mapped {
    r#type: &'static str,
    state: String,
    params: BTreeMap<String, Value>,
    /// What an id is made from when the task has no name.
    label: String,
    changed: Vec<String>,
}

fn state_of(call: &mut Call) -> Result<Option<String>, String> {
    match call.rest.remove("state") {
        None => Ok(None),
        Some(Value::String(s)) if !s.contains("{{") => Ok(Some(s)),
        Some(other) => Err(format!(
            "its state is {}, which is not one fixed word; the task was left out",
            other.canonical()
        )),
    }
}

fn carry(params: &mut BTreeMap<String, Value>, call: &mut Call, param: &str, names: &[&str]) {
    if let Some(value) = call.take(names) {
        params.insert(param.to_owned(), value);
    }
}

/// Maps one module call onto a resource type, taking from `call` every
/// argument a param carries. What is left in `call` was not carried.
fn map_module(module: &str, call: &mut Call) -> Result<Mapped, String> {
    let mut params = BTreeMap::new();
    let mut changed = Vec::new();

    let (r#type, state) = if PACKAGE_MODULES.contains(&module) {
        carry(&mut params, call, "name", &["name", "pkg", "package"]);
        let given = state_of(call)?;
        let state = match given.as_deref() {
            None | Some("present" | "installed") => "present",
            Some("absent" | "removed") => "absent",
            Some("latest") => "latest",
            Some(other) => {
                return Err(format!(
                    "the package resource type has no state for {module}'s {other:?}; the \
                     task was left out"
                ))
            }
        };
        ("package", state.to_owned())
    } else if SERVICE_MODULES.contains(&module) {
        carry(&mut params, call, "name", &["name", "service", "unit"]);
        carry(&mut params, call, "enabled", &["enabled"]);
        let given = state_of(call)?;
        let state = match given.as_deref() {
            Some("started") => "running",
            Some(state @ ("stopped" | "restarted" | "reloaded")) => state,
            None => {
                changed.push(
                    "the task sets no state, only whether the service starts at boot, and a \
                     service resource always has one. It was given `running`, so applying \
                     the document starts the service"
                        .to_owned(),
                );
                "running"
            }
            Some(other) => {
                return Err(format!(
                    "the service resource type has no state for {module}'s {other:?}; the \
                     task was left out"
                ))
            }
        };
        ("service", state.to_owned())
    } else if module == "file" {
        carry(&mut params, call, "path", &["path", "dest", "name"]);
        let given = state_of(call)?;
        let state = match given.as_deref() {
            None | Some("file") => "file",
            Some(state @ ("directory" | "absent")) => state,
            Some(other) => {
                return Err(format!(
                    "the file resource type has no state for the file module's {other:?} \
                     (links and `touch` are not carried); the task was left out"
                ))
            }
        };
        for name in ["owner", "group", "mode"] {
            carry(&mut params, call, name, &[name]);
        }
        ("file", state.to_owned())
    } else if module == "copy" {
        if !call.rest.contains_key("content") {
            return Err(
                "the task copies a file from the machine Ansible runs on, and a document \
                 carries a file's content, not a path to it. Write the content into a file \
                 resource"
                    .to_owned(),
            );
        }
        carry(&mut params, call, "path", &["dest"]);
        for name in ["content", "owner", "group", "mode"] {
            carry(&mut params, call, name, &[name]);
        }
        ("file", "file".to_owned())
    } else if module == "user" {
        carry(&mut params, call, "name", &["name", "user"]);
        for name in ["uid", "groups", "home", "shell", "system"] {
            carry(&mut params, call, name, &[name]);
        }
        let state = state_of(call)?.unwrap_or_else(|| "present".to_owned());
        ("user", state)
    } else if module == "group" {
        carry(&mut params, call, "name", &["name"]);
        for name in ["gid", "system"] {
            carry(&mut params, call, name, &[name]);
        }
        let state = state_of(call)?.unwrap_or_else(|| "present".to_owned());
        ("group", state)
    } else if matches!(module, "command" | "shell") {
        match call.free_form.take() {
            Some(command) => {
                params.insert("command".to_owned(), Value::String(command));
            }
            None => carry(&mut params, call, "command", &["cmd"]),
        }
        if module == "shell" {
            params.insert("shell".to_owned(), Value::Bool(true));
        }
        for name in ["chdir", "creates", "removes"] {
            carry(&mut params, call, name, &[name]);
        }
        ("exec", String::new())
    } else {
        return Err(format!(
            "no resource type of this target compiles to the module {module}, so the task \
             was left out (the types are {})",
            crate::supported_resource_types().join(", ")
        ));
    };
    if matches!(r#type, "user" | "group") && !matches!(state.as_str(), "present" | "absent") {
        return Err(format!(
            "the {type} resource type has no state {state:?}; the task was left out"
        ));
    }

    // The one param each type cannot do without.
    let key = match r#type {
        "file" => "path",
        "exec" => "command",
        _ => "name",
    };
    let Some(main) = params.get(key) else {
        return Err(format!(
            "the task gives {module} no {key}, which the {type} resource type needs; it was \
             left out"
        ));
    };
    let label = match main {
        Value::List(items) => items.iter().map(text_of).collect::<Vec<_>>().join(" "),
        other => text_of(other),
    };

    // A mode Ansible read as a number is the number's octal digits.
    if let Some(mode) = params.get_mut("mode") {
        let number = match mode {
            Value::Int(i) => u64::try_from(*i).ok(),
            Value::Uint(u) => Some(*u),
            _ => None,
        };
        if let Some(number) = number {
            *mode = Value::String(format!("{number:04o}"));
        }
    }
    // Groups written as one comma-separated string are a list.
    if let Some(Value::String(groups)) = params.get("groups") {
        if !groups.contains("{{") {
            let list = groups
                .split(',')
                .map(|g| Value::String(g.trim().to_owned()))
                .collect();
            params.insert("groups".to_owned(), Value::List(list));
        }
    }

    Ok(Mapped {
        r#type,
        state,
        params,
        label,
        changed,
    })
}

/// Writes the value of each variable in `vars` in place of its `{{ name }}`,
/// at any depth.
fn substitute(value: &Value, vars: &BTreeMap<String, Value>) -> Value {
    match value {
        Value::String(text) => substitute_text(text, vars),
        Value::List(items) => Value::List(items.iter().map(|i| substitute(i, vars)).collect()),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), substitute(value, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn substitute_text(text: &str, vars: &BTreeMap<String, Value>) -> Value {
    let mut out = String::new();
    let mut rest = text;
    let mut whole = None;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        let expression = rest[start + 2..start + len].trim();
        match vars.get(expression) {
            Some(value) => {
                if start == 0 && start + len + 2 == text.len() {
                    // The whole value is the variable, so it keeps its type.
                    whole = Some(value.clone());
                }
                out.push_str(&rest[..start]);
                out.push_str(&text_of(value));
            }
            None => out.push_str(&rest[..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    whole.unwrap_or(Value::String(out))
}

/// Collects the play variables an expression still left in a value uses.
fn mentions(
    value: &Value,
    vars: &BTreeMap<String, Value>,
    unusable: &BTreeSet<String>,
    used: &mut BTreeSet<String>,
) {
    match value {
        Value::String(text) => {
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{{") {
                let end = rest[start..]
                    .find("}}")
                    .map_or(rest.len(), |len| start + len);
                let expression = &rest[start + 2..end];
                for name in vars.keys().chain(unusable) {
                    if has_word(expression, name) {
                        used.insert(name.clone());
                    }
                }
                rest = &rest[end..];
                rest = rest.strip_prefix("}}").unwrap_or("");
            }
        }
        Value::List(items) => items.iter().for_each(|i| mentions(i, vars, unusable, used)),
        Value::Map(map) => map.values().for_each(|v| mentions(v, vars, unusable, used)),
        _ => {}
    }
}

/// Whether `word` appears in `text` as an identifier of its own, and not as
/// part of a longer one, an attribute or a quoted string's content.
fn has_word(text: &str, word: &str) -> bool {
    let is_part = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(|c| is_part(c) || c == '.' || c == '\'' || c == '"')
            && !after.is_some_and(is_part)
        {
            return true;
        }
        from = end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("port".to_owned(), Value::Int(8080)),
            ("user".to_owned(), Value::String("deploy".to_owned())),
        ])
    }

    #[test]
    fn a_variable_alone_keeps_its_type_and_one_in_text_becomes_text() {
        let v = vars();
        assert_eq!(substitute_text("{{ port }}", &v), Value::Int(8080));
        assert_eq!(
            substitute_text("/home/{{user}}/app:{{ port }}", &v),
            Value::String("/home/deploy/app:8080".to_owned())
        );
        // Anything that is not exactly a known variable is left as written.
        assert_eq!(
            substitute_text("{{ port | int }} {{ ansible_hostname }} {{ open", &v),
            Value::String("{{ port | int }} {{ ansible_hostname }} {{ open".to_owned())
        );
    }

    #[test]
    fn a_word_is_found_only_as_an_identifier_of_its_own() {
        assert!(has_word("port | int", "port"));
        assert!(has_word("x == port", "port"));
        assert!(!has_word("ansible_port", "port"));
        assert!(!has_word("item.port", "port"));
        assert!(!has_word("ports", "port"));
        assert!(!has_word("x == 'port'", "port"));
    }
}
