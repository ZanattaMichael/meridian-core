//! Stage two: Pulumi's ordering semantics.

use std::collections::BTreeSet;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Value, Warning};

use crate::{refuse_at, Kind, Program, NAME};

/// The resource option a notify with action `replace` compiles to.
const REPLACEMENT_TRIGGER: &str = "replacementTrigger";

/// Turns dependency edges into the `dependsOn` resource option, and a
/// notify with action `replace` into the `replacementTrigger` option.
///
/// Pulumi builds its own graph from the references between resources, so an
/// edge that a reference already implies is left out: the program then reads
/// the way a Pulumi author would have written it. Only an order the
/// properties do not show needs saying.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    program: &mut Program,
) -> Result<Vec<Warning>, CompileError> {
    let mut warnings = Vec::new();
    // The resources whose replacementTrigger a notify wrote.
    let mut triggered: BTreeSet<String> = BTreeSet::new();
    let stack = program.settings.declared.clone();
    for edge in &graph.edges {
        if let Some((id, _)) = &stack {
            if &edge.from == id || &edge.to == id {
                return Err(refuse_at(
                    &edge.to,
                    &edge.pos,
                    &rules::UNSUPPORTED_DEPENDENCY,
                    format!(
                        "{id:?} holds the stack's settings, which are read before anything \
                         runs; nothing can depend on it or be ordered by it"
                    ),
                ));
            }
        }
        let (Some(from), Some(to)) = (program.entry(&edge.from), program.entry(&edge.to)) else {
            let missing = if program.entry(&edge.from).is_none() {
                &edge.from
            } else {
                &edge.to
            };
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNKNOWN_DEPENDENCY,
                format!("an edge names resource {missing:?}, which this document does not declare"),
            ));
        };

        match edge.kind {
            EdgeKind::HardOrder => {
                if to.refs.contains(&edge.from) {
                    continue;
                }
                if let Some(other) = [from, to].into_iter().find(|e| e.kind != Kind::Managed) {
                    let why = match other.kind {
                        Kind::Config => {
                            "Pulumi reads configuration before anything runs, so nothing can \
                             depend on it or be ordered by it"
                        }
                        _ => {
                            "Pulumi runs a function when the values it is given are known, so \
                             it is ordered only by what its arguments refer to"
                        }
                    };
                    return Err(refuse_at(
                        &edge.to,
                        &edge.pos,
                        &rules::UNSUPPORTED_DEPENDENCY,
                        format!("{:?} is a {}: {why}", other.id, other.kind.noun()),
                    ));
                }
                let reference = Value::String(format!("${{{}}}", from.name));
                let index = program
                    .entries
                    .iter()
                    .position(|e| e.id == edge.to)
                    .expect("the entry was just found");
                let depends_on = program.entries[index]
                    .options
                    .entry("dependsOn".to_owned())
                    .or_insert_with(|| Value::List(Vec::new()));
                let Value::List(items) = depends_on else {
                    return Err(refuse_at(
                        &edge.to,
                        &edge.pos,
                        &rules::INVALID_PARAM,
                        "the resource option \"dependsOn\" must be a list".to_owned(),
                    ));
                };
                if !items.contains(&reference) {
                    items.push(reference);
                }
            }
            EdgeKind::Notify => {
                if edge.action != "replace" {
                    return Err(refuse_at(
                        &edge.from,
                        &edge.pos,
                        &rules::UNSUPPORTED_ACTION,
                        format!(
                            "notifies resource {:?} with action {:?}, which this target cannot \
                             express: the one reaction a Pulumi YAML program can be given is \
                             to replace a resource, with the option replacementTrigger \
                             (supported: replace)",
                            edge.to, edge.action
                        ),
                    ));
                }
                if to.kind != Kind::Managed {
                    return Err(refuse_at(
                        &edge.from,
                        &edge.pos,
                        &rules::UNSUPPORTED_ACTION,
                        format!(
                            "notifies resource {:?}, a {}: only a resource can be replaced",
                            to.id,
                            to.kind.noun()
                        ),
                    ));
                }
                // The trigger is what the notifier is made of: a resource's
                // properties, references and all, or a configuration value.
                let watched = match from.kind {
                    Kind::Managed => Value::Map(from.body.clone()),
                    Kind::Config => Value::String(format!("${{{}}}", from.name)),
                    Kind::Invoke => {
                        return Err(refuse_at(
                            &edge.from,
                            &edge.pos,
                            &rules::UNSUPPORTED_ACTION,
                            format!(
                                "{:?} is a function call, which does not change; notify from \
                                 the resource whose change matters",
                                from.id
                            ),
                        ))
                    }
                };
                let (from_name, to_id) = (from.name.clone(), to.id.clone());
                let index = program
                    .entries
                    .iter()
                    .position(|e| e.id == edge.to)
                    .expect("the entry was just found");
                let options = &mut program.entries[index].options;
                if !triggered.contains(&to_id) && options.contains_key(REPLACEMENT_TRIGGER) {
                    return Err(refuse_at(
                        &edge.to,
                        &edge.pos,
                        &rules::INVALID_PARAM,
                        format!(
                            "sets the resource option {REPLACEMENT_TRIGGER}, which a notify \
                             with action replace writes; use one or the other"
                        ),
                    ));
                }
                let trigger = options
                    .entry(REPLACEMENT_TRIGGER.to_owned())
                    .or_insert_with(|| Value::Map(Default::default()));
                if let Value::Map(trigger) = trigger {
                    trigger.insert(from_name.clone(), watched);
                }
                triggered.insert(to_id.clone());
                warnings.push(Warning {
                    target: NAME.to_owned(),
                    resource: to_id,
                    msg: format!(
                        "is replaced whenever {:?} changes: Pulumi has no handlers, so the \
                         resource option {REPLACEMENT_TRIGGER} holds what {from_name} is made of",
                        edge.from
                    ),
                });
            }
        }
    }
    Ok(warnings)
}
