//! Every target against trees it should never be handed.
//!
//! The core checks a document before a plugin sees it, but a plugin is a
//! separate program and must not trust that. Each example's tree is damaged
//! here in the ways a careless or hostile host could damage it, and every
//! target is held to one rule: the answer is files or a refusal that says
//! which rule was broken. Never a panic, and never a refusal with nothing in
//! it.

#![allow(clippy::result_large_err)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};

use meridian_plugin_testkit::{graph_from_file, repo_root};
use meridian_sdk::{Edge, EdgeKind, OutputDeclaration, Position, Resource, ResourceGraph, Value};

use support::{emitter, no_data};

/// Every example that holds one document, with the target it names.
fn examples() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let root = repo_root();
    for directory in fs::read_dir(root.join("examples")).unwrap() {
        let directory = directory.unwrap().path();
        if !directory.is_dir() {
            continue;
        }
        for file in fs::read_dir(&directory).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_none_or(|e| e != "yaml") {
                continue;
            }
            let text = fs::read_to_string(&file).unwrap();
            let targets: Vec<&str> = text
                .lines()
                .filter_map(|line| line.strip_prefix("  target: "))
                .collect();
            // A document that decides by `when` cannot become a tree
            // without its data; `examples.rs` compiles those.
            if text
                .lines()
                .any(|line| line.trim_start().starts_with("when:"))
            {
                continue;
            }
            // Nor can one that interpolates outside its comments: its
            // values are worked out before any target sees it. An escaped
            // opening, written with two dollars, is not an interpolation.
            let (open, escaped) = (["$", "{{"].concat(), ["$$", "{{"].concat());
            if text.lines().any(|line| {
                !line.trim_start().starts_with('#') && line.replace(&escaped, "").contains(&open)
            }) {
                continue;
            }
            if let [target] = targets[..] {
                let path = file.strip_prefix(&root).unwrap().to_str().unwrap();
                found.push((path.to_owned(), target.trim().to_owned()));
            }
        }
    }
    found.sort();
    found
}

const AWKWARD: &[&str] = &[
    "",
    " ",
    "\n",
    "a\nb",
    "a\r\nb",
    "\t",
    "'",
    "\"",
    "`",
    "\\",
    "$",
    "${",
    "${}",
    "${.}",
    "${a",
    "$${a}",
    "${a.b.c.d[0]['x']}",
    "{{ x }}",
    "{% x %}",
    "#{x}",
    "%{x}",
    "<%= x %>",
    "$(x)",
    "; rm -rf /",
    "../../etc/passwd",
    "/",
    "a/b",
    "a b",
    "A",
    "0",
    "-",
    "--help",
    "\u{0}",
    "\u{7f}",
    "\u{feff}",
    "\u{202e}",
    "é",
    "世界",
    "🚀",
    "true",
    "null",
    "~",
    ": ",
    "- ",
    "# ",
    "[",
    "{",
    "&a",
    "*a",
    "!!python/object:os.system",
    "]]>",
    "</script>",
];

fn awkward_values() -> Vec<Value> {
    let mut values: Vec<Value> = AWKWARD
        .iter()
        .map(|s| Value::String((*s).to_owned()))
        .collect();
    values.extend([
        Value::Null,
        Value::Bool(true),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(i64::MIN),
        Value::Uint(u64::MAX),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(-0.0),
        Value::Float(1e300),
        Value::List(Vec::new()),
        Value::List(vec![Value::Null]),
        Value::List(vec![Value::List(vec![Value::Map(BTreeMap::new())])]),
        Value::Map(BTreeMap::new()),
        Value::Map(BTreeMap::from([(String::new(), Value::Null)])),
        Value::Map(BTreeMap::from([(
            "a\nb".to_owned(),
            Value::String("${x}".to_owned()),
        )])),
    ]);
    values
}

/// Compiles a damaged tree and holds the target to the rule. `what` says what
/// was done to the tree, for the failure message.
fn hold(path: &str, target: &str, graph: &ResourceGraph, what: &str, failures: &mut Vec<String>) {
    let emitter = emitter(target);
    let result = catch_unwind(AssertUnwindSafe(|| emitter.emit(graph, &no_data())));
    match result {
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            failures.push(format!(
                "{target} panicked on {path} with {what}: {message}"
            ));
        }
        Ok(Err(refusal)) => {
            if refusal.rule.is_empty() || refusal.msg.is_empty() {
                failures.push(format!(
                    "{target} refused {path} with {what} and gave no rule or no message: {refusal:?}"
                ));
            }
            if refusal.target != target {
                failures.push(format!(
                    "{target} refused {path} with {what} as target {:?}",
                    refusal.target
                ));
            }
        }
        Ok(Ok(emitted)) => {
            for file in emitted.artifact.files.keys() {
                if file.starts_with('/')
                    || file.split('/').any(|part| part == "..")
                    || file.contains('\0')
                {
                    failures.push(format!(
                        "{target} compiled {path} with {what} to a file outside its directory: {file:?}"
                    ));
                }
            }
        }
    }
}

fn report(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The parts of a tree that come from the document's own names.
#[test]
fn no_target_panics_on_an_awkward_name_host_or_version() {
    let mut failures = Vec::new();
    for (path, target) in examples() {
        let graph = graph_from_file(&path, &target);
        for text in AWKWARD {
            let mut named = graph.clone();
            named.name = (*text).to_owned();
            hold(
                &path,
                &target,
                &named,
                &format!("name {text:?}"),
                &mut failures,
            );

            let mut hosted = graph.clone();
            hosted.host = (*text).to_owned();
            hold(
                &path,
                &target,
                &hosted,
                &format!("host {text:?}"),
                &mut failures,
            );

            let mut versioned = graph.clone();
            versioned.target_version = (*text).to_owned();
            hold(
                &path,
                &target,
                &versioned,
                &format!("targetVersion {text:?}"),
                &mut failures,
            );
        }
    }
    report(failures);
}

/// Every resource's id, type and state, one at a time.
#[test]
fn no_target_panics_on_an_awkward_id_type_or_state() {
    let mut failures = Vec::new();
    for (path, target) in examples() {
        let graph = graph_from_file(&path, &target);
        for index in 0..graph.resources.len() {
            for text in AWKWARD {
                let mut changed = graph.clone();
                changed.resources[index].id = (*text).to_owned();
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("resource {index} id {text:?}"),
                    &mut failures,
                );

                let mut changed = graph.clone();
                changed.resources[index].r#type = (*text).to_owned();
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("resource {index} type {text:?}"),
                    &mut failures,
                );

                let mut changed = graph.clone();
                changed.resources[index].state = (*text).to_owned();
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("resource {index} state {text:?}"),
                    &mut failures,
                );

                let mut changed = graph.clone();
                changed.resources[index].runtime_when = (*text).to_owned();
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("resource {index} runtimeWhen {text:?}"),
                    &mut failures,
                );
            }
        }
    }
    report(failures);
}

/// Every param of every resource replaced by every awkward value, and an
/// awkward key added beside them.
#[test]
fn no_target_panics_on_an_awkward_param() {
    let values = awkward_values();
    let mut failures = Vec::new();
    for (path, target) in examples() {
        let graph = graph_from_file(&path, &target);
        for index in 0..graph.resources.len() {
            let keys: Vec<String> = graph.resources[index].params.keys().cloned().collect();
            for key in &keys {
                for value in &values {
                    let mut changed = graph.clone();
                    changed.resources[index]
                        .params
                        .insert(key.clone(), value.clone());
                    hold(
                        &path,
                        &target,
                        &changed,
                        &format!("resource {index} param {key} = {value:?}"),
                        &mut failures,
                    );
                }
                let mut changed = graph.clone();
                changed.resources[index].params.remove(key);
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("resource {index} without param {key}"),
                    &mut failures,
                );
            }
            for text in AWKWARD {
                let mut changed = graph.clone();
                changed.resources[index]
                    .params
                    .insert((*text).to_owned(), Value::String("x".to_owned()));
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("resource {index} with a param named {text:?}"),
                    &mut failures,
                );
            }
            let mut changed = graph.clone();
            changed.resources[index].params.clear();
            hold(
                &path,
                &target,
                &changed,
                &format!("resource {index} with no params"),
                &mut failures,
            );
        }
    }
    report(failures);
}

/// The shape of the tree itself: resources missing, doubled or reordered,
/// edges that point nowhere or in a circle, outputs that read nothing.
#[test]
fn no_target_panics_on_a_malformed_tree() {
    let mut failures = Vec::new();
    for (path, target) in examples() {
        let graph = graph_from_file(&path, &target);
        let edge = |from: &str, to: &str, kind: EdgeKind, action: &str| Edge {
            from: from.to_owned(),
            to: to.to_owned(),
            kind,
            action: action.to_owned(),
            pos: Position::default(),
        };
        let first = graph.resources[0].id.clone();
        let last = graph.resources[graph.resources.len() - 1].id.clone();

        let mut empty = graph.clone();
        empty.resources.clear();
        hold(
            &path,
            &target,
            &empty,
            "no resources and its edges kept",
            &mut failures,
        );

        let mut bare = graph.clone();
        bare.resources.clear();
        bare.edges.clear();
        bare.outputs.clear();
        hold(&path, &target, &bare, "nothing at all", &mut failures);

        let mut doubled = graph.clone();
        doubled.resources.push(graph.resources[0].clone());
        hold(
            &path,
            &target,
            &doubled,
            "its first resource twice",
            &mut failures,
        );

        let mut reversed = graph.clone();
        reversed.resources.reverse();
        hold(
            &path,
            &target,
            &reversed,
            "its resources in reverse",
            &mut failures,
        );

        for index in 0..graph.resources.len() {
            let mut without = graph.clone();
            without.resources.remove(index);
            hold(
                &path,
                &target,
                &without,
                &format!("resource {index} gone and its edges kept"),
                &mut failures,
            );

            let mut alone = graph.clone();
            alone.resources = vec![graph.resources[index].clone()];
            hold(
                &path,
                &target,
                &alone,
                &format!("only resource {index}"),
                &mut failures,
            );
        }

        for kind in [EdgeKind::HardOrder, EdgeKind::Notify] {
            for action in ["", "restart", "run", "\n", "${x}"] {
                for (from, to) in [
                    (first.as_str(), "nowhere"),
                    ("nowhere", first.as_str()),
                    ("nowhere", "nowhere"),
                    (first.as_str(), first.as_str()),
                    (first.as_str(), last.as_str()),
                    (last.as_str(), first.as_str()),
                    ("", ""),
                ] {
                    let mut changed = graph.clone();
                    changed.edges.push(edge(from, to, kind, action));
                    hold(
                        &path,
                        &target,
                        &changed,
                        &format!("an edge {from:?} -> {to:?} ({kind}, action {action:?})"),
                        &mut failures,
                    );
                }
                // The same edge twice, and a loop of two.
                let mut changed = graph.clone();
                changed.edges.push(edge(&first, &last, kind, action));
                changed.edges.push(edge(&first, &last, kind, action));
                changed.edges.push(edge(&last, &first, kind, action));
                hold(
                    &path,
                    &target,
                    &changed,
                    &format!("a doubled edge and a loop ({kind}, action {action:?})"),
                    &mut failures,
                );
            }
        }

        for text in AWKWARD {
            let mut changed = graph.clone();
            changed.outputs.push(OutputDeclaration {
                name: (*text).to_owned(),
                value: (*text).to_owned(),
                pos: Position::default(),
            });
            hold(
                &path,
                &target,
                &changed,
                &format!("an output named and valued {text:?}"),
                &mut failures,
            );
        }
        let mut changed = graph.clone();
        changed.outputs.push(OutputDeclaration {
            name: "twice".to_owned(),
            value: format!("${{{first}}}"),
            pos: Position::default(),
        });
        changed
            .outputs
            .push(changed.outputs[changed.outputs.len() - 1].clone());
        hold(
            &path,
            &target,
            &changed,
            "the same output twice",
            &mut failures,
        );

        // A resource the target has never heard of, carrying everything.
        let mut changed = graph.clone();
        changed.resources.push(Resource {
            id: "probe".to_owned(),
            r#type: graph.resources[0].r#type.clone(),
            state: "absent".to_owned(),
            params: BTreeMap::from([(
                "${probe.x}".to_owned(),
                Value::String("${probe.x}".to_owned()),
            )]),
            runtime_when: "x".to_owned(),
            index: 0,
            pos: Position::default(),
        });
        hold(
            &path,
            &target,
            &changed,
            "a resource that reads itself through its own key",
            &mut failures,
        );
    }
    report(failures);
}
