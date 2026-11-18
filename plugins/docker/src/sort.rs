//! Stage two: Compose's ordering semantics, and the two kinds of notify.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Value, Warning};

use crate::{refuse_at, Composition, Section, NAME};

/// The label a synthetic notify compiles to, on the notified service.
const LABEL_PREFIX: &str = "meridian.dev/restart-on-";

/// Compose starts services in dependency order and creates everything else
/// before any of them, so only a dependency between two services has to be
/// written down.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    composition: &mut Composition,
) -> Result<Vec<Warning>, CompileError> {
    let mut warnings = Vec::new();

    for edge in &graph.edges {
        let position = |id: &str| composition.elements.iter().position(|e| e.id == id);
        let (Some(from), Some(to)) = (position(&edge.from), position(&edge.to)) else {
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency must be between two elements of the Compose file; the project's own \
                 settings are not one"
                    .to_owned(),
            ));
        };
        let from_section = composition.elements[from].section;
        let to_section = composition.elements[to].section;

        if edge.kind == EdgeKind::HardOrder {
            match (from_section, to_section) {
                (Section::Service, Section::Service) => {
                    depend(composition, to, &edge.from, false).map_err(|msg| {
                        refuse_at(&edge.to, &edge.pos, &rules::INVALID_PARAM, msg)
                    })?;
                }
                // Already true of every Compose project.
                (_, Section::Service) => {}
                _ => warnings.push(Warning {
                    target: NAME.to_owned(),
                    resource: edge.to.clone(),
                    msg: format!(
                        "dependsOn {:?} orders nothing here: Compose creates every {} before it \
                         starts a service, and in no order among themselves",
                        edge.from,
                        to_section.type_name()
                    ),
                }),
            }
            continue;
        }

        if edge.action != "restart" {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?} with action {:?}, which this target cannot express: \
                     the one reaction Compose can be given is to restart a service (supported: \
                     restart)",
                    edge.to, edge.action
                ),
            ));
        }
        if to_section != Section::Service {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?}, a {}, which is not something that can restart \
                     (supported: a service)",
                    edge.to,
                    to_section.type_name()
                ),
            ));
        }

        if from_section == Section::Service {
            depend(composition, to, &edge.from, true)
                .map_err(|msg| refuse_at(&edge.to, &edge.pos, &rules::INVALID_PARAM, msg))?;
            continue;
        }

        let notifier = &composition.elements[from];
        let checksum = checksum(&Value::Map(notifier.body.clone()).canonical());
        let key = format!("{LABEL_PREFIX}{}", notifier.id);
        warnings.push(Warning {
            target: NAME.to_owned(),
            resource: edge.to.clone(),
            msg: format!(
                "restarts by being recreated whenever the content of {:?} changes; the trigger is \
                 the label {key} on the service",
                edge.from
            ),
        });
        let labels = composition.elements[to]
            .body
            .entry("labels".to_owned())
            .or_insert_with(|| Value::Map(BTreeMap::new()));
        match labels {
            Value::Map(labels) => {
                labels.insert(key, Value::String(checksum));
            }
            Value::List(labels) => labels.push(Value::String(format!("{key}={checksum}"))),
            _ => {
                return Err(refuse_at(
                    &edge.to,
                    &edge.pos,
                    &rules::INVALID_PARAM,
                    "labels of the notified service must be a map or a list".to_owned(),
                ))
            }
        }
    }
    Ok(warnings)
}

/// Makes the service at `index` depend on `on`, in Compose's long form, and
/// restart after it when `restart` is set. What the author already wrote
/// about the dependency is kept.
fn depend(
    composition: &mut Composition,
    index: usize,
    on: &str,
    restart: bool,
) -> Result<(), String> {
    let started = || {
        Value::Map(BTreeMap::from([(
            "condition".to_owned(),
            Value::String("service_started".to_owned()),
        )]))
    };
    // A dependency Meridian writes waits for the other service to be healthy
    // when it can say so, and to have started when it cannot.
    let healthy = composition
        .elements
        .iter()
        .any(|e| e.id == on && e.section == Section::Service && crate::check::has_healthcheck(e));
    let waited = || {
        Value::Map(BTreeMap::from([(
            "condition".to_owned(),
            Value::String(
                if healthy {
                    "service_healthy"
                } else {
                    "service_started"
                }
                .to_owned(),
            ),
        )]))
    };
    let slot = composition.elements[index]
        .body
        .entry("depends_on".to_owned())
        .or_insert_with(|| Value::Map(BTreeMap::new()));
    if let Value::List(names) = slot {
        let mut map = BTreeMap::new();
        for name in names.iter() {
            let Value::String(name) = name else {
                return Err("depends_on must list service names".to_owned());
            };
            map.insert(name.clone(), started());
        }
        *slot = Value::Map(map);
    }
    let Value::Map(dependencies) = slot else {
        return Err("depends_on must be a list or a map".to_owned());
    };
    let entry = dependencies.entry(on.to_owned()).or_insert_with(waited);
    let Value::Map(entry) = entry else {
        return Err(format!("depends_on.{on} must be a map"));
    };
    if restart {
        entry.insert("restart".to_owned(), Value::Bool(true));
    }
    Ok(())
}

/// A short, stable fingerprint of some content (64-bit FNV-1a). It only has
/// to change when the content does; it protects nothing.
fn checksum(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_stable_and_follows_the_content() {
        assert_eq!(checksum(""), "cbf29ce484222325");
        assert_ne!(checksum("workers = 4"), checksum("workers = 8"));
    }
}
