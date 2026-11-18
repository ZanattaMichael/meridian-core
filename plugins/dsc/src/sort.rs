//! Stage two: the runner's ordering and notification semantics.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{refuse_at, Configuration};

/// The runner sorts resources by `dependsOn` itself, so every dependency is
/// written down and the graph survives into the output. A `notify` already
/// orders the notifier first, and so does reading a resource, so a
/// `dependsOn` that says the same thing is left out.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    configuration: &mut Configuration,
) -> Result<(), CompileError> {
    let position = |configuration: &Configuration, id: &str| {
        configuration.instances.iter().position(|i| i.id == id)
    };

    for edge in graph.edges_of_kind(EdgeKind::Notify) {
        let (Some(from), Some(_)) = (
            position(configuration, &edge.from),
            position(configuration, &edge.to),
        ) else {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a notify must be between two DSC resources; the runner's own settings are not one"
                    .to_owned(),
            ));
        };
        if edge.action != "refresh" {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?} with action {:?}, which this target cannot express: \
                     a notified DSC resource has its Set run again, and that is the one thing \
                     a notify can ask for (supported: refresh)",
                    edge.to, edge.action
                ),
            ));
        }
        let notify = &mut configuration.instances[from].notify;
        if !notify.contains(&edge.to) {
            notify.push(edge.to.clone());
        }
    }

    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        let (Some(from), Some(to)) = (
            position(configuration, &edge.from),
            position(configuration, &edge.to),
        ) else {
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency must be between two DSC resources; the runner's own settings are \
                 not one"
                    .to_owned(),
            ));
        };
        if configuration.instances[from].notify.contains(&edge.to) {
            continue;
        }
        let depends_on = &mut configuration.instances[to].depends_on;
        if !depends_on.contains(&edge.from) {
            depends_on.push(edge.from.clone());
        }
    }

    // A resource that reads another must run after it, said or not.
    for index in 0..configuration.instances.len() {
        let id = configuration.instances[index].id.clone();
        for read in configuration.instances[index].reads.clone() {
            let notified = position(configuration, &read)
                .is_some_and(|from| configuration.instances[from].notify.contains(&id));
            let depends_on = &mut configuration.instances[index].depends_on;
            if !notified && !depends_on.contains(&read) {
                depends_on.push(read);
            }
        }
    }
    Ok(())
}
