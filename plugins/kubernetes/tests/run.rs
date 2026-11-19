//! The running half, against a stand-in for `kubectl`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use meridian_plugin_testkit::{artifact, scratch, secrets, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, SecretRef, Value};
use meridian_target_kubernetes::KubernetesRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("manifests.yaml", "---\napiVersion: v1\nkind: Service\n"),
        (
            "cluster.json",
            r#"{"context":"prod","namespace":"web","kubeconfig":"",
                "objects":{"deployment.apps/web":"web","service/web":"web_svc"},
                "outputs":[{"name":"web_url","parts":[
                  {"text":"http://"},
                  {"get":"service","name":"web","namespace":"","path":".status.loadBalancer.ingress[0].ip"}]},
                 {"name":"token","parts":[
                  {"get":"secret","name":"api","namespace":"vault","path":".data.token"}]}]}"#,
        ),
    ])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that appends each call to `calls` and answers by its first two
/// words: `apply` or `get <type>`.
fn runner(dir: &Path, cases: &str) -> KubernetesRunner {
    let program = stand_in(
        dir,
        "kubectl",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\ncase \"$1 $2\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    KubernetesRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
        output_wait: Duration::from_secs(5),
        output_poll: Duration::from_millis(20),
    }
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

const REPORT: &str = "echo 'configmap/web-config unchanged'; echo 'deployment.apps/web configured'; echo 'service/web created'";
const GETS: &str = "'get service') echo 203.0.113.7 ;;\n'get secret') echo c2VjcmV0 ;;";

#[test]
fn a_plan_is_a_server_side_dry_run() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let cases = "apply*) echo 'configmap/web-config unchanged (server dry run)'; echo 'deployment.apps/web configured (server dry run)'; echo 'service/web created (server dry run)' ;;";
    let plan = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        ["apply --dry-run=server --filename manifests.yaml --context prod --namespace web"]
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            "update web: deployment.apps/web",
            "create web_svc: service/web"
        ]
    );
    assert!(!plan.unknown);
    assert!(dir.join("work/manifests.yaml").exists());
}

#[test]
fn objects_in_a_namespace_the_document_creates_make_the_plan_unknown() {
    let dir = scratch(TMP, "namespace");
    let cases = r#"apply*) echo 'namespace/web created (server dry run)'
  echo 'Error from server (NotFound): error when creating "manifests.yaml": namespaces "web" not found' >&2
  exit 1 ;;"#;
    let plan = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.unknown);
    assert_eq!(plan.changes.len(), 1);

    // Any other failure is a failure.
    let dir = scratch(TMP, "refused");
    let cases = r#"apply*) echo 'namespace/web created (server dry run)'
  echo 'Error from server (Forbidden): deployments.apps is forbidden' >&2
  exit 1 ;;"#;
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "kubectl could not preview the apply");
    assert!(err.output.contains("Forbidden"), "{}", err.output);
}

#[test]
fn an_apply_reports_what_kubectl_did_and_reads_the_outputs() {
    let dir = scratch(TMP, "apply");
    let applied = runner(&dir, &format!("apply*) {REPORT} ;;\n{GETS}"))
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "apply --filename manifests.yaml --context prod --namespace web",
            "get service web --output jsonpath={.status.loadBalancer.ingress[0].ip} --context prod --namespace web",
            // An object that sets its own namespace is read from it.
            "get secret api --output jsonpath={.data.token} --context prod --namespace vault",
        ]
    );
    assert_eq!(applied.changes.len(), 2);
    let url = &applied.outputs[0];
    assert_eq!(url.name, "web_url");
    assert_eq!(url.value, Value::String("http://203.0.113.7".into()));
    assert!(!url.sensitive);
    // Anything read from a Secret is sensitive.
    assert!(applied.outputs[1].sensitive);
}

#[test]
fn an_apply_waits_for_an_output_the_cluster_has_not_filled_in() {
    let dir = scratch(TMP, "waits");
    // The address appears on the third look.
    let cases = format!(
        "apply*) {REPORT} ;;\n'get service') echo x >> \"{dir}/looks\"; if [ \"$(wc -l < \"{dir}/looks\")\" -ge 3 ]; then echo 203.0.113.7; else echo 'error: array index out of bounds: index 0, length 0' >&2; exit 1; fi ;;\n'get secret') echo c2VjcmV0 ;;",
        dir = dir.display()
    );
    let log = KeptLog::default();
    let applied = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(), &log)
        .unwrap();
    assert_eq!(
        applied.outputs[0].value,
        Value::String("http://203.0.113.7".into())
    );
    assert!(log.text().iter().any(|l| l
        == "output web_url: waiting for service/web to have .status.loadBalancer.ingress[0].ip"));
}

#[test]
fn an_output_that_never_arrives_is_an_error_and_not_an_empty_value() {
    let dir = scratch(TMP, "never");
    let mut kubectl = runner(
        &dir,
        &format!("apply*) {REPORT} ;;\n'get service') true ;;"),
    );
    kubectl.output_wait = Duration::from_millis(60);
    let err = kubectl
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with(
            "output \"web_url\": service/web has no value at .status.loadBalancer.ingress[0].ip yet"
        ),
        "{}",
        err.msg
    );
}

#[test]
fn a_failed_apply_keeps_what_kubectl_said() {
    let dir = scratch(TMP, "failed");
    let cases = "apply*) echo 'error: unable to connect to the server' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "kubectl could not apply the manifests");
    assert!(err.output.contains("unable to connect"), "{}", err.output);
    assert_eq!(err.failures.len(), 1, "{:?}", err.failures);
    assert_eq!(err.failures[0].cause.as_str(), "unreachable");
    assert_eq!(err.failures[0].resource, "");
}

#[test]
fn a_refused_object_is_named_by_the_id_the_author_wrote() {
    let dir = scratch(TMP, "refused-object");
    let cases = r#"apply*) echo 'service/web unchanged'
  echo 'Error from server (Invalid): error when creating "manifests.yaml": Deployment.apps "web" is invalid: spec.replicas: Invalid value: -1' >&2
  exit 1 ;;"#;
    let err = runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    let named: Vec<(&str, &str, &str)> = err
        .failures
        .iter()
        .map(|f| (f.resource.as_str(), f.cause.as_str(), f.tool_code.as_str()))
        .collect();
    assert_eq!(named, [("web", "invalid-config", "Invalid")]);
}

#[test]
fn outputs_are_read_from_the_live_objects() {
    let dir = scratch(TMP, "outputs");
    let kubectl = runner(&dir, GETS);
    assert!(kubectl.outputs(&context(&dir)).is_err());
    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(
        dir.join("work/cluster.json"),
        &compiled().files["cluster.json"],
    )
    .unwrap();
    let outputs = kubectl.outputs(&context(&dir)).unwrap();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[1].value, Value::String("c2VjcmV0".into()));
}

/// Unlike anything else the tests write, with the characters a JSON string
/// has to escape.
const SECRET: &str = "hunter2 \"q\" $5 it's \\";

#[test]
fn secrets_reach_kubectl_on_standard_input_and_no_file_holds_them() {
    let dir = scratch(TMP, "secrets");
    let d = dir.display();
    let log = KeptLog::default();
    let manifests = "---\napiVersion: v1\nkind: Secret\nmetadata:\n  name: db-login\nstringData:\n  password: \"${meridian_secret_db_password}\"\n  user: app\n";
    let artifact = artifact(&[
        ("manifests.yaml", manifests),
        (
            "cluster.json",
            r#"{"context":"prod","namespace":"web","kubeconfig":"","objects":{"secret/db-login":"db_login"},"outputs":[]}"#,
        ),
    ]);
    let cases = format!(
        "'apply --dry-run=server') cat > \"{d}/stdin-plan\"; echo 'secret/db-login created (server dry run)' ;;\n'apply --filename') cat > \"{d}/stdin-apply\"; echo 'secret/db-login created' ;;"
    );
    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", SECRET)]);
    let runner = runner(&dir, &cases);

    let plan = runner.plan(&context, &artifact, &log).unwrap();
    assert_eq!(plan.changes.len(), 1);
    let applied = runner.apply(&context, &artifact, &log).unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(
        calls(&dir),
        [
            "apply --dry-run=server --filename - --context prod --namespace web",
            "apply --filename - --context prod --namespace web",
        ]
    );
    // What kubectl read is the manifests with the value in the token's place,
    // escaped as the JSON string it sits in.
    let filled = manifests.replace(
        "${meridian_secret_db_password}",
        "hunter2 \\\"q\\\" $5 it's \\\\",
    );
    assert_eq!(fs::read_to_string(dir.join("stdin-plan")).unwrap(), filled);
    assert_eq!(fs::read_to_string(dir.join("stdin-apply")).unwrap(), filled);
    // The file in the work directory still holds the token, and nothing else
    // written anywhere holds the value.
    let on_disk = fs::read_to_string(dir.join("work").join("manifests.yaml")).unwrap();
    assert_eq!(on_disk, manifests);
    assert!(
        !log.text().join("\n").contains("hunter2"),
        "{:?}",
        log.text()
    );
    assert!(!fs::read_to_string(dir.join("calls"))
        .unwrap()
        .contains("hunter2"));
}

#[test]
fn without_secrets_kubectl_reads_the_file_as_before() {
    let dir = scratch(TMP, "no-secrets");
    let cases = "apply*) echo 'service/web created' ;;";
    runner(&dir, cases)
        .apply(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err(); // the outputs cannot be read by this stand-in
    assert_eq!(
        calls(&dir)[0],
        "apply --filename manifests.yaml --context prod --namespace web"
    );
}

#[test]
fn a_debug_level_raises_kubectls_verbosity_and_level_0_adds_nothing() {
    let cases = "apply*) echo 'service/web created (server dry run)' ;;";
    for (level, expected) in [
        (
            0,
            "apply --dry-run=server --filename manifests.yaml --context prod --namespace web",
        ),
        (
            2,
            "apply --dry-run=server --filename manifests.yaml -v=6 --context prod --namespace web",
        ),
        (
            9,
            "apply --dry-run=server --filename manifests.yaml -v=8 --context prod --namespace web",
        ),
    ] {
        let dir = scratch(TMP, &format!("debug-{level}"));
        let context = RunContext {
            debug_level: level,
            ..context(&dir)
        };
        runner(&dir, cases)
            .plan(&context, &compiled(), &KeptLog::default())
            .unwrap();
        assert_eq!(calls(&dir), [expected], "level {level}");
    }
}

const DELETED: &str =
    "delete*) echo 'deployment.apps \"web\" deleted'; echo 'service \"web\" deleted' ;;";

#[test]
fn a_destroy_deletes_every_object_and_waits() {
    let dir = scratch(TMP, "destroy");
    let runner = runner(&dir, DELETED);
    assert!(runner.destroys());
    let removed = runner
        .destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        ["delete --ignore-not-found --wait --filename manifests.yaml --context prod --namespace web"]
    );
    let shown: Vec<String> = removed.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "delete web: deployment.apps/web",
            "delete web_svc: service/web"
        ]
    );
}

#[test]
fn a_destroy_preview_is_a_server_side_dry_run() {
    let dir = scratch(TMP, "destroy-preview");
    let cases = "delete*) echo 'deployment.apps \"web\" deleted (server dry run)' ;;";
    let plan = runner(&dir, cases)
        .plan_destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        ["delete --ignore-not-found --dry-run=server --filename manifests.yaml --context prod --namespace web"]
    );
    assert_eq!(
        plan.changes[0].to_string(),
        "delete web: deployment.apps/web"
    );
}

#[test]
fn a_failed_delete_says_so() {
    let dir = scratch(TMP, "destroy-fails");
    let cases = "delete*) echo 'error: forbidden' >&2; exit 1 ;;";
    let err = runner(&dir, cases)
        .destroy(&context(&dir), &compiled(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "kubectl could not delete the objects");
}
