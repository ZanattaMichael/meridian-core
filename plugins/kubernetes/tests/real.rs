//! The plugin against a real cluster: plan, apply, outputs and the restart a
//! notify promises, with nothing standing in for `kubectl`.
//!
//! The tests run when `MERIDIAN_TEST_REAL_KUBECTL=1`, against whatever
//! cluster the current kubeconfig context names. A `kind` cluster is enough,
//! and is what the `kubernetes` workflow starts:
//!
//! ```sh
//! kind create cluster --wait 180s
//! MERIDIAN_TEST_REAL_KUBECTL=1 cargo test -p meridian-target-kubernetes --test real
//! ```
//!
//! Without the variable they pass without doing anything. With it, a cluster
//! that cannot be reached is a failure. Everything is created in the
//! namespace `meridian-real`, which is deleted before and after.

#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{graph_from_text, repo_root, scratch, KeptLog};
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, NoLog, Output, Plan, RunContext, Runner,
};
use meridian_target_kubernetes::{Kubernetes, KubernetesRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const DOCUMENT: &str = "plugins/kubernetes/tests/documents/workload.yaml";
const NAMESPACE: &str = "meridian-real";

/// One `kubectl` call of the test's own, returning what it printed.
fn kubectl(args: &[&str]) -> Result<String, String> {
    let finished = Tool::new("kubectl")
        .args(args.iter().copied())
        .capture_stdout()
        .run(&NoLog)
        .map_err(|e| e.to_string())?;
    if finished.success() {
        Ok(finished.stdout)
    } else {
        Err(format!("{}{}", finished.stdout, finished.stderr))
    }
}

/// Whether to run. Asked for and no cluster is a failure, not a skip.
fn cluster() -> bool {
    if std::env::var("MERIDIAN_TEST_REAL_KUBECTL").ok().as_deref() != Some("1") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_KUBECTL=1 to run against a real cluster");
        return false;
    }
    if let Err(why) = kubectl(&["version", "--output", "json"]) {
        panic!("MERIDIAN_TEST_REAL_KUBECTL=1, and the cluster cannot be reached: {why}");
    }
    true
}

/// Deletes the test's namespace when the test ends, however it ends.
struct TearDown;

impl Drop for TearDown {
    fn drop(&mut self) {
        delete_namespace();
    }
}

fn delete_namespace() {
    let _ = kubectl(&[
        "delete",
        "namespace",
        NAMESPACE,
        "--ignore-not-found",
        "--wait=true",
        "--timeout=180s",
    ]);
}

fn compile(text: &str) -> Artifact {
    Kubernetes
        .emit(&graph_from_text(DOCUMENT, text, "kubernetes"), &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "workload".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn plan(dir: &Path, artifact: &Artifact) -> (Plan, Vec<String>) {
    let log = KeptLog::default();
    let plan = KubernetesRunner::from_env()
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    (plan, log.text())
}

fn apply(dir: &Path, artifact: &Artifact) -> Applied {
    let log = KeptLog::default();
    KubernetesRunner::from_env()
        .apply(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn changes(list: &[Change]) -> Vec<(ChangeAction, &str)> {
    let mut seen: Vec<_> = list
        .iter()
        .map(|c| (c.action, c.resource.as_str()))
        .collect();
    seen.sort_by_key(|(_, resource)| *resource);
    seen
}

fn output<'a>(outputs: &'a [Output], name: &str) -> &'a str {
    outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name:?}"))
        .value
        .as_str()
        .expect("an output read from the cluster is text")
}

/// Waits for the Deployment's rollout to finish and names its pods.
fn pods_once_rolled_out() -> Vec<String> {
    kubectl(&[
        "rollout",
        "status",
        "deployment/web",
        "--namespace",
        NAMESPACE,
        "--timeout=240s",
    ])
    .unwrap_or_else(|why| panic!("the rollout did not finish: {why}"));
    // A pod being replaced can linger while it terminates; only the ones
    // that are not being deleted belong to the current rollout.
    let listed = kubectl(&[
        "get",
        "pods",
        "--namespace",
        NAMESPACE,
        "--selector",
        "app=web",
        "--output",
        "jsonpath={range .items[*]}{.metadata.name}{\" \"}{.metadata.deletionTimestamp}{\"\\n\"}{end}",
    ])
    .unwrap_or_else(|why| panic!("the pods could not be listed: {why}"));
    let mut pods: Vec<String> = listed
        .lines()
        .filter_map(
            |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                [name] => Some(name.to_owned()),
                _ => None,
            },
        )
        .collect();
    pods.sort();
    pods
}

#[ignore = "needs a real kubectl and a cluster; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_workload_is_planned_applied_read_back_and_restarted_by_a_notify() {
    if !cluster() {
        return;
    }
    delete_namespace();
    let _tear_down = TearDown;
    let dir = scratch(TMP, "real-workload");
    let text = fs::read_to_string(repo_root().join(DOCUMENT)).unwrap();
    let first = compile(&text);

    // The namespace is created by this same document, so the cluster cannot
    // preview what goes inside it. The plan says so and is never "in sync".
    let (preview, said) = plan(&dir, &first);
    assert!(preview.unknown, "{said:?}");
    assert!(
        changes(&preview.changes).contains(&(ChangeAction::Create, "space")),
        "{:?}",
        preview.changes
    );
    assert!(
        said.iter()
            .any(|line| line.contains("cannot preview them until the namespace exists")),
        "{said:?}"
    );
    assert!(
        kubectl(&["get", "namespace", NAMESPACE]).is_err(),
        "a plan creates nothing"
    );

    // An apply creates everything, under the ids the document gave.
    let applied = apply(&dir, &first);
    assert_eq!(
        changes(&applied.changes),
        [
            (ChangeAction::Create, "space"),
            (ChangeAction::Create, "web"),
            (ChangeAction::Create, "web_config"),
        ]
    );
    assert_eq!(output(&applied.outputs, "greeting"), "hello");
    assert_eq!(output(&applied.outputs, "workload"), "meridian-real/web");
    let uid = output(&applied.outputs, "uid").to_owned();
    assert_eq!(
        uid.len(),
        36,
        "the API server assigns a uid of 36 characters"
    );

    // Outputs can be read again later, without an apply.
    let read = KubernetesRunner::from_env()
        .outputs(&context(&dir))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(output(&read, "uid"), uid);

    // With the namespace there, the preview is complete, and says nothing
    // would change.
    let (again, said) = plan(&dir, &first);
    assert!(!again.unknown, "{said:?}");
    assert!(again.changes.is_empty(), "{:?}", again.changes);

    let before = pods_once_rolled_out();
    assert_eq!(before.len(), 1, "{before:?}");

    // The ConfigMap's content changes; the Deployment's own fields do not.
    // The notify compiled to a checksum on the pod template, so the plan
    // shows the Deployment changing too.
    let second = compile(&text.replace("greeting: hello", "greeting: goodbye"));
    let (preview, _) = plan(&dir, &second);
    assert!(!preview.unknown);
    assert_eq!(
        changes(&preview.changes),
        [
            (ChangeAction::Update, "web"),
            (ChangeAction::Update, "web_config"),
        ]
    );
    let applied = apply(&dir, &second);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert_eq!(output(&applied.outputs, "greeting"), "goodbye");
    assert_eq!(
        output(&applied.outputs, "uid"),
        uid,
        "the Deployment was updated, not replaced"
    );

    // And the pods really were replaced: that is what the notify is for.
    let after = pods_once_rolled_out();
    assert_eq!(after.len(), 1, "{after:?}");
    assert_ne!(before, after, "the pod was not restarted");

    // Applying the same thing again changes nothing and restarts nothing.
    let applied = apply(&dir, &second);
    assert!(applied.changes.is_empty(), "{:?}", applied.changes);
    assert_eq!(pods_once_rolled_out(), after);
}

#[ignore = "needs a real kubectl and a cluster; see docs/contributing/real-tool-tests.md"]
#[test]
fn what_the_api_server_refuses_fails_the_plan_with_its_reason() {
    if !cluster() {
        return;
    }
    let dir = scratch(TMP, "real-refused");
    // The compile cannot know that a ConfigMap's data must be strings; only
    // the API server, which has the schema, can.
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: workload\nspec:\n  target: kubernetes\n  resources:\n    - id: target\n      type: cluster\n      params:\n        namespace: default\n    - id: broken\n      type: v1/ConfigMap\n      params:\n        metadata:\n          name: meridian-real-broken\n        data:\n          nested:\n            not: a string\n";
    let artifact = compile(text);
    let error = KubernetesRunner::from_env()
        .plan(&context(&dir), &artifact, &NoLog)
        .expect_err("the API server refuses the object");
    assert_eq!(error.msg, "kubectl could not preview the apply");
    assert!(error.exit_code.is_some_and(|code| code != 0));
    assert!(error.output.contains("ConfigMap"), "{}", error.output);
    assert!(
        kubectl(&[
            "get",
            "configmap",
            "meridian-real-broken",
            "--namespace",
            "default"
        ])
        .is_err(),
        "nothing was stored"
    );
}
