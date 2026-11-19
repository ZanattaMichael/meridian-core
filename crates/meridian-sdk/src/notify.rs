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

/// Meridian-run notifies on AWS, for every target that deploys there: one
/// `aws` command per type, as AWS has no generic action call.
pub mod aws {
    use super::ActSchema;

    /// Each CloudFormation type and the actions Meridian performs on it.
    pub const ACTIONS: &[(&str, &[&str])] = &[
        ("AWS::EC2::Instance", &["restart", "start", "stop"]),
        ("AWS::ECS::Service", &["redeploy"]),
        ("AWS::Lambda::Function", &["run"]),
    ];

    /// What a target acts on: for each `(type as its documents write it,
    /// CloudFormation type)` in `types`, that type's actions.
    pub fn acts(types: &[(&str, &str)]) -> Vec<ActSchema> {
        types
            .iter()
            .filter_map(|(written, kind)| {
                let (_, actions) = ACTIONS.iter().find(|(k, _)| k == kind)?;
                Some(ActSchema::new(written, actions))
            })
            .collect()
    }

    /// The CloudFormation type a document's type stands for, by `types`.
    pub fn kind_of<'a>(types: &[(&str, &'a str)], written: &str) -> Option<&'a str> {
        types
            .iter()
            .find(|(w, _)| *w == written)
            .map(|(_, kind)| *kind)
    }

    /// The `aws` arguments that perform `action` on the resource of type
    /// `kind` whose physical id is `id`; `None` for an action not in
    /// [`ACTIONS`] or an id the call cannot take. A Lambda's result is
    /// written to `out`.
    pub fn args(kind: &str, id: &str, action: &str, out: &str) -> Option<Vec<String>> {
        let v = |items: &[&str]| Some(items.iter().map(|i| (*i).to_owned()).collect());
        match (kind, action) {
            ("AWS::EC2::Instance", "restart") => {
                v(&["ec2", "reboot-instances", "--instance-ids", id])
            }
            ("AWS::EC2::Instance", "start") => v(&["ec2", "start-instances", "--instance-ids", id]),
            ("AWS::EC2::Instance", "stop") => v(&["ec2", "stop-instances", "--instance-ids", id]),
            ("AWS::ECS::Service", "redeploy") => {
                let (cluster, service) = ecs_service(id)?;
                v(&[
                    "ecs",
                    "update-service",
                    "--cluster",
                    &cluster,
                    "--service",
                    &service,
                    "--force-new-deployment",
                ])
            }
            ("AWS::Lambda::Function", "run") => {
                v(&["lambda", "invoke", "--function-name", id, out])
            }
            _ => None,
        }
    }

    /// The cluster and service of an ECS service id: an ARN
    /// (`arn:aws:ecs:REGION:ACCOUNT:service/CLUSTER/NAME`, or the old
    /// `service/NAME` in the default cluster) or `CLUSTER/NAME`.
    fn ecs_service(id: &str) -> Option<(String, String)> {
        let path = match id.strip_prefix("arn:") {
            Some(_) => id.splitn(6, ':').nth(5)?.strip_prefix("service/")?,
            None => id,
        };
        match path.split('/').collect::<Vec<_>>().as_slice() {
            [cluster, name] if !cluster.is_empty() && !name.is_empty() => {
                Some(((*cluster).to_owned(), (*name).to_owned()))
            }
            [name] if !name.is_empty() => Some(("default".to_owned(), (*name).to_owned())),
            _ => None,
        }
    }

    /// Why a call that exited 0 still failed: a Lambda that raised reports
    /// `FunctionError` in what `aws lambda invoke` prints.
    pub fn failed(kind: &str, stdout: &str) -> Option<String> {
        (kind == "AWS::Lambda::Function" && stdout.contains("\"FunctionError\""))
            .then(|| "the function ran and raised an error".to_owned())
    }

    /// The region of an ARN, `arn:PARTITION:SERVICE:REGION:...`.
    pub fn region_of(arn: &str) -> Option<&str> {
        let region = arn.strip_prefix("arn:")?.split(':').nth(2)?;
        (!region.is_empty()).then_some(region)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn each_action_is_its_own_aws_call() {
            assert_eq!(
                args("AWS::EC2::Instance", "i-0abc", "restart", "out.json").unwrap(),
                ["ec2", "reboot-instances", "--instance-ids", "i-0abc"]
            );
            assert_eq!(
                args(
                    "AWS::ECS::Service",
                    "arn:aws:ecs:eu-west-1:123:service/web/api",
                    "redeploy",
                    "out.json"
                )
                .unwrap(),
                [
                    "ecs",
                    "update-service",
                    "--cluster",
                    "web",
                    "--service",
                    "api",
                    "--force-new-deployment"
                ]
            );
            assert_eq!(
                args(
                    "AWS::ECS::Service",
                    "arn:aws:ecs:eu-west-1:123:service/api",
                    "redeploy",
                    "o"
                )
                .unwrap()[3],
                "default"
            );
            assert_eq!(
                args("AWS::Lambda::Function", "migrate", "run", "out.json").unwrap(),
                ["lambda", "invoke", "--function-name", "migrate", "out.json"]
            );
            assert!(args("AWS::EC2::Instance", "i-0abc", "redeploy", "o").is_none());
            assert!(args("AWS::ECS::Service", "a/b/c", "redeploy", "o").is_none());
        }

        #[test]
        fn a_lambda_that_raised_failed_and_an_arn_has_a_region() {
            assert!(failed(
                "AWS::Lambda::Function",
                r#"{"StatusCode":200,"FunctionError":"Unhandled"}"#
            )
            .is_some());
            assert!(failed("AWS::Lambda::Function", r#"{"StatusCode":200}"#).is_none());
            assert_eq!(
                region_of("arn:aws:lambda:eu-west-1:123:function:x"),
                Some("eu-west-1")
            );
            assert_eq!(region_of("arn:aws:s3:::bucket"), None);
            assert_eq!(region_of("i-0abc"), None);
        }

        #[test]
        fn acts_list_the_actions_of_each_written_type() {
            let acts = acts(&[
                ("AWS::EC2::Instance", "AWS::EC2::Instance"),
                ("vm", "AWS::EC2::Instance"),
            ]);
            assert!(crate::notify::performs(&acts, "vm", "restart"));
            assert!(!crate::notify::performs(&acts, "vm", "redeploy"));
        }
    }
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
