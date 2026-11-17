//! Importing existing manifests, all the way round: the manifests are
//! imported, the document is written and read back, the document is compiled,
//! and the manifests it compiles to are compared, by meaning, with the ones
//! that were imported.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_import::diff::{assert_same, Rules};
use meridian_import::yaml;
use meridian_plugin_testkit::{
    assert_golden_text, import_source, import_text, notes_text, round_trip,
};
use meridian_sdk::{Importer, NoteLevel, Value};
use meridian_target_kubernetes::{Kubernetes, KubernetesImporter, MANIFESTS_PATH};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/import")
}

/// The objects of a stream of manifests, as Kubernetes reads them.
fn objects(text: &str) -> Value {
    Value::List(
        yaml::read("manifests.yaml", text, yaml::Rules::Yaml11)
            .unwrap()
            .iter()
            .map(yaml::Node::value)
            .collect(),
    )
}

#[test]
fn manifests_come_back_meaning_the_same() {
    let source = import_source(&testdata().join("web"), "web");
    let (imported, trips) = round_trip(&KubernetesImporter, &Kubernetes, &source);
    assert_eq!(trips.len(), 1);
    assert_eq!(imported.needing_attention(), 0, "{}", notes_text(&imported));
    assert_golden_text(&testdata(), "web.meridian.yaml", &trips[0].rendered.text);

    // The semantic diff: object for object, in the order they were written.
    let original = &source.files["web.yaml"];
    let compiled = &trips[0].emitted.artifact.files[MANIFESTS_PATH];
    assert_same(&objects(original), &objects(compiled), Rules::default());

    // The shell variables came through as the text they are.
    assert!(
        compiled.contains("echo \"starting as ${USER} in ${HOME}\""),
        "{compiled}"
    );
    // A Deployment and its Service share a name; both kept it.
    let ids: Vec<&str> = imported.documents[0]
        .resources
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["shop", "web_config", "web", "web_service"]);
}

#[test]
fn what_a_cluster_wrote_is_left_out_and_said_so() {
    // An object as `kubectl get -o yaml` prints it, inside the List that
    // command wraps several objects in.
    let exported = r#"
apiVersion: v1
kind: List
items:
  - apiVersion: v1
    kind: Service
    metadata:
      name: api
      namespace: shop
      uid: 0b8ccd4e-1f3a-4c55-9f0e-2b6f1f8f6c11
      resourceVersion: "184467"
      creationTimestamp: "2026-05-02T09:14:07Z"
      annotations:
        kubectl.kubernetes.io/last-applied-configuration: '{"apiVersion":"v1"}'
        team: payments
      managedFields:
        - manager: kubectl
          operation: Update
    spec:
      clusterIP: 10.96.14.7
      ports:
        - port: 443
    status:
      loadBalancer: {}
"#;
    let source = import_text("api", &[("exported.yaml", exported)]);
    let (imported, trips) = round_trip(&KubernetesImporter, &Kubernetes, &source);
    let compiled = objects(&trips[0].emitted.artifact.files[MANIFESTS_PATH]);
    let expected = objects(
        r#"
apiVersion: v1
kind: Service
metadata:
  name: api
  namespace: shop
  annotations:
    team: payments
spec:
  clusterIP: 10.96.14.7
  ports:
    - port: 443
"#,
    );
    assert_same(&expected, &compiled, Rules::default());

    assert_eq!(imported.notes.len(), 1);
    let note = &imported.notes[0];
    assert_eq!(note.level, NoteLevel::Info);
    assert_eq!(note.resource, "api");
    for field in [
        "status",
        "metadata.uid",
        "metadata.managedFields",
        "metadata.annotations[kubectl.kubernetes.io/last-applied-configuration]",
    ] {
        assert!(note.msg.contains(field), "{field}: {}", note.msg);
    }
}

#[test]
fn what_is_not_an_object_is_reported_and_marked() {
    let source = import_text(
        "mixed",
        &[(
            "all.yaml",
            r#"
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - web.yaml
---
just: some
yaml: file
---
apiVersion: batch/v1
kind: Job
metadata:
  generateName: migrate-
spec:
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: migrate
          image: example/migrate:1
"#,
        )],
    );
    let (imported, trips) = round_trip(&KubernetesImporter, &Kubernetes, &source);
    let levels: Vec<(NoteLevel, &str)> = imported
        .notes
        .iter()
        .map(|n| (n.level, n.subject.as_str()))
        .collect();
    assert_eq!(
        levels,
        [
            (NoteLevel::Untranslated, "Kustomization \"\""),
            (NoteLevel::Untranslated, "document"),
            (NoteLevel::Changed, "Job \"\""),
        ]
    );
    assert_eq!(trips[0].rendered.todos, 3);
    assert_eq!(imported.documents[0].resources.len(), 1);
    assert_eq!(imported.documents[0].resources[0].id, "job");
}

#[test]
fn a_source_with_nothing_to_import_is_refused() {
    let chart = import_text(
        "chart",
        &[(
            "deployment.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: {{ .Release.Name }}\n",
        )],
    );
    let err = KubernetesImporter.import(&chart).unwrap_err();
    assert!(err.msg.contains("Helm template"), "{err}");

    let broken = import_text("x", &[("a.yaml", "a: [1, 2\nb: 3\n")]);
    let err = KubernetesImporter.import(&broken).unwrap_err();
    assert_eq!(err.file, "a.yaml");

    let empty = import_text("x", &[("a.yaml", "# nothing here\n")]);
    let err = KubernetesImporter.import(&empty).unwrap_err();
    assert!(err.msg.contains("no manifests"), "{err}");

    // A manifest may hold braces inside a string without being a template.
    let rule = import_text(
        "x",
        &[(
            "rule.yaml",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: rules\ndata:\n  alert: \"High load on {{ $labels.instance }}\"\n",
        )],
    );
    assert!(KubernetesImporter.import(&rule).is_ok());
}
