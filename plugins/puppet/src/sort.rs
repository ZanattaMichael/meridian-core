//! Stage two: Puppet's ordering semantics.

use std::collections::{BTreeSet, HashMap, HashSet};

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{Manifest, Reference, NAME};

fn fail(rule: &Rule, resource: &str, pos: &meridian_sdk::Position, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        resource: resource.to_owned(),
        pos: pos.clone(),
        msg,
    }
}

/// Turns every dependency edge into a `require` metaparameter on the
/// dependent resource.
///
/// This is where Puppet and Ansible diverge most visibly. Puppet's catalog is
/// a graph, so nothing is flattened and there is nothing to warn about: the
/// edges the author wrote survive into the output one for one, and the agent
/// is free to apply independent resources in whatever order it likes.
///
/// The stage still re-checks the order it was handed, for a cycle first and
/// then for a violated constraint. Puppet would catch a cycle itself, but with
/// a message about resource titles and not about the document.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    manifest: &mut Manifest,
) -> Result<(), CompileError> {
    let index: HashMap<String, usize> = manifest
        .resources
        .iter()
        .enumerate()
        .map(|(i, d)| (d.resource_id.clone(), i))
        .collect();
    let order = graph.order();
    let pos: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    check_acyclic(graph, &order)?;

    // A notify already orders the notifier before the resource it notifies,
    // so a require that says the same thing is noise in the output. What
    // notify covers is collected first so the two never appear on one pair.
    let implied: HashSet<(String, String)> = manifest
        .resources
        .iter()
        .flat_map(|d| {
            d.notify
                .iter()
                .map(|r| (d.resource_id.clone(), r.title.clone()))
        })
        .collect();

    let mut requires: HashMap<usize, BTreeSet<(String, Reference)>> = HashMap::new();
    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        // A hard-order edge points from the dependency to the resource that
        // declared it, so `to` carries the require.
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
        if implied.contains(&(edge.from.clone(), edge.to.clone())) {
            continue;
        }
        let (Some(&dependent), Some(&dependency)) = (index.get(&edge.to), index.get(&edge.from))
        else {
            let missing = if index.contains_key(&edge.to) {
                &edge.from
            } else {
                &edge.to
            };
            return Err(fail(
                &rules::UNKNOWN_DEPENDENCY,
                &edge.to,
                &edge.pos,
                format!(
                    "dependency edge names resource {missing:?}, which this manifest does not declare"
                ),
            ));
        };
        let reference = Reference {
            r#type: manifest.resources[dependency].r#type.clone(),
            title: edge.from.clone(),
        };
        requires
            .entry(dependent)
            .or_default()
            .insert((reference.to_string(), reference));
    }
    for (i, set) in requires {
        manifest.resources[i].require = set.into_iter().map(|(_, r)| r).collect();
    }
    Ok(())
}

/// Walks the graph one readiness level at a time. A resource that is never
/// placed is one a cycle is holding back.
fn check_acyclic(graph: &ResourceGraph, order: &[&str]) -> Result<(), CompileError> {
    // The core refuses a repeated id, and the counting below relies on it.
    let mut seen = HashSet::new();
    if let Some(repeated) = graph.resources.iter().find(|r| !seen.insert(r.id.as_str())) {
        return Err(fail(
            &rules::DUPLICATE_RESOURCE,
            &repeated.id,
            &repeated.pos,
            format!("resource id {:?} is used twice", repeated.id),
        ));
    }
    let mut indegree: HashMap<&str, usize> = order.iter().map(|id| (*id, 0)).collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        *indegree.entry(edge.to.as_str()).or_default() += 1;
        dependents
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    let mut ready: Vec<&str> = order
        .iter()
        .copied()
        .filter(|id| indegree.get(id) == Some(&0))
        .collect();
    let mut placed = 0;
    while !ready.is_empty() {
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
    Ok(())
}
