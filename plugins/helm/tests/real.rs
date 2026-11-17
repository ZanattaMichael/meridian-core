//! The plugin against the real `helm` and `kustomize`, without a cluster:
//! what Meridian compiles is linted and rendered by the tools themselves.
//!
//! These need the tools installed, so they are ignored by default. Run them
//! with `cargo test -p meridian-target-helm --test real -- --ignored`; the
//! `helm` workflow does on every pull request that touches the plugin.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]
#![allow(clippy::result_large_err)]

use std::collections::BTreeSet;
use std::path::Path;

use meridian_plugin_testkit::{graph_from_file, graph_from_text, scratch, KeptLog};
use meridian_sdk::tool::Tool;
use meridian_sdk::{Artifact, Data, Emitter, NoLog, RunContext};
use meridian_target_helm::{Helm, HelmRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "test".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn example(path: &str) -> Artifact {
    Helm.emit(&graph_from_file(path, "helm"), &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn render(dir: &Path, artifact: &Artifact) -> String {
    HelmRunner::from_env()
        .render(&context(dir), artifact, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"))
}

fn helm(dir: &Path, args: &[&str]) -> String {
    Tool::new("helm")
        .args(args.iter().copied())
        .dir(dir.join("work"))
        .capture_stdout()
        .run_ok("helm failed", &NoLog)
        .unwrap_or_else(|e| panic!("{e}"))
        .stdout
}

fn assert_lines(rendered: &str, lines: &[&str]) {
    for line in lines {
        assert!(
            rendered.lines().any(|l| l.trim() == *line),
            "{line:?} is not a line of\n{rendered}"
        );
    }
}

/// The objects of a manifest stream as a set of texts, without comments, so
/// two streams can be compared whatever order they are in.
fn objects(text: &str) -> BTreeSet<String> {
    text.split("\n---")
        .map(|document| {
            document
                .lines()
                .filter(|l| !l.starts_with('#') && !l.starts_with("---") && !l.trim().is_empty())
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|document| !document.is_empty())
        .collect()
}

#[test]
#[ignore = "needs helm"]
fn the_example_chart_passes_helm_lint_and_renders_what_the_document_says() {
    let dir = scratch(TMP, "real-example");
    let rendered = render(&dir, &example("examples/helm/web-app.yaml"));
    assert_lines(
        &rendered,
        &[
            "kind: ConfigMap",
            "kind: Deployment",
            "kind: Service",
            "name: \"web-app-config\"",
            "name: \"web-app\"",
            "app.kubernetes.io/version: \"1.27\"",
            "replicas: 2",
            "- image: \"nginx:1.27\"",
            "- containerPort: 8080",
            "targetPort: 8080",
            "index.html: \"<h1>Hello from Meridian</h1>\\n\"",
            // The author's own braces reach the cluster as braces.
            "banner.tmpl: \"{{ .Site.Title }} is served from port 8080\"",
        ],
    );
    // The restart checksum is a real SHA-256 of the ConfigMap's template.
    let checksum = rendered
        .lines()
        .find_map(|l| l.trim().strip_prefix("checksum/web_config: "))
        .expect("the pod template carries the checksum");
    let digest = checksum.trim_matches('"');
    assert_eq!(digest.len(), 64, "{checksum}");
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{checksum}");

    let lint = helm(&dir, &["lint", "chart"]);
    assert!(lint.contains("0 chart(s) failed"), "{lint}");
}

#[test]
#[ignore = "needs helm"]
fn text_survives_helm_exactly_as_the_author_wrote_it() {
    let dir = scratch(TMP, "real-escaping");
    let document = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: tricky
spec:
  target: helm
  resources:
    - id: values
      type: values
      params:
        big: 5000000
        ratio: 0.25
        on: true
        word: "it's \"quoted\" & <tagged>"
        list: [1, two, {three: 3}]
    - id: tricky
      type: v1/ConfigMap
      params:
        data:
          braces: "{{ .Values.word }} {{- end }} }}"
          comment: "{{/* not a comment */}}"
          start: "{{start"
          backtick: "`{{` and `}}`"
          percent: "100% ${values.word} 50%s"
          big: n=${values.big}
          ratio: r=${values.ratio}
          enabled: is ${values.on}
          dollar: $${HOME} costs $5
          whole: ${values.word}
        list: ${values.list}
        number: ${values.big}
"#;
    let artifact = Helm
        .emit(
            &graph_from_text("tricky.yaml", document, "helm"),
            &Data::new(),
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact;
    let rendered = render(&dir, &artifact);
    assert_lines(
        &rendered,
        &[
            "braces: '{{ .Values.word }} {{- end }} }}'",
            "comment: '{{/* not a comment */}}'",
            "start: '{{start'",
            "backtick: '`{{` and `}}`'",
            "percent: \"100% it's \\\"quoted\\\" & <tagged> 50%s\"",
            // A values-file number is a float to Helm, which would print it
            // 5e+06 if it were left to.
            "big: \"n=5000000\"",
            "ratio: \"r=0.25\"",
            "enabled: \"is true\"",
            "dollar: ${HOME} costs $5",
            "whole: \"it's \\\"quoted\\\" & <tagged>\"",
            "list: [1,\"two\",{\"three\":3}]",
            "number: 5000000",
        ],
    );
    helm(&dir, &["lint", "chart"]);
}

#[test]
#[ignore = "needs helm and kustomize"]
fn the_overlay_example_is_patched_by_kustomize_and_still_installable_by_helm() {
    let dir = scratch(TMP, "real-overlay");
    let rendered = render(&dir, &example("examples/helm/web-app-kustomize.yaml"));
    assert_lines(
        &rendered,
        &[
            // namePrefix, and Kustomize following the reference to the
            // renamed ConfigMap.
            "name: prod-web",
            "name: prod-web-settings",
            "environment: production",
            "replicas: 4",
            "image: registry.example.com/mirror/nginx:1.27",
            "memory: 256Mi",
        ],
    );
    assert!(
        !rendered.lines().any(|l| l.trim() == "replicas: 2"),
        "the overlay's replica count replaces the chart's:\n{rendered}"
    );

    // What Helm will install is the patched objects and nothing else.
    helm(&dir, &["lint", "patched"]);
    let installable = helm(&dir, &["template", "web-app", "patched"]);
    assert_eq!(objects(&installable), objects(&rendered));
}

#[test]
#[ignore = "needs helm"]
fn a_chart_helm_rejects_is_reported_with_what_helm_said() {
    let dir = scratch(TMP, "real-broken");
    let mut artifact = example("examples/helm/web-app.yaml");
    artifact.files.insert(
        "chart/templates/web.yaml".into(),
        "kind: {{ .Values.missing.deeper }}\n".into(),
    );
    let err = HelmRunner::from_env()
        .render(&context(&dir), &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "helm could not render the chart");
    assert!(err.output.contains("nil pointer"), "{}", err.output);
}
