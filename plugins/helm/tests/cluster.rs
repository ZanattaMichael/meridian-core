//! The plugin against a real cluster: plan, apply, outputs, upgrade and the
//! Kustomize overlay, end to end, with nothing standing in for anything.
//!
//! These need `helm`, `kustomize`, `kubectl` and a cluster the current
//! context may install into (a `kind` cluster is enough), so they are ignored
//! by default. Run them with
//! `cargo test -p meridian-target-helm --test cluster -- --ignored`; the
//! `helm` workflow does on every pull request that touches the plugin.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::path::Path;

use meridian_plugin_testkit::{graph_from_text, scratch, KeptLog};
use meridian_sdk::tool::Tool;
use meridian_sdk::{Applied, Artifact, Data, Emitter, NoLog, Plan, RunContext, Runner, Value};
use meridian_target_helm::{Helm, HelmRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compile(document: &str) -> Artifact {
    Helm.emit(&graph_from_text("app.yaml", document, "helm"), &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "app".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn plan(dir: &Path, artifact: &Artifact) -> Plan {
    HelmRunner::from_env()
        .plan(&context(dir), artifact, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"))
}

fn apply(dir: &Path, artifact: &Artifact) -> Applied {
    HelmRunner::from_env()
        .apply(&context(dir), artifact, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Removes what an earlier run left, so the test starts from nothing.
fn uninstall(release: &str, namespace: &str) {
    let _ = Tool::new("helm")
        .args(["uninstall", release, "--namespace", namespace, "--wait"])
        .run(&NoLog);
}

fn shown(changes: &[meridian_sdk::Change]) -> Vec<String> {
    let mut shown: Vec<String> = changes.iter().map(|c| c.to_string()).collect();
    shown.sort();
    shown
}

fn output<'a>(applied: &'a Applied, name: &str) -> &'a Value {
    &applied
        .outputs
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no output {name}"))
        .value
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

/// A workload that becomes ready without pulling anything large.
fn document(replicas: u32, mode: &str, with_service: bool) -> String {
    let service = if with_service {
        "    - id: svc\n      type: v1/Service\n      params:\n        spec:\n          selector: {app: pause}\n          ports: [{port: 80}]\n"
    } else {
        ""
    };
    let service_outputs = if with_service {
        "    - name: cluster_ip\n      value: ${svc.spec.clusterIP}\n    - name: url\n      value: http://${svc.spec.clusterIP}:${svc.spec.ports[0].port}/\n"
    } else {
        ""
    };
    format!(
        r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: meridian-helm-test
spec:
  target: helm
  resources:
    - id: values
      type: values
      params:
        replicas: {replicas}
        mode: {mode}
        word: "it's \"quoted\" & <tagged>"
    - id: release
      type: release
      params:
        namespace: meridian-helm-test
        createNamespace: true
        wait: true
        timeout: 4m
    - id: settings
      type: v1/ConfigMap
      params:
        data:
          mode: ${{values.mode}}
          braces: "{{{{ .Values.word }}}} {{{{- end }}}}"
          mixed: "50% ${{values.word}} $${{HOME}}"
          lines: "one\ntwo: {{{{ three }}}}\n"
      notifies:
        - resource: web
          action: restart
    - id: web
      type: apps/v1/Deployment
      params:
        metadata:
          name: ${{release.name}}-web
        spec:
          replicas: ${{values.replicas}}
          selector:
            matchLabels: {{app: pause}}
          template:
            metadata:
              labels: {{app: pause}}
            spec:
              containers:
                - name: pause
                  image: registry.k8s.io/pause:3.9
                  env:
                    - name: MODE
                      valueFrom:
                        configMapKeyRef:
                          name: ${{settings}}
                          key: mode
{service}  outputs:
    - name: ready
      value: ${{web.status.readyReplicas}}
    - name: deployment
      value: ${{web}}
    - name: braces
      value: ${{settings.data.braces}}
    - name: mixed
      value: ${{settings.data.mixed}}
    - name: lines
      value: ${{settings.data.lines}}
{service_outputs}"#
    )
}

#[test]
#[ignore = "needs helm, kubectl and a cluster"]
fn a_release_is_planned_installed_read_and_upgraded() {
    let dir = scratch(TMP, "cluster-release");
    uninstall("meridian-helm-test", "meridian-helm-test");

    // Nothing is installed: everything is to be created.
    let first = compile(&document(1, "fast", true));
    let planned = plan(&dir, &first);
    assert!(!planned.unknown);
    assert_eq!(
        shown(&planned.changes),
        [
            "create settings: ConfigMap settings",
            "create svc: Service svc",
            "create web: Deployment meridian-helm-test-web",
        ]
    );

    // The release waits for the workload, so the outputs are there.
    let applied = apply(&dir, &first);
    assert_eq!(shown(&applied.changes), shown(&planned.changes));
    assert_eq!(output(&applied, "ready"), &Value::Int(1));
    assert_eq!(
        output(&applied, "deployment"),
        &text("meridian-helm-test-web")
    );
    // What the author wrote is what the cluster holds, character for
    // character.
    assert_eq!(
        output(&applied, "braces"),
        &text("{{ .Values.word }} {{- end }}")
    );
    assert_eq!(
        output(&applied, "mixed"),
        &text("50% it's \"quoted\" & <tagged> ${HOME}")
    );
    assert_eq!(output(&applied, "lines"), &text("one\ntwo: {{ three }}\n"));
    let Value::String(cluster_ip) = output(&applied, "cluster_ip") else {
        panic!("the cluster IP is a string")
    };
    assert!(!cluster_ip.is_empty());
    assert_eq!(
        output(&applied, "url"),
        &text(&format!("http://{cluster_ip}:80/"))
    );

    // Applied, the same document is in sync, and applying it again does not
    // give the release a new revision.
    assert!(plan(&dir, &first).in_sync());
    assert!(apply(&dir, &first).changes.is_empty());
    let history = Tool::new("helm")
        .args([
            "history",
            "meridian-helm-test",
            "--namespace",
            "meridian-helm-test",
        ])
        .args(["--output", "json"])
        .capture_stdout()
        .run_ok("helm history failed", &NoLog)
        .unwrap()
        .stdout;
    assert_eq!(history.matches("\"revision\"").count(), 1, "{history}");

    // A changed document: the ConfigMap changes, which restarts the workload
    // through its checksum; the workload scales; the Service is gone.
    let second = compile(&document(2, "slow", false));
    let planned = plan(&dir, &second);
    assert_eq!(
        shown(&planned.changes),
        [
            // The document no longer has a resource for it, so it goes by
            // the tool's own name.
            "delete Service/svc: Service svc",
            "update settings: ConfigMap settings",
            "update web: Deployment meridian-helm-test-web",
        ]
    );
    let applied = apply(&dir, &second);
    assert_eq!(output(&applied, "ready"), &Value::Int(2));
    assert!(plan(&dir, &second).in_sync());
    let gone = Tool::new("kubectl")
        .args(["get", "service", "svc", "--namespace", "meridian-helm-test"])
        .run(&NoLog)
        .unwrap();
    assert!(!gone.success(), "Helm deleted what the chart dropped");

    // Outputs can be read again later without applying anything.
    let outputs = HelmRunner::from_env().outputs(&context(&dir)).unwrap();
    assert_eq!(outputs.len(), 5);

    uninstall("meridian-helm-test", "meridian-helm-test");
}

#[test]
#[ignore = "needs helm, kustomize, kubectl and a cluster"]
fn an_overlay_is_installed_as_one_release_and_read_back_in_sync() {
    let dir = scratch(TMP, "cluster-overlay");
    uninstall("meridian-overlay-test", "meridian-overlay-test");

    let document = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: meridian-overlay-test
spec:
  target: helm
  resources:
    - id: release
      type: release
      params:
        namespace: meridian-overlay-test
        createNamespace: true
        wait: true
        timeout: 4m
    - id: overlay
      type: kustomization
      params:
        namePrefix: prod-
        commonAnnotations:
          example.com/template: "{{ kept }}"
        replicas:
          - name: ${web}
            count: 2
        patches:
          - target: {kind: ConfigMap, name: "${settings}"}
            patch:
              - op: add
                path: /data/patched
                value: "yes"
    - id: settings
      type: v1/ConfigMap
      params:
        data: {mode: fast}
    - id: web
      type: apps/v1/Deployment
      params:
        spec:
          replicas: 1
          selector:
            matchLabels: {app: overlay-pause}
          template:
            metadata:
              labels: {app: overlay-pause}
            spec:
              containers:
                - name: pause
                  image: registry.k8s.io/pause:3.9
                  env:
                    - name: MODE
                      valueFrom:
                        configMapKeyRef:
                          name: ${settings}
                          key: mode
  outputs:
    - name: deployment
      value: ${web}
    - name: ready
      value: ${web.status.readyReplicas}
    - name: patched
      value: ${settings.data.patched}
    - name: annotation
      value: ${settings.metadata.annotations["example.com/template"]}
"#;
    let artifact = compile(document);
    let planned = plan(&dir, &artifact);
    assert_eq!(
        shown(&planned.changes),
        [
            "create settings: ConfigMap prod-settings",
            "create web: Deployment prod-web",
        ]
    );

    let applied = apply(&dir, &artifact);
    // The outputs follow the objects to where the overlay put them, and show
    // what the overlay made of them.
    assert_eq!(output(&applied, "deployment"), &text("prod-web"));
    assert_eq!(output(&applied, "ready"), &Value::Int(2));
    assert_eq!(output(&applied, "patched"), &text("yes"));
    assert_eq!(output(&applied, "annotation"), &text("{{ kept }}"));

    // It is one Helm release, and it is in sync with what was patched.
    let releases = Tool::new("helm")
        .args(["list", "--namespace", "meridian-overlay-test", "--short"])
        .capture_stdout()
        .run_ok("helm list failed", &NoLog)
        .unwrap()
        .stdout;
    assert_eq!(releases.trim(), "meridian-overlay-test");
    assert!(plan(&dir, &artifact).in_sync());

    uninstall("meridian-overlay-test", "meridian-overlay-test");
}
