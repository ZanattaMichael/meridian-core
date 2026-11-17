//! Stage three: the constraints that are Salt's and not Meridian's.

use std::collections::HashMap;

use meridian_sdk::CompileError;

use crate::{Reference, Sls, State, NAME};

/// Top-level keys of a state file that Salt reads as declarations of its own
/// and never as a state's ID.
const RESERVED_IDS: &[&str] = &["exclude", "extend", "include"];

/// What must not appear in a runtime condition: it is written inside a Jinja
/// `if` tag, so anything that opens or closes a tag would end it early.
const JINJA_DELIMITERS: &[&str] = &["{%", "%}", "{{", "}}", "{#", "#}"];

fn fail(state: &State, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: state.resource_id.clone(),
        rule: rule.to_owned(),
        msg,
        pos: state.pos.clone(),
    }
}

fn fail_document(rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        rule: rule.to_owned(),
        msg,
        ..CompileError::default()
    }
}

/// Checks a state file before anything is serialised. Every failure names the
/// offending resource and the rule it broke, so the message points at the
/// line the author must change.
pub(crate) fn validate(sls: &Sls) -> Result<(), CompileError> {
    check_name(&sls.name)?;
    check_host(&sls.host)?;

    // Requisites name a state by its ID, so a duplicate makes the file
    // ambiguous and Salt refuses to render it.
    let mut ids: HashMap<&str, &State> = HashMap::new();
    for state in sls.all() {
        check_id(state)?;
        if let Some(previous) = ids.insert(&state.id, state) {
            return Err(fail(
                state,
                "duplicate-state-id",
                format!(
                    "state ID {:?} is already used by resource {:?}",
                    state.id, previous.resource_id
                ),
            ));
        }
    }

    for state in sls.all() {
        if state.module.is_empty() || state.function.is_empty() {
            return Err(fail(
                state,
                "missing-function",
                "state has no function".to_owned(),
            ));
        }
        if !state.args.iter().any(|(name, _)| name == "name") {
            return Err(fail(
                state,
                "missing-name",
                format!("{}.{} was given no name", state.module, state.function),
            ));
        }
        for (kind, references) in [
            ("require", &state.require),
            ("watch", &state.watch),
            ("onchanges", &state.onchanges),
        ] {
            for reference in references {
                check_reference(state, kind, reference, &ids)?;
            }
        }
        check_condition(state)?;
    }

    for reaction in &sls.reactions {
        if !reaction.when.is_empty() {
            return Err(fail(
                reaction,
                "conditional-reaction",
                format!(
                    "state {:?} carries a condition; it runs because something \
                     changed, not because a condition holds",
                    reaction.id
                ),
            ));
        }
        if reaction.watch.is_empty() && reaction.onchanges.is_empty() {
            return Err(fail(
                reaction,
                "unbound-reaction",
                format!("state {:?} reacts to nothing", reaction.id),
            ));
        }
    }
    Ok(())
}

/// A state file is applied by name, and Salt reads a dot in that name as a
/// directory separator.
fn check_name(name: &str) -> Result<(), CompileError> {
    if name.is_empty() {
        return Err(fail_document(
            "missing-name",
            "the document has no name to call its state file by".to_owned(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(fail_document(
            "invalid-sls-name",
            format!(
                "document name {name:?} cannot name a state file; Salt applies a state by name \
                 and allows letters, digits, \"_\" and \"-\" there (a dot means a directory)"
            ),
        ));
    }
    if name == "top" {
        return Err(fail_document(
            "reserved-sls-name",
            "a document named \"top\" would compile to top.sls, which Salt reads as the top file"
                .to_owned(),
        ));
    }
    Ok(())
}

/// The top file matches the host as an exact minion ID, in a comma-separated
/// list of one.
fn check_host(host: &str) -> Result<(), CompileError> {
    if host.is_empty() {
        return Err(fail_document(
            "missing-host",
            "state file has no target host".to_owned(),
        ));
    }
    if host.contains(|c: char| c.is_whitespace() || c == ',') {
        return Err(fail_document(
            "invalid-host",
            format!("host {host:?} cannot be a minion ID: it holds a space or a comma"),
        ));
    }
    Ok(())
}

fn check_id(state: &State) -> Result<(), CompileError> {
    if state.id.trim().is_empty() {
        return Err(fail(state, "empty-id", "state has an empty ID".to_owned()));
    }
    if state.id.contains(['\n', '\r']) {
        return Err(fail(
            state,
            "multiline-id",
            format!("state ID {:?} spans more than one line", state.id),
        ));
    }
    if RESERVED_IDS.contains(&state.id.as_str()) {
        return Err(fail(
            state,
            "reserved-id",
            format!(
                "state ID {:?} is a declaration Salt reads for itself (reserved: {})",
                state.id,
                RESERVED_IDS.join(", ")
            ),
        ));
    }
    Ok(())
}

fn check_reference(
    state: &State,
    kind: &str,
    reference: &Reference,
    ids: &HashMap<&str, &State>,
) -> Result<(), CompileError> {
    match ids.get(reference.id.as_str()) {
        Some(found) if found.module == reference.module => {}
        _ => {
            return Err(fail(
                state,
                "missing-requisite",
                format!(
                    "{kind} names {}: {:?}, which no state in the file is",
                    reference.module, reference.id
                ),
            ));
        }
    }
    if reference.id == state.id {
        return Err(fail(
            state,
            "self-requisite",
            format!("state {:?} names itself in its {kind}", state.id),
        ));
    }
    Ok(())
}

fn check_condition(state: &State) -> Result<(), CompileError> {
    let when = &state.when;
    if when.is_empty() {
        return Ok(());
    }
    if when.trim().is_empty() {
        return Err(fail(
            state,
            "invalid-runtime-condition",
            "runtimeWhen is blank".to_owned(),
        ));
    }
    if when.contains(['\n', '\r']) {
        return Err(fail(
            state,
            "invalid-runtime-condition",
            "runtimeWhen spans more than one line".to_owned(),
        ));
    }
    if let Some(found) = JINJA_DELIMITERS.iter().find(|d| when.contains(*d)) {
        return Err(fail(
            state,
            "invalid-runtime-condition",
            format!(
                "runtimeWhen {when:?} holds {found:?}; on this target it is a bare Jinja \
                 expression such as grains['os_family'] == 'Debian', written without tag delimiters"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{Position, Value};

    fn state(module: &'static str, function: &'static str, id: &str) -> State {
        State {
            id: id.to_owned(),
            module,
            function,
            args: vec![("name".to_owned(), Value::String("nginx".to_owned()))],
            resource_id: id.to_owned(),
            pos: Position {
                line: 13,
                ..Position::default()
            },
            ..State::default()
        }
    }

    fn to(module: &'static str, id: &str) -> Reference {
        Reference {
            module,
            id: id.to_owned(),
        }
    }

    fn sls() -> Sls {
        Sls {
            name: "web".to_owned(),
            host: "web01".to_owned(),
            states: vec![
                state("pkg", "installed", "pkg"),
                State {
                    require: vec![to("pkg", "pkg")],
                    ..state("file", "managed", "conf")
                },
            ],
            secrets: Vec::new(),
            reactions: vec![State {
                watch: vec![to("file", "conf")],
                ..state("service", "running", "svc")
            }],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Sls);
        let table: &[(&str, &str, Break)] = &[
            ("missing-name", "", |s| s.name.clear()),
            ("invalid-sls-name", "", |s| s.name = "web.prod".to_owned()),
            ("invalid-sls-name", "", |s| s.name = "web prod".to_owned()),
            ("invalid-sls-name", "", |s| s.name = "web/prod".to_owned()),
            ("reserved-sls-name", "", |s| s.name = "top".to_owned()),
            ("missing-host", "", |s| s.host.clear()),
            ("invalid-host", "", |s| s.host = "web01,web02".to_owned()),
            ("invalid-host", "", |s| s.host = "web 01".to_owned()),
            ("invalid-host", "", |s| s.host = "web01\n".to_owned()),
            ("empty-id", "pkg", |s| s.states[0].id = " ".to_owned()),
            ("multiline-id", "pkg", |s| {
                s.states[0].id = "a\nb".to_owned()
            }),
            ("reserved-id", "pkg", |s| {
                s.states[0].id = "include".to_owned()
            }),
            ("reserved-id", "pkg", |s| {
                s.states[0].id = "extend".to_owned()
            }),
            ("reserved-id", "pkg", |s| {
                s.states[0].id = "exclude".to_owned()
            }),
            ("duplicate-state-id", "again", |s| {
                let mut twin = state("pkg", "installed", "again");
                twin.id = "pkg".to_owned();
                s.states.push(twin);
            }),
            // A state and a reaction share one set of IDs.
            ("duplicate-state-id", "svc", |s| {
                s.reactions[0].id = "conf".to_owned()
            }),
            ("missing-function", "pkg", |s| s.states[0].function = ""),
            ("missing-function", "pkg", |s| s.states[0].module = ""),
            ("missing-name", "pkg", |s| s.states[0].args.clear()),
            ("missing-requisite", "conf", |s| {
                s.states[1].require = vec![to("pkg", "gone")];
            }),
            // The ID exists, but as a state of another module.
            ("missing-requisite", "conf", |s| {
                s.states[1].require = vec![to("service", "pkg")];
            }),
            ("missing-requisite", "svc", |s| {
                s.reactions[0].watch = vec![to("file", "gone")];
            }),
            ("missing-requisite", "conf", |s| {
                s.states[1].onchanges = vec![to("cmd", "gone")];
            }),
            ("self-requisite", "conf", |s| {
                s.states[1].require = vec![to("file", "conf")];
            }),
            ("invalid-runtime-condition", "pkg", |s| {
                s.states[0].when = "  ".to_owned()
            }),
            ("invalid-runtime-condition", "pkg", |s| {
                s.states[0].when = "a\nb".to_owned();
            }),
            ("conditional-reaction", "svc", |s| {
                s.reactions[0].when = "grains['os'] == 'Debian'".to_owned();
            }),
            ("unbound-reaction", "svc", |s| s.reactions[0].watch.clear()),
        ];

        assert_eq!(validate(&sls()), Ok(()));
        for (i, (rule, resource, break_it)) in table.iter().enumerate() {
            let mut broken = sls();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule, "row {i}");
            assert_eq!(err.resource, *resource, "row {i}: {rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 13, "{rule} must point at the resource");
            }
        }
    }

    /// A runtime condition is written inside a Jinja `if` tag, so no
    /// delimiter that would end the tag may appear in it.
    #[test]
    fn a_condition_holds_no_jinja_delimiter() {
        for delimiter in JINJA_DELIMITERS {
            let mut s = sls();
            s.states[0].when = format!("grains['os'] {delimiter} 'x'");
            let err = validate(&s).expect_err(delimiter);
            assert_eq!(err.rule, "invalid-runtime-condition", "{delimiter}");
            assert!(err.msg.contains(delimiter), "{delimiter}");
        }
        for good in [
            "grains['os_family'] == 'Debian'",
            "salt['file.file_exists']('/etc/x')",
            "{'a': 1}['a'] % 2 == 1",
        ] {
            let mut s = sls();
            s.states[0].when = good.to_owned();
            assert_eq!(validate(&s), Ok(()), "{good}");
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Sls);
        let table: &[(&str, Change)] = &[
            ("a name with a hyphen and an underscore", |s| {
                s.name = "web-prod_1".to_owned();
            }),
            ("a name that only starts like the top file", |s| {
                s.name = "topology".to_owned()
            }),
            ("a fully qualified minion ID", |s| {
                s.host = "web01.example.com".to_owned()
            }),
            ("a reaction bound by onchanges alone", |s| {
                let r = &mut s.reactions[0];
                r.onchanges = std::mem::take(&mut r.watch);
            }),
            ("a state that requires a reaction", |s| {
                s.states[1].require.push(to("service", "svc"));
            }),
            ("an ID that only holds a reserved word", |s| {
                s.states[0].id = "include_pkg".to_owned();
                s.states[1].require = vec![to("pkg", "include_pkg")];
            }),
            ("a file with no states", |s| {
                s.states.clear();
                s.reactions.clear();
            }),
        ];
        for (what, change) in table {
            let mut s = sls();
            change(&mut s);
            assert_eq!(validate(&s), Ok(()), "{what}");
        }
    }
}
