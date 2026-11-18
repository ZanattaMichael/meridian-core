//! Stage two: CloudFormation's ordering semantics.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{refuse_at, Template};

/// Turns dependency edges into `DependsOn`.
///
/// CloudFormation orders resources by the references between them, so an edge
/// a `Ref` or `Fn::GetAtt` already implies is left out. Only an order the
/// properties do not show needs saying. A notify edge is refused: there is
/// nothing in CloudFormation to compile one into.
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
                     CloudFormation has no way to act on one resource because another changed",
                    edge.to, edge.action
                ),
            ));
        }
        let position = |id: &str| template.entries.iter().position(|e| e.id == id);
        let (Some(from), Some(to)) = (position(&edge.from), position(&edge.to)) else {
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency must be between two resources of the template; the stack's own \
                 settings are not one"
                    .to_owned(),
            ));
        };
        if template.entries[to].refs.contains(&edge.from) {
            continue;
        }
        let logical = template.entries[from].logical.clone();
        let depends_on = &mut template.entries[to].depends_on;
        if !depends_on.contains(&logical) {
            depends_on.push(logical);
        }
    }
    for entry in &mut template.entries {
        entry.depends_on.sort();
    }
    Ok(())
}
