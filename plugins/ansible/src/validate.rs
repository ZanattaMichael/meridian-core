//! Stage three: the constraints that are Ansible's and not Meridian's.

use std::collections::{HashMap, HashSet};

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::CompileError;

use crate::{Playbook, Task, NAME};

fn fail(task: &Task, rule: &Rule, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: task.resource_id.clone(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        msg,
        pos: task.pos.clone(),
    }
}

/// Checks a play before anything is serialised. Every failure names the
/// offending resource and the rule it broke, so the message points at the
/// line the author must change.
pub(crate) fn validate(playbook: &Playbook) -> Result<(), CompileError> {
    if playbook.hosts.is_empty() {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: rules::MISSING_HOST.name.to_owned(),
            hint: rules::MISSING_HOST.hint.to_owned(),
            msg: "play has no target host".to_owned(),
            ..CompileError::default()
        });
    }

    // Handlers are notified by task name, so a duplicate name makes the
    // playbook ambiguous and not merely untidy.
    let mut names: HashMap<&str, &str> = HashMap::new();
    for task in &playbook.tasks {
        check_name(task, "task")?;
        if let Some(previous) = names.insert(&task.name, &task.resource_id) {
            return Err(fail(
                task,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "task name {:?} is already used by resource {:?}",
                    task.name, previous
                ),
            ));
        }
    }

    let mut handler_names = HashSet::new();
    for handler in &playbook.handlers {
        check_name(handler, "handler")?;
        if !handler_names.insert(handler.name.as_str()) {
            return Err(fail(
                handler,
                &rules::DUPLICATE_RESOURCE,
                format!("handler name {:?} is declared twice", handler.name),
            ));
        }
        if !handler.when.is_empty() {
            return Err(fail(
                handler,
                &rules::UNSUPPORTED_RUNTIME_CONDITION,
                format!(
                    "handler {:?} carries a condition; a handler runs because \
                     something changed, not because a condition holds",
                    handler.name
                ),
            ));
        }
    }

    for task in &playbook.tasks {
        for notified in &task.notify {
            if !handler_names.contains(notified.as_str()) {
                return Err(fail(
                    task,
                    &rules::UNKNOWN_NOTIFY_TARGET,
                    format!("notifies handler {notified:?}, which no handler block defines"),
                ));
            }
        }
        if task.module.is_empty() {
            return Err(fail(
                task,
                &rules::MISSING_PARAM,
                "task has no module".to_owned(),
            ));
        }
        // A module the plugin mapped always has arguments; one the document
        // named may take none, as `ansible.builtin.setup` does.
        if task.args.is_empty() && !task.native {
            return Err(fail(
                task,
                &rules::EMPTY_PARAM,
                format!("module {} was given no arguments", task.module),
            ));
        }
    }
    Ok(())
}

fn check_name(task: &Task, what: &str) -> Result<(), CompileError> {
    if task.name.trim().is_empty() {
        return Err(fail(
            task,
            &rules::INVALID_NAME,
            format!("{what} has an empty name"),
        ));
    }
    if task.name.contains(['\n', '\r']) {
        return Err(fail(
            task,
            &rules::INVALID_NAME,
            format!("{what} name {:?} spans more than one line", task.name),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{Position, Value};

    fn task(id: &str, name: &str) -> Task {
        Task {
            name: name.to_owned(),
            module: "ansible.builtin.package".to_owned(),
            args: [("name".to_owned(), Value::String("nginx".to_owned()))].into(),
            resource_id: id.to_owned(),
            pos: Position {
                line: 7,
                ..Position::default()
            },
            ..Task::default()
        }
    }

    fn play() -> Playbook {
        Playbook {
            name: "web".to_owned(),
            hosts: "web01".to_owned(),
            inventory_vars: Vec::new(),
            tasks: vec![Task {
                notify: vec!["restart nginx".to_owned()],
                ..task("pkg", "install nginx")
            }],
            handlers: vec![task("svc", "restart nginx")],
        }
    }

    /// Every rule of this stage, each broken by one change to a play that
    /// is otherwise accepted, with the resource the refusal must name.
    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Playbook);
        let table: &[(&str, &str, Break)] = &[
            ("missing-host", "", |p| p.hosts.clear()),
            ("invalid-name", "pkg", |p| p.tasks[0].name = "  ".to_owned()),
            ("invalid-name", "pkg", |p| {
                p.tasks[0].name = "install\nnginx".to_owned();
            }),
            ("invalid-name", "pkg", |p| {
                p.tasks[0].name = "install\rnginx".to_owned();
            }),
            ("duplicate-resource", "again", |p| {
                p.tasks.push(task("again", "install nginx"));
            }),
            ("invalid-name", "svc", |p| p.handlers[0].name.clear()),
            ("invalid-name", "svc", |p| {
                p.handlers[0].name = "restart\nnginx".to_owned();
            }),
            ("duplicate-resource", "twin", |p| {
                p.handlers.push(task("twin", "restart nginx"));
            }),
            ("unsupported-runtime-condition", "svc", |p| {
                p.handlers[0].when = "ansible_os_family == 'Debian'".to_owned();
            }),
            ("unknown-notify-target", "pkg", |p| {
                p.tasks[0].notify.push("reload nginx".to_owned());
            }),
            ("unknown-notify-target", "pkg", |p| p.handlers.clear()),
            ("missing-param", "pkg", |p| p.tasks[0].module.clear()),
            ("empty-param", "pkg", |p| p.tasks[0].args.clear()),
        ];

        assert_eq!(validate(&play()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = play();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 7, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Playbook);
        let table: &[(&str, Change)] = &[
            ("a task and a handler may share a name", |p| {
                p.tasks.push(task("other", "restart nginx"));
            }),
            ("a task may carry a condition", |p| {
                p.tasks[0].when = "ansible_os_family == 'Debian'".to_owned();
            }),
            ("a handler nothing notifies", |p| {
                p.handlers.push(task("spare", "reload nginx"));
            }),
            ("a play with no handlers and no notifications", |p| {
                p.handlers.clear();
                p.tasks[0].notify.clear();
            }),
            ("a play with no tasks at all", |p| p.tasks.clear()),
            ("a module the document named, given no arguments", |p| {
                p.tasks[0].args.clear();
                p.tasks[0].native = true;
            }),
        ];
        for (what, change) in table {
            let mut p = play();
            change(&mut p);
            assert_eq!(validate(&p), Ok(()), "{what}");
        }
    }

    #[test]
    fn the_first_broken_rule_is_the_one_reported() {
        let mut p = play();
        p.hosts.clear();
        p.tasks[0].module.clear();
        assert_eq!(validate(&p).unwrap_err().rule, "missing-host");

        // Names are checked for every task before any task's module is.
        let mut p = play();
        p.tasks[0].module.clear();
        p.tasks.push(task("late", ""));
        assert_eq!(validate(&p).unwrap_err().rule, "invalid-name");
    }
}
