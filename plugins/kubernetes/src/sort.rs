//! Stage two: Kubernetes' ordering semantics, and the synthetic notify.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Value, Warning};

use crate::{refuse_at, Manifests, NAME};

/// The workload kinds whose pods roll when their pod template changes.
const ROLLING: &[&str] = &["DaemonSet", "Deployment", "StatefulSet"];

/// The annotation a notify compiles to, on the workload's pod template.
const ANNOTATION_PREFIX: &str = "meridian.dev/restart-on-";

/// The objects stay in the tree's apply order, which is the order `kubectl`
/// sends them in. That is all the ordering Kubernetes offers, so a document
/// that declares dependencies is told what they will and will not do.
///
/// A notify becomes a checksum of the notifier, written onto the notified
/// workload's pod template: when the notifier's content changes, so does the
/// template, and the workload's own controller rolls its pods.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    manifests: &mut Manifests,
) -> Result<Vec<Warning>, CompileError> {
    let mut warnings = Vec::new();
    let mut ordered = false;

    for edge in &graph.edges {
        let position = |id: &str| manifests.objects.iter().position(|o| o.id == id);
        let (Some(from), Some(to)) = (position(&edge.from), position(&edge.to)) else {
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency must be between two objects; the cluster's own settings are not one"
                    .to_owned(),
            ));
        };
        if edge.kind == EdgeKind::HardOrder {
            ordered = true;
            continue;
        }

        if edge.action != "restart" {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?} with action {:?}, which this target cannot express: \
                     the one reaction Kubernetes can be given is to roll a workload's pods \
                     (supported: restart)",
                    edge.to, edge.action
                ),
            ));
        }
        if !ROLLING.contains(&manifests.objects[to].kind.as_str()) {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?}, a {}, which has no pods to restart (supported: {})",
                    edge.to,
                    manifests.objects[to].kind,
                    ROLLING.join(", ")
                ),
            ));
        }

        let notifier = &manifests.objects[from];
        let checksum = checksum(&Value::Map(notifier.body.clone()).canonical());
        let key = format!("{ANNOTATION_PREFIX}{}", notifier.name);
        warnings.push(Warning {
            target: NAME.to_owned(),
            resource: edge.to.clone(),
            msg: format!(
                "restarts by rolling its pods whenever the content of {:?} changes; the trigger \
                 is the annotation {key} on its pod template",
                edge.from
            ),
        });

        let object = &mut manifests.objects[to];
        let mut level = &mut object.body;
        for step in ["spec", "template", "metadata", "annotations"] {
            let next = level
                .entry(step.to_owned())
                .or_insert_with(|| Value::Map(BTreeMap::new()));
            let Value::Map(next) = next else {
                return Err(refuse_at(
                    &edge.to,
                    &edge.pos,
                    &rules::INVALID_PARAM,
                    format!("{step} of the notified workload must be a map"),
                ));
            };
            level = next;
        }
        level.insert(key, Value::String(checksum));
    }

    if ordered {
        warnings.insert(
            0,
            Warning {
                target: NAME.to_owned(),
                resource: String::new(),
                msg: "dependsOn orders the manifests in the file and nothing more; kubectl \
                      applies them in that order without waiting for one to be ready before \
                      the next"
                    .to_owned(),
            },
        );
    }
    Ok(warnings)
}

/// A short, stable fingerprint of some content (64-bit FNV-1a). It only has
/// to change when the content does; it protects nothing.
fn checksum(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_stable_and_follows_the_content() {
        assert_eq!(checksum(""), "cbf29ce484222325");
        assert_eq!(checksum("a"), "af63dc4c8601ec8c");
        assert_ne!(checksum("workers = 4"), checksum("workers = 8"));
    }
}
