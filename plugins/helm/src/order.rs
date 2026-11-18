//! Stage two: what Helm can and cannot do with the document's edges.

use meridian_sdk::rules;
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Warning};

use crate::{refuse_at, Chart, Node, NAME};

/// The order Helm creates the objects of a release in, by kind. A kind that
/// is not listed is created after all of these.
const INSTALL_ORDER: &[&str] = &[
    "PriorityClass",
    "Namespace",
    "NetworkPolicy",
    "ResourceQuota",
    "LimitRange",
    "PodSecurityPolicy",
    "PodDisruptionBudget",
    "ServiceAccount",
    "Secret",
    "SecretList",
    "ConfigMap",
    "StorageClass",
    "PersistentVolume",
    "PersistentVolumeClaim",
    "CustomResourceDefinition",
    "ClusterRole",
    "ClusterRoleList",
    "ClusterRoleBinding",
    "ClusterRoleBindingList",
    "Role",
    "RoleList",
    "RoleBinding",
    "RoleBindingList",
    "Service",
    "DaemonSet",
    "Pod",
    "ReplicationController",
    "ReplicaSet",
    "Deployment",
    "HorizontalPodAutoscaler",
    "StatefulSet",
    "Job",
    "CronJob",
    "IngressClass",
    "Ingress",
    "APIService",
];

/// The kinds that own pods through a template, which a restart rolls.
const WORKLOADS: &[&str] = &["DaemonSet", "Deployment", "StatefulSet"];

/// Where a kind falls in Helm's install order.
fn rank(kind: &str) -> usize {
    INSTALL_ORDER
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(INSTALL_ORDER.len())
}

/// Checks every dependency against the order Helm will use, and compiles
/// every notification into a checksum on the notified workload.
///
/// Helm sends a release's objects to the cluster in one pass, sorted by kind,
/// and nothing in a chart can change that. A dependency that order already
/// honours needs nothing written. One it does not is refused, since compiling
/// it away would promise an order the tool will not keep.
pub(crate) fn apply_edges(
    graph: &ResourceGraph,
    chart: &mut Chart,
) -> Result<Vec<Warning>, CompileError> {
    let mut warnings = Vec::new();
    // A checksum is taken by rendering the watched object's template, so two
    // workloads that restart each other would render each other without end.
    let notifies: Vec<(&str, &str)> = graph
        .edges_of_kind(EdgeKind::Notify)
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    for edge in graph.edges_of_kind(EdgeKind::Notify) {
        if reaches(&notifies, &edge.to, &edge.from) {
            return Err(refuse_at(
                &edge.from,
                &edge.pos,
                &rules::UNSUPPORTED_ACTION,
                format!(
                    "notifies resource {:?}, which in turn notifies it back: a restart is \
                     triggered by a checksum of the watched object, and two objects cannot \
                     each hold a checksum of the other",
                    edge.to
                ),
            ));
        }
    }
    for edge in &graph.edges {
        let (Some(from), Some(to)) = (chart.manifest(&edge.from), chart.manifest(&edge.to)) else {
            return Err(refuse_at(
                if edge.kind == EdgeKind::Notify {
                    &edge.from
                } else {
                    &edge.to
                },
                &edge.pos,
                &rules::UNSUPPORTED_DEPENDENCY,
                "a dependency or a notification must be between two objects of the chart; the \
                 chart's settings are not objects"
                    .to_owned(),
            ));
        };
        let (from_id, from_kind) = (from.id.clone(), from.kind.clone());
        let (to_id, to_kind) = (to.id.clone(), to.kind.clone());

        match edge.kind {
            EdgeKind::HardOrder => {
                if rank(&from_kind) >= rank(&to_kind) {
                    return Err(refuse_at(
                        &to_id,
                        &edge.pos,
                        &rules::UNSUPPORTED_DEPENDENCY,
                        format!(
                            "depends on {from_id:?}, an order this target cannot keep: Helm \
                             creates everything in a release in one pass, sorted by kind, and \
                             it does not create a {from_kind} before a {to_kind}. Kubernetes \
                             retries until what an object needs exists, so the dependency can \
                             usually be removed"
                        ),
                    ));
                }
            }
            EdgeKind::Notify => {
                if edge.action != "restart" {
                    return Err(refuse_at(
                        &from_id,
                        &edge.pos,
                        &rules::UNSUPPORTED_ACTION,
                        format!(
                            "notifies resource {to_id:?} with action {:?}, which this target \
                             cannot express: the one thing a chart can do when an object \
                             changes is roll a workload's pods (action \"restart\")",
                            edge.action
                        ),
                    ));
                }
                if !WORKLOADS.contains(&to_kind.as_str()) {
                    return Err(refuse_at(
                        &from_id,
                        &edge.pos,
                        &rules::UNSUPPORTED_ACTION,
                        format!(
                            "notifies resource {to_id:?} to restart, but a {to_kind} has no \
                             pods to roll (restartable: {})",
                            WORKLOADS.join(", ")
                        ),
                    ));
                }
                let checksum = Node::Action(format!(
                    "{{{{ include (print $.Template.BasePath \"/{from_id}.yaml\") . | sha256sum | quote }}}}"
                ));
                let target = chart
                    .manifests
                    .iter_mut()
                    .find(|m| m.id == to_id)
                    .expect("the notified object was found above");
                let annotated = set(
                    &mut target.body,
                    &["spec", "template", "metadata", "annotations"],
                    format!("checksum/{from_id}"),
                    checksum,
                );
                if !annotated {
                    return Err(refuse_at(
                        &from_id,
                        &edge.pos,
                        &rules::UNSUPPORTED_ACTION,
                        format!(
                            "notifies resource {to_id:?} to restart, but its \
                             spec.template.metadata.annotations is not a mapping the checksum \
                             that triggers the restart can be written into"
                        ),
                    ));
                }
                warnings.push(Warning {
                    target: NAME.to_owned(),
                    resource: to_id.clone(),
                    msg: format!(
                        "restarts by rolling its pods whenever {from_id:?} changes: Helm has \
                         no handlers, so a checksum of that object is kept on the pod template \
                         as the annotation checksum/{from_id}"
                    ),
                });
            }
        }
    }
    Ok(warnings)
}

/// Whether following notifications from `start` arrives at `goal`.
fn reaches(notifies: &[(&str, &str)], start: &str, goal: &str) -> bool {
    let mut seen = vec![start];
    let mut next = 0;
    while let Some(&at) = seen.get(next) {
        next += 1;
        for (from, to) in notifies {
            if *from == at {
                if *to == goal {
                    return true;
                }
                if !seen.contains(to) {
                    seen.push(to);
                }
            }
        }
    }
    false
}

/// Sets `key` in the mapping at `path`, creating the mappings on the way.
/// Returns false when something on the way is not a mapping.
fn set(fields: &mut Vec<(String, Node)>, path: &[&str], key: String, value: Node) -> bool {
    let Some((first, rest)) = path.split_first() else {
        fields.retain(|(k, _)| *k != key);
        fields.push((key, value));
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        return true;
    };
    if !fields.iter().any(|(k, _)| k == first) {
        fields.push(((*first).to_owned(), Node::Map(Vec::new())));
        // `metadata` stays first at the top of an object; everywhere else the
        // fields are in sorted order, and this keeps them so.
        let from = usize::from(fields.first().is_some_and(|(k, _)| k == "metadata"));
        fields[from..].sort_by(|a, b| a.0.cmp(&b.0));
    }
    match fields.iter_mut().find(|(k, _)| k == first) {
        Some((_, Node::Map(inner))) => set(inner, rest, key, value),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helm_creates_what_others_need_first() {
        assert!(rank("Namespace") < rank("ConfigMap"));
        assert!(rank("Secret") < rank("Deployment"));
        assert!(rank("ConfigMap") < rank("Deployment"));
        assert!(rank("Service") < rank("Deployment"));
        assert!(rank("Deployment") < rank("Ingress"));
        assert_eq!(rank("Certificate"), rank("Widget"));
        assert!(rank("Ingress") < rank("Certificate"));
    }

    #[test]
    fn a_notification_cycle_is_found_however_long() {
        let notifies = [("a", "b"), ("b", "c"), ("c", "a"), ("x", "y")];
        assert!(reaches(&notifies, "b", "a"));
        assert!(reaches(&notifies, "a", "a"));
        assert!(!reaches(&notifies, "y", "x"));
        assert!(!reaches(&notifies, "a", "x"));
    }

    #[test]
    fn setting_a_nested_key_creates_the_way_and_keeps_fields_sorted() {
        let text = |s: &str| Node::Text(s.to_owned());
        let mut body = vec![
            ("metadata".to_owned(), Node::Map(Vec::new())),
            (
                "spec".to_owned(),
                Node::Map(vec![("replicas".to_owned(), text("2"))]),
            ),
        ];
        assert!(set(
            &mut body,
            &["spec", "template", "metadata", "annotations"],
            "checksum/b".to_owned(),
            text("B"),
        ));
        assert!(set(
            &mut body,
            &["spec", "template", "metadata", "annotations"],
            "checksum/a".to_owned(),
            text("A"),
        ));
        let Node::Map(spec) = &body[1].1 else {
            panic!()
        };
        assert_eq!(spec[0].0, "replicas");
        let Node::Map(template) = &spec[1].1 else {
            panic!()
        };
        let Node::Map(metadata) = &template[0].1 else {
            panic!()
        };
        assert_eq!(
            metadata[0].1,
            Node::Map(vec![
                ("checksum/a".to_owned(), text("A")),
                ("checksum/b".to_owned(), text("B")),
            ])
        );
    }

    #[test]
    fn a_key_cannot_be_set_through_something_that_is_not_a_mapping() {
        let mut body = vec![("spec".to_owned(), Node::Action("{{ x }}".to_owned()))];
        assert!(!set(
            &mut body,
            &["spec", "template"],
            "k".to_owned(),
            Node::Text("v".to_owned()),
        ));
    }
}
