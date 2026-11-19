//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_at};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_kubernetes::Kubernetes;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Kubernetes.emit(graph, &Data::new())
}

const CONFIG: &str =
    "    - id: app_config\n      type: v1/ConfigMap\n      params:\n        data: {workers: \"4\"}\n";
const DEPLOYMENT: &str = "    - id: app\n      type: apps/v1/Deployment\n      params:\n        spec:\n          replicas: 1\n";

fn compile(resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: app\nspec:\n  target: kubernetes\n  resources:\n{resources}"
    );
    emit(&graph_from_text("infra.yaml", &text, "kubernetes"))
}

fn manifests(resources: &str) -> String {
    compile(resources).unwrap().artifact.files["manifests.yaml"].clone()
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

#[test]
fn the_example_compiles_to_its_golden_file() {
    let graph = graph_from_file("examples/kubernetes/web.yaml", "kubernetes");
    assert_golden(&golden(), "web", &emit(&graph).unwrap());
}

#[test]
fn an_object_is_named_after_its_id_and_laid_out_as_kubernetes_writes_it() {
    let text = manifests(CONFIG);
    assert_eq!(
        text,
        "---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: app-config\ndata:\n  workers: \"4\"\n"
    );
}

#[test]
fn a_reference_in_params_is_a_name_known_at_compile_time() {
    let text = manifests(&format!(
        "{CONFIG}    - id: pod\n      type: v1/Pod\n      params:\n        metadata: {{namespace: tools}}\n        spec:\n          config: ${{app_config}}\n          same: ${{app_config.metadata.name}}\n          where: ${{pod.metadata.namespace}}/x\n"
    ));
    assert!(text.contains("config: app-config\n"), "{text}");
    assert!(text.contains("same: app-config\n"), "{text}");
    assert!(text.contains("where: tools/x\n"), "{text}");

    let err = refusal(&format!(
        "{CONFIG}    - id: pod\n      type: v1/Pod\n      params:\n        spec:\n          ip: ${{app_config.status.ip}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(
        err.msg.contains("only known once the object exists"),
        "{}",
        err.msg
    );

    let err = refusal(&format!(
        "{CONFIG}    - id: pod\n      type: v1/Pod\n      params:\n        spec:\n          config: ${{app_confg}}\n"
    ));
    assert_eq!(
        err.msg,
        "param \"spec\": ${app_confg} refers to \"app_confg\", which is not an object in this document"
    );
}

#[test]
fn a_restart_notify_becomes_a_checksum_on_the_pod_template() {
    let notify = "      notifies:\n        - resource: app\n          action: restart\n";
    let first = compile(&format!("{CONFIG}{notify}{DEPLOYMENT}")).unwrap();
    let text = &first.artifact.files["manifests.yaml"];
    assert!(
        text.contains(
            "  template:\n    metadata:\n      annotations:\n        meridian.dev/restart-on-app-config: "
        ),
        "{text}"
    );
    assert_eq!(first.warnings.len(), 1);
    assert!(
        first.warnings[0].msg.contains("rolling its pods"),
        "{}",
        first.warnings[0].msg
    );

    // The checksum follows the notifier's content and nothing else.
    let again = manifests(&format!("{CONFIG}{notify}{DEPLOYMENT}"));
    assert_eq!(text, &again);
    let changed = manifests(&format!(
        "{}{notify}{DEPLOYMENT}",
        CONFIG.replace("\"4\"", "\"8\"")
    ));
    let line = |text: &str| {
        text.lines()
            .find(|l| l.contains("restart-on-app-config"))
            .unwrap()
            .to_owned()
    };
    assert_ne!(line(text), line(&changed));
}

#[test]
fn a_notify_kubernetes_cannot_act_on_is_refused() {
    let err = refusal(&format!(
        "{CONFIG}      notifies:\n        - resource: app\n          action: reload\n{DEPLOYMENT}"
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert!(err.msg.contains("(supported: restart)"), "{}", err.msg);

    let err = refusal(&format!(
        "{DEPLOYMENT}      notifies:\n        - resource: app_config\n          action: restart\n{CONFIG}"
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert!(err.msg.contains("has no pods to restart"), "{}", err.msg);
}

#[test]
fn depends_on_splits_the_objects_into_waves_the_runner_waits_between() {
    let job = "    - id: migrate\n      type: batch/v1/Job\n      params: {spec: {}}\n      dependsOn:\n        - resource: app_config\n";
    let emitted = compile(&format!(
        "{DEPLOYMENT}      dependsOn:\n        - resource: migrate\n{CONFIG}{job}"
    ))
    .unwrap();
    let files = &emitted.artifact.files;
    let text = &files["manifests.yaml"];
    assert!(
        text.find("kind: ConfigMap").unwrap() < text.find("kind: Deployment").unwrap(),
        "{text}"
    );
    assert!(files["wave-1.yaml"].contains("kind: ConfigMap"));
    assert!(files["wave-2.yaml"].contains("kind: Job"));
    assert!(files["wave-3.yaml"].contains("kind: Deployment"));
    let cluster = &files["cluster.json"];
    assert!(
        cluster.contains("\"object\": \"job.batch/migrate\""),
        "{cluster}"
    );
    assert!(cluster.contains("\"for\": \"complete\""), "{cluster}");
    assert!(emitted.warnings.is_empty(), "{:?}", emitted.warnings);

    // Without a dependsOn there is one apply and no waves.
    let files = compile(CONFIG).unwrap().artifact.files;
    assert!(!files.contains_key("wave-1.yaml"));
    assert!(!files["cluster.json"].contains("waves"));
}

#[test]
fn the_sidecar_tells_the_runner_what_kubectl_will_call_each_object() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: app\nspec:\n  target: kubernetes\n  resources:\n    - id: target\n      type: cluster\n      params: {{context: prod}}\n{DEPLOYMENT}  outputs:\n    - name: ready\n      value: ${{app.status.readyReplicas}} ready\n"
    );
    let emitted = emit(&graph_from_text("infra.yaml", &text, "kubernetes")).unwrap();
    let cluster = &emitted.artifact.files["cluster.json"];
    assert!(cluster.contains("\"context\": \"prod\""), "{cluster}");
    assert!(
        cluster.contains("\"deployment.apps/app\": \"app\""),
        "{cluster}"
    );
    assert!(
        cluster.contains("\"get\": \"deployment.v1.apps\""),
        "{cluster}"
    );
    assert!(
        cluster.contains("\"path\": \".status.readyReplicas\""),
        "{cluster}"
    );
    assert!(cluster.contains("\"text\": \" ready\""), "{cluster}");
    assert!(!emitted.artifact.files["manifests.yaml"].contains("prod"));
}

#[test]
fn names_types_and_duplicates_are_checked() {
    let err = refusal("    - id: app\n      type: Deployment\n      params: {}\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert!(err.msg.contains("apps/v1/Deployment"), "{}", err.msg);

    let err = refusal(
        "    - id: app\n      type: v1/ConfigMap\n      params:\n        metadata: {name: My_App}\n",
    );
    assert_eq!(err.rule, "invalid-name");

    let err = refusal(&format!(
        "{CONFIG}    - id: other\n      type: v1/ConfigMap\n      params:\n        metadata: {{name: app-config}}\n"
    ));
    assert_eq!(err.rule, "duplicate-resource");

    let err = refusal("    - id: target\n      type: cluster\n      params: {context: prod}\n");
    assert_eq!(err.rule, "empty-document");
}

#[test]
fn absent_and_runtime_conditions_are_refused() {
    let err =
        refusal("    - id: app\n      type: v1/ConfigMap\n      state: absent\n      params: {}\n");
    assert_eq!(err.rule, "unsupported-state");
    assert!(err.msg.contains("never deletes"), "{}", err.msg);
}

const SECRET: &str = "    - id: db_login\n      type: v1/Secret\n      params:\n        stringData:\n          username: app\n          password: placeholder\n";

/// The document with the `password` of the Secret made a secret.
fn with_secret(resources: &str, id: &str, path: &[&str]) -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: app\nspec:\n  target: kubernetes\n  resources:\n{resources}"
    );
    let mut graph = graph_from_text("infra.yaml", &text, "kubernetes");
    secret_at(&mut graph, id, path, "db_password");
    graph
}

#[test]
fn a_secret_in_string_data_is_written_as_a_token_the_runner_fills_on_the_way_to_kubectl() {
    let graph = with_secret(SECRET, "db_login", &["stringData", "password"]);
    let emitted = emit(&graph).unwrap();
    let text = &emitted.artifact.files["manifests.yaml"];
    assert_eq!(
        text,
        "---\napiVersion: v1\nkind: Secret\nmetadata:\n  name: db-login\nstringData:\n  password: \"${meridian_secret_db_password}\"\n  username: app\n"
    );
    assert!(!text.contains("(secret "), "{text}");
    assert_eq!(
        meridian_target_kubernetes::secret_token("db_password"),
        "${meridian_secret_db_password}"
    );
    assert!(Kubernetes.capabilities().secrets);
}

#[test]
fn a_secret_anywhere_but_a_secrets_string_data_is_refused_with_where_it_goes() {
    let cases: [(&str, &str, &[&str], &str); 5] = [
        (
            CONFIG,
            "app_config",
            &["data", "workers"],
            "it would be written into the object for every reader of it to see",
        ),
        (
            SECRET,
            "db_login",
            &["data", "password"],
            "put the secret in stringData",
        ),
        (
            SECRET,
            "db_login",
            &["metadata", "labels", "tier"],
            "only a value of stringData can",
        ),
        (
            SECRET,
            "db_login",
            &["stringData", "nested", "deeper"],
            "a stringData value is a whole string",
        ),
        (
            "    - id: target\n      type: cluster\n      params:\n        context: prod\n",
            "target",
            &["context"],
            "the command line kubectl is given",
        ),
    ];
    for (resources, id, path, said) in cases {
        let err = emit(&with_secret(resources, id, path)).unwrap_err();
        assert_eq!(err.rule, "unsupported-secret-place", "{path:?}: {err}");
        assert!(err.msg.contains(said), "{path:?}: {err}");
        assert!(!err.msg.contains("(secret "), "{err}");
    }
}

#[test]
fn text_that_reads_as_a_secrets_token_is_refused() {
    let err = refusal(
        "    - id: c\n      type: v1/ConfigMap\n      params:\n        data:\n          x: \"${meridian_secret_db_password}\"\n",
    );
    assert_eq!(err.rule, "reserved-name");
    assert!(err.msg.contains("choose other text"), "{err}");
}
