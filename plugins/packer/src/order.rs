//! Stage two: Packer's ordering semantics.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{refuse_at, Kind, Template};

/// Checks every edge against the way a build runs, and links post-processors
/// into the sequences their dependencies describe.
///
/// A build has a fixed shape: Packer installs plugins, reads variables, looks
/// up data, starts every source, runs the provisioners in the order they are
/// written, and then post-processes what was built. The blocks already arrive
/// in an order that satisfies every dependency, and provisioners are written
/// in that order, so a dependency between two provisioners needs nothing more.
/// A dependency that only restates the shape is accepted and adds nothing. One
/// that contradicts it cannot be honoured and is refused, since writing the
/// template anyway would run the steps in an order the author ruled out.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    template: &mut Template,
) -> Result<(), CompileError> {
    // For each post-processor, the one whose artifact it takes, and the one
    // that takes its own.
    let mut input: BTreeMap<String, String> = BTreeMap::new();
    let mut taken_by: BTreeMap<String, String> = BTreeMap::new();

    for edge in &graph.edges {
        let (Some(from), Some(to)) = (template.block(&edge.from), template.block(&edge.to)) else {
            let missing = if template.block(&edge.from).is_none() {
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

        if edge.kind == EdgeKind::Notify {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?} with action {:?}, which this target cannot express: \
                     a Packer build runs each step once on a machine it then discards, so \
                     nothing in it can react to a change",
                    edge.to, edge.action
                ),
            ));
        }

        let unsupported =
            |msg: String| refuse_at(&edge.to, &edge.pos, &rules::UNSUPPORTED_DEPENDENCY, msg);

        if from.kind == Kind::ErrorCleanup {
            return Err(unsupported(format!(
                "depends on {:?}, the error-cleanup provisioner, which only runs when the \
                 build has already failed; nothing can be ordered after it",
                from.name
            )));
        }
        if from.kind.stage() < to.kind.stage() {
            // Already how a build runs.
            continue;
        }
        if from.kind.stage() > to.kind.stage() {
            return Err(unsupported(format!(
                "is a {} and depends on {:?}, a {}: Packer always deals with a {} first, \
                 and that order cannot be changed",
                to.kind.noun(),
                from.name,
                from.kind.noun(),
                to.kind.noun(),
            )));
        }

        match to.kind {
            // Provisioners are written in the order the blocks arrive in, and
            // the error-cleanup provisioner runs after whichever one failed.
            Kind::Provisioner | Kind::ErrorCleanup => {}
            Kind::PostProcessor => {
                if let Some(other) = input.get(&edge.to) {
                    return Err(unsupported(format!(
                        "depends on the post-processors {other:?} and {:?}, but a \
                         post-processor takes the artifact of one step only",
                        edge.from
                    )));
                }
                if let Some(other) = taken_by.get(&edge.from) {
                    return Err(unsupported(format!(
                        "depends on the post-processor {:?}, whose artifact already goes to \
                         {other:?}. A sequence does not fork: declare {:?} a second time \
                         under another id to start a second sequence",
                        edge.from, edge.from
                    )));
                }
                input.insert(edge.to.clone(), edge.from.clone());
                taken_by.insert(edge.from.clone(), edge.to.clone());
            }
            Kind::Data | Kind::Local => {
                if !to.refs.contains(&edge.from) {
                    return Err(unsupported(format!(
                        "depends on {:?} without reading anything from it. Packer orders data \
                         sources and locals by the references between them alone; refer to it \
                         in a param, as ${{{}}}, or remove the dependency",
                        from.name, from.name
                    )));
                }
            }
            Kind::Source => {
                return Err(unsupported(format!(
                    "depends on the source {:?}, but Packer builds every source of a build at \
                     the same time. To build one image from another, put them in two documents \
                     and pass the first one's output to the second",
                    from.name
                )))
            }
            Kind::Plugin | Kind::Variable => {
                return Err(unsupported(format!(
                    "depends on {:?}, but a {} is not something Packer orders",
                    from.name,
                    to.kind.noun()
                )))
            }
        }
    }

    // A sequence starts at each post-processor that takes the build's own
    // artifact, in the order they arrive, and follows the links from there.
    let index_of: BTreeMap<&str, usize> = template
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.as_str(), i))
        .collect();
    let mut chains = Vec::new();
    for (i, block) in template.blocks.iter().enumerate() {
        if block.kind != Kind::PostProcessor || input.contains_key(&block.name) {
            continue;
        }
        let mut chain = vec![i];
        let mut last = block.name.as_str();
        while let Some(next) = taken_by.get(last) {
            chain.push(index_of[next.as_str()]);
            last = next;
        }
        chains.push(chain);
    }
    template.chains = chains;
    Ok(())
}
