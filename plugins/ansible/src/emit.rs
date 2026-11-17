//! Stage four: a play becomes its final files.

use std::collections::BTreeMap;

use meridian_sdk::Value;
use meridian_yaml::Yaml;

use crate::{Playbook, Task};

/// The artifact's file names, shared with the runner.
pub const PLAYBOOK_PATH: &str = "playbook.yml";
pub const INVENTORY_PATH: &str = "inventory.ini";

/// Renders a playbook. This stage performs no mapping and no ordering:
/// everything it writes was decided by transform and sort, so the same play
/// always renders to the same bytes.
pub(crate) fn serialise(playbook: &Playbook) -> BTreeMap<String, String> {
    let mut play = vec![
        ("name".to_owned(), Yaml::str(&playbook.name)),
        ("hosts".to_owned(), Yaml::str(&playbook.hosts)),
        ("tasks".to_owned(), task_list(&playbook.tasks)),
    ];
    if !playbook.handlers.is_empty() {
        play.push(("handlers".to_owned(), task_list(&playbook.handlers)));
    }
    let document = Yaml::Seq(vec![Yaml::Map(play)]);

    BTreeMap::from([
        (
            PLAYBOOK_PATH.to_owned(),
            format!("---\n{}", meridian_yaml::write(&document)),
        ),
        // Meridian resolves the host itself, so the inventory is a literal
        // and not a lookup.
        (
            INVENTORY_PATH.to_owned(),
            format!("[meridian]\n{}\n", playbook.hosts),
        ),
    ])
}

fn task_list(tasks: &[Task]) -> Yaml {
    Yaml::Seq(tasks.iter().map(task_node).collect())
}

/// Lays a task out in a fixed key order: name, module, then the structural
/// keys. Key order is part of the output's identity, so it is decided here
/// once.
fn task_node(task: &Task) -> Yaml {
    let args = task
        .args
        .iter()
        .map(|(key, value)| (key.clone(), value_node(value)))
        .collect();
    let mut pairs = vec![
        ("name".to_owned(), Yaml::str(&task.name)),
        (task.module.clone(), Yaml::Map(args)),
    ];
    if !task.notify.is_empty() {
        pairs.push((
            "notify".to_owned(),
            Yaml::Seq(task.notify.iter().map(Yaml::str).collect()),
        ));
    }
    if !task.when.is_empty() {
        pairs.push(("when".to_owned(), Yaml::str(&task.when)));
    }
    // A task handed a secret is not allowed to print its arguments, its
    // diff or its result, however the run was asked for them.
    if task.args.values().any(holds_secret) {
        pairs.push(("no_log".to_owned(), Yaml::Bool(true)));
    }
    Yaml::Map(pairs)
}

/// Whether a value refers to a secret the runner hands over.
fn holds_secret(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains(crate::SECRET_VARIABLE_PREFIX),
        Value::List(items) => items.iter().any(holds_secret),
        Value::Map(map) => map.values().any(holds_secret),
        _ => false,
    }
}

/// Renders a param value. Map keys arrive sorted, so a value serialises
/// identically every time.
pub(crate) fn value_node(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(i) => Yaml::Int(*i),
        Value::Uint(u) => Yaml::Uint(*u),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::str(s),
        // A plugin is never sent a secret as a value; one would be written as its mask.
        Value::Secret(_) => Yaml::str(meridian_sdk::SENSITIVE),
        Value::List(items) => Yaml::Seq(items.iter().map(value_node).collect()),
        Value::Map(map) => Yaml::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), value_node(value)))
                .collect(),
        ),
    }
}
