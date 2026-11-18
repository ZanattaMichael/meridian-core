//! Stage two: Bicep's ordering semantics.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{refuse_at, Template};

/// Turns dependency edges into `dependsOn`.
///
/// Bicep orders resources by the references between them, so an edge a
/// reference already implies is left out; Bicep's own linter flags a
/// `dependsOn` that repeats one. A notify edge is refused: there is nothing
/// in a deployment to compile one into.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    template: &mut Template,
) -> Result<(), CompileError> {
    for edge in &graph.edges {
        if edge.kind == EdgeKind::Notify {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?} with action {:?}, which this target cannot express: \
                     a deployment has no way to act on one resource because another changed",
                    edge.to, edge.action
                ),
            ));
        }
        let position = |id: &str| template.statements.iter().position(|s| s.symbol == id);
        let (Some(_), Some(to)) = (position(&edge.from), position(&edge.to)) else {
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency must be between two resources of the file; the deployment's own \
                 settings are not one"
                    .to_owned(),
            ));
        };
        let statement = &mut template.statements[to];
        if !statement.refs.contains(&edge.from) && !statement.depends_on.contains(&edge.from) {
            statement.depends_on.push(edge.from.clone());
        }
    }
    for statement in &mut template.statements {
        statement.depends_on.sort();
    }
    Ok(())
}
