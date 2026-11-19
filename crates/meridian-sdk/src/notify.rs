//! Meridian-run notifies: a notify whose tool cannot express it, performed
//! by the plugin's runner after the stage applies (decision 0056).
//!
//! A runner lists what it acts on in [`Runner::acts`](crate::Runner::acts).
//! The host takes each notify it covers out of the tree before `emit`, so the
//! emitter never refuses it, and keeps it as a [`MeridianRun`] to trigger
//! after the apply when its notifier changed.

use crate::{ChangeAction, EdgeKind, ResourceGraph, Warning};

/// The notify actions a runner performs on one resource type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActSchema {
    /// The type as a document writes it, or a glob with `*`.
    pub r#type: String,
    pub actions: Vec<String>,
}

impl ActSchema {
    pub fn new(r#type: &str, actions: &[&str]) -> ActSchema {
        ActSchema {
            r#type: r#type.to_owned(),
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    /// Whether this covers `action` on a resource of type `ty`.
    pub fn covers(&self, ty: &str, action: &str) -> bool {
        glob(&self.r#type, ty) && self.actions.iter().any(|a| a == action)
    }
}

/// Whether any of `acts` covers `action` on type `ty`.
pub fn performs(acts: &[ActSchema], ty: &str, action: &str) -> bool {
    acts.iter().any(|a| a.covers(ty, action))
}

/// A notify Meridian runs: `action` on `resource` when `notifier` changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MeridianRun {
    pub notifier: String,
    pub resource: String,
    pub action: String,
}

/// Takes out of `graph` every notify edge `acts` covers, and returns them,
/// in the order the edges were declared, with the warning each one gets.
pub fn take_meridian_run(
    graph: &mut ResourceGraph,
    acts: &[ActSchema],
) -> (Vec<MeridianRun>, Vec<Warning>) {
    if acts.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let type_of = |id: &str| {
        graph
            .resources
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.r#type.clone())
            .unwrap_or_default()
    };
    let mut taken = Vec::new();
    let mut warnings = Vec::new();
    let mut kept = Vec::new();
    for edge in std::mem::take(&mut graph.edges) {
        if edge.kind == EdgeKind::Notify && performs(acts, &type_of(&edge.to), &edge.action) {
            warnings.push(Warning {
                target: graph.target.clone(),
                resource: edge.to.clone(),
                msg: format!(
                    "Meridian runs {} on {} after the stage applies, when {} changes; the tool's \
                     own plan and state do not show it",
                    edge.action, edge.to, edge.from
                ),
            });
            taken.push(MeridianRun {
                notifier: edge.from,
                resource: edge.to,
                action: edge.action,
            });
        } else {
            kept.push(edge);
        }
    }
    graph.edges = kept;
    (taken, warnings)
}

/// Whether a change triggers a notify: a create, update or replace does; an
/// adopt or a delete does not.
pub fn triggers(action: ChangeAction) -> bool {
    matches!(
        action,
        ChangeAction::Create | ChangeAction::Update | ChangeAction::Replace
    )
}

/// Matches `text` against `pattern`, where `*` stands for any run of
/// characters.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text.ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edge, Resource};

    #[test]
    fn a_glob_matches_any_run_of_characters() {
        assert!(glob("Microsoft.*/*", "Microsoft.Web/sites"));
        assert!(glob("*", "anything"));
        assert!(glob("file", "file"));
        assert!(!glob("file", "files"));
        assert!(!glob("Microsoft.*/*", "AWS::EC2::Instance"));
        assert!(!glob("a*a", "a"));
    }

    #[test]
    fn only_a_covered_notify_is_taken_out_of_the_tree() {
        let resource = |id: &str, ty: &str| Resource {
            id: id.to_owned(),
            r#type: ty.to_owned(),
            ..Resource::default()
        };
        let notify = |from: &str, to: &str, action: &str| Edge {
            from: from.to_owned(),
            to: to.to_owned(),
            kind: EdgeKind::Notify,
            action: action.to_owned(),
            pos: Default::default(),
        };
        let mut graph = ResourceGraph {
            target: "arm".to_owned(),
            resources: vec![resource("settings", "config"), resource("web", "site")],
            edges: vec![
                notify("settings", "web", "restart"),
                notify("settings", "web", "explode"),
                Edge {
                    kind: EdgeKind::HardOrder,
                    ..notify("settings", "web", "")
                },
            ],
            ..ResourceGraph::default()
        };
        let (taken, warnings) =
            take_meridian_run(&mut graph, &[ActSchema::new("site", &["restart"])]);
        assert_eq!(
            taken,
            vec![MeridianRun {
                notifier: "settings".to_owned(),
                resource: "web".to_owned(),
                action: "restart".to_owned(),
            }]
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(graph.edges.len(), 2);
    }
}
