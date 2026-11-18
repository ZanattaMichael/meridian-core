//! Stage two: systemd's ordering semantics.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{refuse_at, Units};

/// Turns dependency edges into `Requires=` and `After=`, and notify edges
/// into `PartOf=`.
///
/// Quadlet already orders a unit after every unit file it names: a container
/// whose `Volume` is `data.volume` requires and follows `data-volume.service`
/// without being told. An edge such a reference implies is left out, so the
/// unit reads the way a person would have written it. Only an order the keys
/// do not show needs saying.
pub(crate) fn apply_ordering(graph: &ResourceGraph, units: &mut Units) -> Result<(), CompileError> {
    for edge in &graph.edges {
        let (Some(from), Some(to)) = (units.unit(&edge.from), units.index(&edge.to)) else {
            let missing = if units.unit(&edge.from).is_none() {
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
        let service = from.service.clone();

        match edge.kind {
            EdgeKind::HardOrder => {
                if units.units[to].linked.contains(&edge.from) {
                    continue;
                }
                let section = units.units[to].section_mut("Unit");
                push_unique(section.entry("Requires".to_owned()).or_default(), &service);
                push_unique(section.entry("After".to_owned()).or_default(), &service);
            }
            EdgeKind::Notify => {
                if edge.action != "restart" {
                    return Err(refuse_at(
                        &edge.from,
                        &edge.pos,
                        &rules::UNSUPPORTED_ACTION,
                        format!(
                            "notifies resource {:?} with action {:?}, which this target cannot \
                             express: what systemd can do to a unit when another changes is \
                             restart it (supported: restart)",
                            edge.to, edge.action
                        ),
                    ));
                }
                // A restart of `from` is propagated to every unit that is
                // part of it, and the runner restarts `from` when its file
                // changes.
                let section = units.units[to].section_mut("Unit");
                push_unique(section.entry("PartOf".to_owned()).or_default(), &service);
            }
        }
    }
    Ok(())
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|v| v == value) {
        values.push(value.to_owned());
    }
}
