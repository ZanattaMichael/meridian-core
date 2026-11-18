//! Stage two: Chef's ordering semantics, and its notifications.

use std::collections::HashMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::transform::mapping;
use crate::{refuse_at, Cookbook, Notify};

/// A recipe runs from top to bottom, and the blocks are already in the
/// tree's apply order, so a dependency needs nothing written down. The stage
/// re-checks the order it was handed and turns each notifies edge into Chef's
/// own `notifies`.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    cookbook: &mut Cookbook,
) -> Result<(), CompileError> {
    // The first block of each resource; a user's group memberships follow it.
    let mut first: HashMap<&str, usize> = HashMap::new();
    for (i, block) in cookbook.blocks.iter().enumerate() {
        first.entry(block.resource_id.as_str()).or_insert(i);
    }
    let first: HashMap<String, usize> = first
        .into_iter()
        .map(|(id, i)| (id.to_owned(), i))
        .collect();

    for edge in &graph.edges {
        let (Some(&from), Some(&to)) = (first.get(&edge.from), first.get(&edge.to)) else {
            let missing = if first.contains_key(&edge.to) {
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

        if edge.kind == EdgeKind::HardOrder {
            if from > to {
                return Err(refuse_at(
                    &edge.to,
                    &edge.pos,
                    &rules::ORDERING_VIOLATED,
                    format!(
                        "resource {:?} is ordered before its dependency {:?}; the graph stage \
                         produced an order this target cannot trust",
                        edge.to, edge.from
                    ),
                ));
            }
            continue;
        }

        let notified = graph.resource(&edge.to).map_or("", |r| r.r#type.as_str());
        // A resource of Chef's own takes the action it is notified with, as
        // a recipe writes it: which actions it has is the resource's own
        // business, until its schema is synced.
        let native = graph
            .resource(&edge.to)
            .map(crate::transform::native_resource)
            .transpose()?
            .flatten();
        if let Some(resource) = native {
            if !is_action(&edge.action) {
                return Err(refuse_at(
                    &edge.from,
                    &edge.pos,
                    &rules::UNSUPPORTED_ACTION,
                    format!(
                        "notifies resource {:?} with action {:?}, which is not a Chef action: \
                         on resource {resource} the action is written as the recipe writes it, \
                         one lower-case word such as run or reload",
                        edge.to, edge.action
                    ),
                ));
            }
        }
        let actions = mapping(notified).map_or(&[][..], |m| m.actions);
        if native.is_none() && actions.is_empty() {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?}, a {notified}, which has nothing to do when notified \
                     on this target (a service or an exec does)",
                    edge.to
                ),
            ));
        }
        if native.is_none() && !actions.contains(&edge.action.as_str()) {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?} with action {:?}, which resource type {notified:?} \
                     does not support on this target (supported: {})",
                    edge.to,
                    edge.action,
                    actions.join(", ")
                ),
            ));
        }
        let notify = Notify {
            target: format!(
                "{}[{}]",
                cookbook.blocks[to].resource, cookbook.blocks[to].name
            ),
            action: edge.action.clone(),
        };
        let notifies = &mut cookbook.blocks[from].notifies;
        if !notifies.contains(&notify) {
            notifies.push(notify);
            notifies.sort();
        }
    }
    Ok(())
}

/// Whether `action` is shaped like a Chef action: one lower-case word.
fn is_action(action: &str) -> bool {
    action.starts_with(|c: char| c.is_ascii_lowercase())
        && action
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
