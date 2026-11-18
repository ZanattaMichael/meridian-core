//! Stage two: the tool's ordering semantics, and what it makes of a notify.
//!
//! The resources arrive already in an order that satisfies every `dependsOn`,
//! so a tool that works through its input from top to bottom has nothing to
//! do here. A tool with its own way to say "after" (Compose's `depends_on`,
//! Terraform's references) writes the edges into its model in this stage.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Warning};

use crate::{refuse_at, Entry};

pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    _entries: &mut [Entry],
) -> Result<Vec<Warning>, CompileError> {
    // filesync reads its manifest from top to bottom, and the entries are in
    // the order the graph gave them, so `dependsOn` already holds.

    // TODO(plugin): a notify is the one thing tools differ on most. If your
    // tool reacts to a change by itself, compile the edge to that and declare
    // `native_notify`. If you can build the reaction from something else the
    // tool has, do that, declare `synthetic_notify`, and return a warning that
    // says what the operator will see. If neither is possible, refuse, as
    // here. What you must not do is accept the edge and ignore it.
    if let Some(edge) = graph.edges_of_kind(EdgeKind::Notify).next() {
        return Err(refuse_at(
            &edge.from,
            &edge.pos,
            &rules::UNSUPPORTED_NOTIFY,
            format!(
                "notifies resource {:?}, and filesync has no way to react to a change",
                edge.to
            ),
        ));
    }
    Ok(Vec::new())
}
