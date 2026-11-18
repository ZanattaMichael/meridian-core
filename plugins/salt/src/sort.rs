//! Stage two: Salt's ordering semantics.

use std::collections::HashMap;

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, EdgeKind, Position, ResourceGraph};

use crate::{Sls, NAME};

fn fail(rule: &Rule, resource: &str, pos: &Position, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        resource: resource.to_owned(),
        pos: pos.clone(),
        msg,
    }
}

/// Turns every dependency edge into a `require` requisite on the dependent
/// state.
///
/// Salt evaluates states in the order the file lists them and honours
/// requisites on top of that, so the dependency graph survives into the
/// output edge for edge. Nothing is flattened and there is nothing to warn
/// about: the file order is the tree's apply order, and each `require` says
/// why. A state whose requirement failed is not run, which is exactly what
/// the edge means.
///
/// The stage still re-checks the order it was handed, for a cycle first and
/// then for a violated constraint. Salt would catch a cycle itself, but at
/// apply time, on the minion, with a message about state IDs and not about
/// the document.
pub(crate) fn apply_ordering(graph: &ResourceGraph, sls: &mut Sls) -> Result<(), CompileError> {
    let order = graph.order();
    let pos: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    check_acyclic(graph, &order)?;

    let index: HashMap<String, usize> = sls
        .states
        .iter()
        .enumerate()
        .map(|(i, s)| (s.resource_id.clone(), i))
        .collect();

    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        // A hard-order edge points from the dependency to the resource that
        // declared it, so `from` is applied before `to` and `to` carries the
        // require.
        let before = pos.get(edge.from.as_str()).copied().unwrap_or(0);
        let after = pos.get(edge.to.as_str()).copied().unwrap_or(0);
        if before > after {
            return Err(fail(
                &rules::ORDERING_VIOLATED,
                &edge.to,
                &edge.pos,
                format!(
                    "resource {:?} is ordered before its dependency {:?}; \
                     the graph stage produced an order this target cannot trust",
                    edge.to, edge.from
                ),
            ));
        }
        let (Some(&dependent), Some(&dependency)) = (index.get(&edge.to), index.get(&edge.from))
        else {
            return Err(fail(
                &rules::UNKNOWN_DEPENDENCY,
                &edge.to,
                &edge.pos,
                format!(
                    "resource {:?} depends on {:?}, and one of them is not in the tree",
                    edge.to, edge.from
                ),
            ));
        };
        let reference = sls.states[dependency].reference();
        let state = &mut sls.states[dependent];
        // A watch already requires what it watches, so a require that says
        // the same thing is noise in the output.
        if !state.watch.contains(&reference) && !state.require.contains(&reference) {
            state.require.push(reference);
        }
    }
    Ok(())
}

/// Kahn's algorithm over the hard-order edges: if not every resource can be
/// placed, what is left is a cycle.
fn check_acyclic(graph: &ResourceGraph, order: &[&str]) -> Result<(), CompileError> {
    let mut indegree: HashMap<&str, usize> = order.iter().map(|id| (*id, 0)).collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        *indegree.entry(edge.to.as_str()).or_default() += 1;
        dependents
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut placed = 0;
    while let Some(id) = ready.pop() {
        placed += 1;
        for dependent in dependents.get(id).into_iter().flatten() {
            if let Some(count) = indegree.get_mut(dependent) {
                *count -= 1;
                if *count == 0 {
                    ready.push(dependent);
                }
            }
        }
    }
    if placed != indegree.len() {
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
    Ok(())
}
