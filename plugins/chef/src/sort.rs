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
        let actions = mapping(notified).map_or(&[][..], |m| m.actions);
        if actions.is_empty() {
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
        if !actions.contains(&edge.action.as_str()) {
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
