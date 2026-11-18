//! Stage two: Pulumi's ordering semantics.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Value};

use crate::{refuse_at, Kind, Program};

/// Turns dependency edges into the `dependsOn` resource option.
///
/// Pulumi builds its own graph from the references between resources, so an
/// edge that a reference already implies is left out: the program then reads
/// the way a Pulumi author would have written it. Only an order the
/// properties do not show needs saying.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    program: &mut Program,
) -> Result<(), CompileError> {
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
                return Err(refuse_at(
                    &edge.from,
                    &edge.pos,
                    &rules::UNSUPPORTED_ACTION,
                    format!(
                        "notifies resource {:?} with action {:?}, which this target cannot \
                         express: nothing in a Pulumi YAML program makes one resource react to \
                         a change in another. Where a change must replace a resource, pass the \
                         changing value into one of its properties and name that property in \
                         the resource option replaceOnChanges",
                        edge.to, edge.action
                    ),
                ));
            }
        }
    }
    Ok(())
}
