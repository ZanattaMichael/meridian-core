//! Stage two: Resource Manager's ordering semantics.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{refuse_at, Template};

/// Turns dependency edges and references into `dependsOn`.
///
/// Resource Manager deploys resources in parallel unless told otherwise, and
/// a `resourceId()` or a `reference()` by id does not tell it. So every
/// resource another one refers to is written into its `dependsOn`, together
/// with every order the document asks for. A notify edge is refused: there is
/// nothing in a template to compile one into.
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
                     Resource Manager has no way to act on one resource because another changed",
                    edge.to, edge.action
                ),
            ));
        }
        let position = |id: &str| template.entries.iter().position(|e| e.id == id);
        let (Some(_), Some(to)) = (position(&edge.from), position(&edge.to)) else {
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency must be between two resources of the template; a parameter and \
                 the deployment's own settings are not resources"
                    .to_owned(),
            ));
        };
        template.entries[to].depends.insert(edge.from.clone());
    }

    if let Some(cycle) = find_cycle(template) {
        let first = template
            .entries
            .iter()
            .find(|e| e.id == cycle[0])
            .expect("a cycle is made of entries");
        return Err(refuse_at(
            &first.id,
            &first.pos,
            &rules::DEPENDENCY_CYCLE,
            format!(
                "resources depend on each other in a circle, which Resource Manager refuses: {}",
                cycle.join(" -> ")
            ),
        ));
    }

    let ids: BTreeMap<String, String> = template
        .entries
        .iter()
        .map(|e| (e.id.clone(), format!("[{}]", e.resource_id)))
        .collect();
    for entry in &mut template.entries {
        entry.depends_on = entry.depends.iter().map(|id| ids[id].clone()).collect();
    }
    Ok(())
}

/// The first circle of dependencies, as the ids along it with the first one
/// repeated at the end.
fn find_cycle(template: &Template) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unseen,
        Open,
        Done,
    }
    let index: BTreeMap<&str, usize> = template
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.as_str(), i))
        .collect();
    let mut marks = vec![Mark::Unseen; template.entries.len()];

    fn visit(
        at: usize,
        template: &Template,
        index: &BTreeMap<&str, usize>,
        marks: &mut [Mark],
        path: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        marks[at] = Mark::Open;
        path.push(at);
        for id in &template.entries[at].depends {
            let Some(&next) = index.get(id.as_str()) else {
                continue;
            };
            match marks[next] {
                Mark::Open => {
                    let start = path.iter().position(|&p| p == next).unwrap_or(0);
                    let mut cycle = path[start..].to_vec();
                    cycle.push(next);
                    return Some(cycle);
                }
                Mark::Unseen => {
                    if let Some(cycle) = visit(next, template, index, marks, path) {
                        return Some(cycle);
                    }
                }
                Mark::Done => {}
            }
        }
        path.pop();
        marks[at] = Mark::Done;
        None
    }

    for start in 0..template.entries.len() {
        if marks[start] == Mark::Unseen {
            if let Some(cycle) = visit(start, template, &index, &mut marks, &mut Vec::new()) {
                return Some(
                    cycle
                        .into_iter()
                        .map(|i| template.entries[i].id.clone())
                        .collect(),
                );
            }
        }
    }
    None
}
