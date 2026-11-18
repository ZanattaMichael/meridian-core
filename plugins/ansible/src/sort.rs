//! Stage two: Ansible's ordering semantics.

use std::collections::HashMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Warning};

use crate::NAME;

/// Confirms the tree's apply order is one Ansible can run, and reports what
/// flattening it loses.
///
/// Ansible has no way to express a graph: the dependency graph becomes one
/// strict task sequence. That is lossy in a direction the operator should
/// know about. Resources that could have run concurrently are serialised, and
/// nothing in the playbook records that they were ever independent, so the
/// stage returns a warning naming them.
///
/// It also re-checks the order it was handed, for a cycle first and then for
/// a violated constraint. The core should have made both impossible, but a
/// target that trusts an invariant it never verifies fails silently when the
/// invariant breaks.
pub(crate) fn check_order(graph: &ResourceGraph) -> Result<Vec<Warning>, CompileError> {
    let order = graph.order();
    let pos: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    // The core refuses a repeated id, and the counting below relies on it.
    if let Some(repeated) = graph
        .resources
        .iter()
        .enumerate()
        .find(|(i, r)| pos[r.id.as_str()] != *i)
        .map(|(_, r)| r)
    {
        return Err(CompileError {
            target: NAME.to_owned(),
            ..CompileError::for_resource(
                repeated,
                &rules::DUPLICATE_RESOURCE,
                format!("resource id {:?} is used twice", repeated.id),
            )
        });
    }

    let mut indegree: HashMap<&str, usize> = order.iter().map(|id| (*id, 0)).collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        // A hard-order edge points from the dependency to the resource that
        // declared it, so `from` is applied before `to`.
        *indegree.entry(edge.to.as_str()).or_default() += 1;
        dependents
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }

    let (warnings, placed) = walk_levels(&order, &pos, indegree, &dependents);
    if placed != order.len() {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: rules::DEPENDENCY_CYCLE.name.to_owned(),
            hint: rules::DEPENDENCY_CYCLE.hint.to_owned(),
            msg: "a dependency cycle reached this target's sort stage; \
                  the graph stage should have rejected it first"
                .to_owned(),
            ..CompileError::default()
        });
    }

    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        let before = pos.get(edge.from.as_str()).copied().unwrap_or(0);
        let after = pos.get(edge.to.as_str()).copied().unwrap_or(0);
        if before > after {
            return Err(CompileError {
                target: NAME.to_owned(),
                rule: rules::ORDERING_VIOLATED.name.to_owned(),
                hint: rules::ORDERING_VIOLATED.hint.to_owned(),
                resource: edge.to.clone(),
                pos: edge.pos.clone(),
                msg: format!(
                    "resource {:?} is ordered before its dependency {:?}; \
                     the graph stage produced an order this target cannot flatten",
                    edge.to, edge.from
                ),
            });
        }
    }
    Ok(warnings)
}

/// Advances the graph one readiness level at a time. A level holding more
/// than one resource is exactly the parallelism this target is about to
/// discard, and the count of placed resources is how a cycle shows itself.
fn walk_levels<'a>(
    order: &[&'a str],
    pos: &HashMap<&str, usize>,
    mut indegree: HashMap<&'a str, usize>,
    dependents: &HashMap<&str, Vec<&'a str>>,
) -> (Vec<Warning>, usize) {
    let mut warnings = Vec::new();
    let mut ready: Vec<&str> = order
        .iter()
        .copied()
        .filter(|id| indegree.get(id) == Some(&0))
        .collect();

    let mut placed = 0;
    while !ready.is_empty() {
        ready.sort_by_key(|id| pos.get(id).copied().unwrap_or(usize::MAX));
        if ready.len() > 1 {
            let quoted: Vec<String> = ready.iter().map(|id| format!("{id:?}")).collect();
            warnings.push(Warning {
                target: NAME.to_owned(),
                resource: String::new(),
                msg: format!(
                    "resources {} are independent but will run in sequence; \
                     this target flattens the dependency graph and cannot express parallelism",
                    quoted.join(", ")
                ),
            });
        }
        let mut next = Vec::new();
        for id in &ready {
            placed += 1;
            for dependent in dependents.get(id).into_iter().flatten() {
                if let Some(count) = indegree.get_mut(dependent) {
                    *count -= 1;
                    if *count == 0 {
                        next.push(*dependent);
                    }
                }
            }
        }
        ready = next;
    }
    (warnings, placed)
}
