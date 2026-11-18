//! The plugin against a real cluster: plan, apply, outputs, the restart a
//! notify promises and the schema export an editor reads, with nothing
//! standing in for `kubectl`.
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

use meridian_plugin_testkit::{graph_from_text, repo_root, scratch, secret_at, secrets, KeptLog};
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, NoLog, Output, ParamKind, Plan,
    RunContext, Runner, SchemaRequest, SchemaSource,
};
use meridian_target_kubernetes::{Kubernetes, KubernetesRunner, KubernetesSchemas};

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

#[ignore = "needs a real kubectl; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_api_servers_kinds_are_exported_for_an_editor() {
    if !cluster() {
        return;
    }
    let dir = scratch(TMP, "real-schemas");
    let text = fs::read_to_string(repo_root().join(DOCUMENT)).unwrap();
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text(DOCUMENT, &text, "kubernetes")],
    );
    let log = KeptLog::default();
    let export = KubernetesSchemas::from_env()
        .export(&request, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(export.tool, "kubectl");
    assert!(
        export
            .pins
            .get("server")
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())),
        "the server's version is pinned: {:?}",
        export.pins
    );

    // The kinds the document uses are described field by field, with what
    // the API server says about them.
    let deployment = export
        .schemas
        .iter()
        .find(|s| s.r#type == "apps/v1/Deployment")
        .unwrap_or_else(|| panic!("apps/v1/Deployment is described: {:?}", export.notes));
    assert!(!deployment.description.is_empty());
    assert!(
        deployment.find_param("apiVersion").is_none(),
        "given by the type"
    );
    let spec = deployment.find_param("spec").unwrap();
    assert!(spec
        .find_param("replicas")
        .is_some_and(|p| p.kind == ParamKind::Integer));
    assert!(spec.find_param("template").is_some_and(|p| p.required));
    assert!(deployment.find_param("status").is_some_and(|p| p.computed));
    let namespace = export
        .schemas
        .iter()
        .find(|s| s.r#type == "v1/Namespace")
        .unwrap();
    assert!(namespace
        .find_param("metadata")
        .unwrap()
        .find_param("name")
        .is_some());

    // Every other kind the server knows is offered by name.
    let service = export
        .schemas
        .iter()
        .find(|s| s.r#type == "v1/Service")
        .unwrap();
    assert!(service.open && service.params.is_empty());
    assert!(
        export
            .schemas
            .iter()
            .any(|s| s.r#type == "rbac.authorization.k8s.io/v1/Role"),
        "a kind of another group is listed"
    );
    assert!(!export.schemas.iter().any(|s| s.r#type.ends_with("List")));
}

/// Unlike anything the cluster or the plugin prints, with the characters a
/// JSON string escapes, so that finding it is finding a leak.
const SECRET: &str = "zzq-Vt9 \"not\" a\\real $tok'en 4kLm";

#[ignore = "needs a real kubectl and a cluster; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_the_cluster_as_a_secret_and_is_in_no_file() {
    if !cluster() {
        return;
    }
    let _ = kubectl(&[
        "delete",
        "secret",
        "meridian-real-login",
        "--ignore-not-found",
    ]);
    struct Remove;
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = kubectl(&[
                "delete",
                "secret",
                "meridian-real-login",
                "--ignore-not-found",
            ]);
        }
    }
    let _remove = Remove;
    let dir = scratch(TMP, "real-secret");
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: login\nspec:\n  target: kubernetes\n  resources:\n    - id: target\n      type: cluster\n      params:\n        namespace: default\n    - id: login\n      type: v1/Secret\n      params:\n        metadata:\n          name: meridian-real-login\n        stringData:\n          user: app\n          password: placeholder\n";
    let mut graph = graph_from_text("login.yaml", text, "kubernetes");
    let slot = secret_at(&mut graph, "login", &["stringData", "password"], "token");
    let artifact = Kubernetes.emit(&graph, &Data::new()).unwrap().artifact;
    assert!(!artifact.files["manifests.yaml"].contains(SECRET));

    let mut context = context(&dir);
    context.secrets = secrets(&[(slot, "token", SECRET)]);
    let log = KeptLog::default();
    let runner = KubernetesRunner::from_env();
    let plan = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(changes(&plan.changes), [(ChangeAction::Create, "login")]);
    let applied = runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(changes(&applied.changes), [(ChangeAction::Create, "login")]);

    // The cluster holds the value, as a Secret.
    let encoded = kubectl(&[
        "get",
        "secret",
        "meridian-real-login",
        "--namespace",
        "default",
        "--output",
        "jsonpath={.data.password}",
    ])
    .unwrap();
    let decoded = Tool::new("base64")
        .arg("--decode")
        .stdin(encoded.trim().to_owned())
        .capture_stdout()
        .run_ok("base64", &NoLog)
        .unwrap()
        .stdout;
    assert_eq!(decoded, SECRET);

    // Nothing printed holds it, and no file does: the manifests in the work
    // directory hold the token, as the artifact does.
    assert!(
        !log.text().iter().any(|l| l.contains(SECRET)),
        "the secret reached the log:\n{}",
        log.text().join("\n")
    );
    for entry in fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(SECRET),
                "{} holds the secret in the open",
                path.display()
            );
        }
    }
    assert!(fs::read_to_string(dir.join("work").join("manifests.yaml"))
        .unwrap()
        .contains("\"${meridian_secret_token}\""));

    // A second plan sees nothing to change.
    let plan = runner
        .plan(&context, &artifact, &KeptLog::default())
        .unwrap();
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
}
