//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_at};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_helm::Helm;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Helm.emit(graph, &Data::new())
}

fn compile_named(name: &str, spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: {name}\nspec:\n  target: helm\n{spec}"
    );
    emit(&graph_from_text("app.yaml", &text, "helm"))
}

fn compile(spec: &str) -> Emitted {
    compile_named("app", spec).unwrap_or_else(|e| panic!("{e}"))
}

fn file(spec: &str, path: &str) -> String {
    let emitted = compile(spec);
    emitted
        .artifact
        .files
        .get(path)
        .unwrap_or_else(|| {
            panic!(
                "no {path} among {:?}",
                emitted.artifact.files.keys().collect::<Vec<_>>()
            )
        })
        .clone()
}

fn refusal(spec: &str) -> CompileError {
    compile_named("app", spec).expect_err("the document should be refused")
}

const CONFIG: &str =
    "    - id: settings\n      type: v1/ConfigMap\n      params:\n        data: {mode: fast}\n";

const VALUES: &str = "    - id: values\n      type: values\n      params:\n        replicas: 3\n        big: 5000000\n        ratio: 0.5\n        debug: true\n        image: {repository: nginx, tag: \"1.27\"}\n        hosts: [a.example.com, b.example.com]\n        my-key: {inner: x}\n        resources: {limits: {memory: 64Mi}}\n";

fn workload(id: &str, extra: &str) -> String {
    format!(
        "    - id: {id}\n      type: apps/v1/Deployment\n      params:\n        spec:\n          selector: {{matchLabels: {{app: {id}}}}}\n          template:\n            metadata: {{labels: {{app: {id}}}}}\n            spec:\n              containers:\n                - name: main\n                  image: nginx\n{extra}"
    )
}

#[test]
fn the_example_compiles_to_its_golden_chart() {
    let graph = graph_from_file("examples/helm/web-app.yaml", "helm");
    assert_golden(&golden(), "web-app", &emit(&graph).unwrap());
}

#[test]
fn the_overlay_example_compiles_to_its_golden_chart_and_kustomization() {
    let graph = graph_from_file("examples/helm/web-app-kustomize.yaml", "helm");
    assert_golden(&golden(), "web-app-kustomize", &emit(&graph).unwrap());
}

#[test]
fn an_object_is_one_template_named_after_its_id() {
    let files = compile(&format!("  resources:\n{CONFIG}")).artifact.files;
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        [
            "chart/Chart.yaml",
            "chart/templates/settings.yaml",
            "chart/values.yaml",
            "release.json"
        ]
    );
    assert_eq!(
        files["chart/templates/settings.yaml"],
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: settings\ndata:\n  mode: fast\n"
    );
    assert_eq!(files["chart/values.yaml"], "{}\n");
    assert_eq!(
        files["chart/Chart.yaml"],
        "apiVersion: v2\nname: app\ndescription: Compiled by Meridian from the document app.\ntype: application\nversion: 0.1.0\n"
    );
}

#[test]
fn an_object_without_a_name_is_named_after_its_id() {
    let text = file(
        "  resources:\n    - id: web_config\n      type: v1/ConfigMap\n      params:\n        metadata: {labels: {a: b}}\n        data: {k: v}\n",
        "chart/templates/web_config.yaml",
    );
    assert!(
        text.contains("metadata:\n  name: web-config\n  labels:\n    a: b\n"),
        "{text}"
    );
}

#[test]
fn a_whole_value_reference_keeps_its_type() {
    let text = file(
        &format!(
            "  resources:\n{VALUES}    - id: c\n      type: v1/ConfigMap\n      params:\n        replicas: ${{values.replicas}}\n        debug: ${{values.debug}}\n        tag: ${{values.image.tag}}\n        limits: ${{values.resources}}\n        first: ${{values.hosts[0]}}\n        odd: ${{values.my-key.inner}}\n        all: ${{values}}\n"
        ),
        "chart/templates/c.yaml",
    );
    for line in [
        "replicas: {{ .Values.replicas | toRawJson }}\n",
        "debug: {{ .Values.debug | toRawJson }}\n",
        "tag: {{ .Values.image.tag | toRawJson }}\n",
        "limits: {{ .Values.resources | toRawJson }}\n",
        "first: {{ (index .Values.hosts 0) | toRawJson }}\n",
        "odd: {{ (index .Values \"my-key\").inner | toRawJson }}\n",
        "all: {{ .Values | toRawJson }}\n",
    ] {
        assert!(text.contains(line), "{line} is not in\n{text}");
    }
}

#[test]
fn a_reference_inside_text_is_built_with_printf() {
    let text = file(
        &format!(
            "  resources:\n{VALUES}    - id: c\n      type: v1/ConfigMap\n      params:\n        data:\n          image: ${{values.image.repository}}:${{values.image.tag}}\n          big: count=${{values.big}}\n          odds: \"100% of \\\"${{values.ratio}}\\\"\\nnext\"\n"
        ),
        "chart/templates/c.yaml",
    );
    for line in [
        "image: {{ printf \"%s:%s\" .Values.image.repository .Values.image.tag | toRawJson }}\n",
        // A number goes through toRawJson, so a large one is not written 5e+06.
        "big: {{ printf \"count=%s\" (toRawJson .Values.big) | toRawJson }}\n",
        "odds: {{ printf \"100%% of \\\"%s\\\"\\nnext\" (toRawJson .Values.ratio) | toRawJson }}\n",
    ] {
        assert!(text.contains(line), "{line} is not in\n{text}");
    }
}

#[test]
fn the_release_and_the_chart_can_be_read() {
    let text = file(
        "  resources:\n    - id: rel\n      type: release\n      params: {name: shop, namespace: store}\n    - id: about\n      type: chart\n      params: {name: shop-chart, version: 2.0.1, appVersion: \"9\"}\n    - id: c\n      type: v1/ConfigMap\n      params:\n        metadata:\n          name: ${rel.name}-settings\n        data:\n          where: ${rel.namespace}\n          what: ${about.name}-${about.version} (${about.appVersion})\n",
        "chart/templates/c.yaml",
    );
    assert!(
        text.contains("  name: {{ printf \"%s-settings\" .Release.Name | toRawJson }}\n"),
        "{text}"
    );
    assert!(
        text.contains("  where: {{ .Release.Namespace | toRawJson }}\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "  what: {{ printf \"%s-%s (%s)\" .Chart.Name .Chart.Version .Chart.AppVersion | toRawJson }}\n"
        ),
        "{text}"
    );
}

#[test]
fn a_reference_to_an_object_is_its_name() {
    let spec = format!(
        "  resources:\n    - id: named\n      type: v1/ConfigMap\n      params:\n        metadata: {{name: \"${{release.name}}-config\"}}\n        data: {{k: v}}\n    - id: release\n      type: release\n      params: {{name: shop}}\n{CONFIG}    - id: user\n      type: v1/ConfigMap\n      params:\n        data:\n          plain: ${{settings}}\n          long: ${{settings.metadata.name}}\n          templated: ${{named}}\n          inside: cm/${{named}}/x\n          where: ${{named.metadata.namespace}}\n          host: ${{named}}.${{named.metadata.namespace}}.svc\n"
    );
    let text = file(&spec, "chart/templates/user.yaml");
    for line in [
        "  plain: settings\n",
        "  long: settings\n",
        "  templated: {{ printf \"%s-config\" .Release.Name | toRawJson }}\n",
        "  inside: {{ printf \"cm/%s-config/x\" .Release.Name | toRawJson }}\n",
        // An object with no namespace of its own lands in the release's.
        "  where: {{ .Release.Namespace | toRawJson }}\n",
        "  host: {{ printf \"%s-config.%s.svc\" .Release.Name .Release.Namespace | toRawJson }}\n",
    ] {
        assert!(text.contains(line), "{line} is not in\n{text}");
    }
    // The runner is told the name the object has with the document's values.
    let release = file(&spec, "release.json");
    assert!(release.contains("\"name\": \"shop-config\""), "{release}");
}

#[test]
fn text_the_author_wrote_is_never_a_template() {
    let text = file(
        "  resources:\n    - id: c\n      type: v1/ConfigMap\n      params:\n        data:\n          go: \"{{ .Values.x }} and {{- if }}\"\n          dollar: cost is $${5}\n          \"{{key}}\": v\n          closing: \"}} alone\"\n",
        "chart/templates/c.yaml",
    );
    for line in [
        "  go: '{{ `{{` }} .Values.x }} and {{ `{{` }}- if }}'\n",
        "  dollar: cost is ${5}\n",
        "  '{{ `{{` }}key}}': v\n",
        "  closing: '}} alone'\n",
    ] {
        assert!(text.contains(line), "{line} is not in\n{text}");
    }
}

#[test]
fn values_are_written_as_they_are() {
    let text = file(
        "  resources:\n    - id: v\n      type: values\n      params:\n        template: \"{{ not helm's }}\"\n        price: $${5}\n        list: [1, two]\n    - id: c\n      type: v1/ConfigMap\n      params: {data: {k: v}}\n",
        "chart/values.yaml",
    );
    assert_eq!(
        text,
        "list:\n  - 1\n  - two\nprice: ${5}\ntemplate: '{{ not helm''s }}'\n"
    );
}

#[test]
fn a_restart_notification_becomes_a_checksum_on_the_pod_template() {
    let spec = format!(
        "  resources:\n    - id: settings\n      type: v1/ConfigMap\n      params:\n        data: {{mode: fast}}\n      notifies:\n        - resource: web\n          action: restart\n    - id: keys\n      type: v1/Secret\n      params:\n        stringData: {{token: abc}}\n      notifies:\n        - resource: web\n          action: restart\n{}",
        workload("web", "")
    );
    let emitted = compile(&spec);
    let text = &emitted.artifact.files["chart/templates/web.yaml"];
    assert!(
        text.contains(
            "  template:\n    metadata:\n      annotations:\n        checksum/keys: {{ include (print $.Template.BasePath \"/keys.yaml\") . | sha256sum | quote }}\n        checksum/settings: {{ include (print $.Template.BasePath \"/settings.yaml\") . | sha256sum | quote }}\n      labels:\n        app: web\n"
        ),
        "{text}"
    );
    let warnings: Vec<String> = emitted.warnings.iter().map(|w| w.to_string()).collect();
    assert_eq!(warnings.len(), 2);
    assert!(
        warnings.iter().any(|w| w
            .starts_with("helm: web: restarts by rolling its pods whenever \"settings\" changes")),
        "{warnings:?}"
    );
}

#[test]
fn only_a_restart_of_a_workload_can_be_notified() {
    let err = refusal(&format!(
        "  resources:\n    - id: settings\n      type: v1/ConfigMap\n      params:\n        data: {{mode: fast}}\n      notifies:\n        - resource: web\n          action: reload\n{}",
        workload("web", "")
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "settings");
    assert!(err.msg.contains("action \"reload\""), "{}", err.msg);

    let err = refusal(
        "  resources:\n    - id: a\n      type: v1/ConfigMap\n      params: {data: {k: v}}\n      notifies:\n        - resource: b\n          action: restart\n    - id: b\n      type: v1/Service\n      params: {spec: {ports: [{port: 80}]}}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert!(
        err.msg.contains("a Service has no pods to roll"),
        "{}",
        err.msg
    );
}

#[test]
fn two_workloads_cannot_restart_each_other() {
    let err = refusal(&format!(
        "  resources:\n{}      notifies:\n        - resource: b\n          action: restart\n{}      notifies:\n        - resource: a\n          action: restart\n",
        workload("a", ""),
        workload("b", "")
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert!(err.msg.contains("notifies it back"), "{}", err.msg);
}

#[test]
fn a_dependency_helm_already_honours_needs_nothing_written() {
    let spec = format!(
        "  resources:\n{CONFIG}{}      dependsOn:\n        - resource: settings\n",
        workload("web", "")
    );
    let emitted = compile(&spec);
    assert!(emitted.warnings.is_empty());
    assert!(!emitted.artifact.files["chart/templates/web.yaml"].contains("settings"));
}

#[test]
fn a_dependency_helm_cannot_honour_is_refused() {
    let err = refusal(&format!(
        "  resources:\n{}{}      dependsOn:\n        - resource: db\n",
        workload("db", ""),
        workload("api", "")
    ));
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(err.resource, "api");
    assert!(
        err.msg
            .contains("it does not create a Deployment before a Deployment"),
        "{}",
        err.msg
    );

    let err = refusal(&format!(
        "  resources:\n{}    - id: settings\n      type: v1/ConfigMap\n      params: {{data: {{k: v}}}}\n      dependsOn:\n        - resource: web\n",
        workload("web", "")
    ));
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(err.resource, "settings");
}

#[test]
fn a_dependency_on_settings_is_refused() {
    let err = refusal(&format!(
        "  resources:\n    - id: values\n      type: values\n      params: {{a: 1}}\n{}      dependsOn:\n        - resource: values\n",
        workload("web", "")
    ));
    assert_eq!(err.rule, "unsupported-dependency");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(
        "  resources:\n    - id: c\n      type: v1/ConfigMap\n      params:\n        data: {k: \"${valeus.x}\"}\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "c");
    assert_eq!(
        err.msg,
        "param \"data\": ${valeus.x} refers to \"valeus\", which is not a resource in this document"
    );
}

#[test]
fn a_value_that_does_not_exist_is_refused_at_compile_time() {
    let err = refusal(&format!(
        "  resources:\n{VALUES}    - id: c\n      type: v1/ConfigMap\n      params:\n        data: {{k: \"${{values.image.tga}}\"}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(
        err.msg,
        "param \"data\": ${values.image.tga}: the values have no image.tga"
    );
}

#[test]
fn a_template_cannot_read_what_only_the_cluster_knows() {
    let err = refusal(&format!(
        "  resources:\n{CONFIG}    - id: c\n      type: v1/ConfigMap\n      params:\n        data: {{k: \"${{settings.metadata.uid}}\"}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(
        err.msg.contains(
            "${settings.metadata.uid} reads metadata.uid of \"settings\", which only the cluster knows"
        ),
        "{}",
        err.msg
    );
}

#[test]
fn a_map_cannot_be_part_of_text_and_a_key_cannot_be_a_reference() {
    let err = refusal(&format!(
        "  resources:\n{VALUES}    - id: c\n      type: v1/ConfigMap\n      params:\n        data: {{k: \"limits=${{values.resources}}\"}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(
        err.msg.contains("puts a mapping inside text"),
        "{}",
        err.msg
    );

    let err = refusal(&format!(
        "  resources:\n{VALUES}    - id: c\n      type: v1/ConfigMap\n      params:\n        data: {{\"${{values.replicas}}\": v}}\n"
    ));
    assert!(err.msg.contains("holds a reference"), "{}", err.msg);
}

#[test]
fn settings_and_values_are_literal() {
    let err = refusal(&format!(
        "  resources:\n{CONFIG}    - id: values\n      type: values\n      params: {{name: \"${{settings}}\"}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "values");

    let err = refusal(&format!(
        "  resources:\n{CONFIG}    - id: release\n      type: release\n      params: {{name: \"${{settings}}\"}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
}

#[test]
fn a_type_that_is_not_an_object_is_refused() {
    let err =
        refusal("  resources:\n    - id: web\n      type: Deployment\n      params: {spec: {}}\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert!(err.msg.contains("apps/v1/Deployment"), "{}", err.msg);
}

#[test]
fn the_type_already_says_the_api_version_and_kind() {
    let err = refusal(
        "  resources:\n    - id: c\n      type: v1/ConfigMap\n      params: {kind: ConfigMap, data: {k: v}}\n",
    );
    assert_eq!(err.rule, "invalid-param");
}

#[test]
fn settings_are_checked() {
    let rule = |spec: &str| refusal(&format!("  resources:\n{CONFIG}{spec}")).rule;
    assert_eq!(
        rule("    - id: r\n      type: release\n      params: {values: {a: 1}}\n"),
        "unsupported-param"
    );
    assert_eq!(
        rule("    - id: r\n      type: release\n      params: {wait: \"yes\"}\n"),
        "invalid-param"
    );
    assert_eq!(
        rule("    - id: r\n      type: release\n      params: {timeout: soon}\n"),
        "invalid-param"
    );
    assert_eq!(
        rule("    - id: r\n      type: release\n      params: {namespace: Not_A_Namespace}\n"),
        "invalid-param"
    );
    assert_eq!(
        rule("    - id: c\n      type: chart\n      params: {version: \"1.0\"}\n"),
        "invalid-param"
    );
    assert_eq!(
        rule("    - id: c\n      type: chart\n      params: {name: My Chart}\n"),
        "invalid-name"
    );
    assert_eq!(
        rule("    - id: a\n      type: values\n      params: {a: 1}\n    - id: b\n      type: values\n      params: {b: 2}\n"),
        "duplicate-resource"
    );

    let err = refusal(&format!(
        "  resources:\n{CONFIG}    - id: c\n      type: chart\n      params: {{version: 1.5}}\n"
    ));
    assert_eq!(
        err.msg,
        "param \"version\" of the chart must be a string, and this is a number; put it in quotes"
    );
}

#[test]
fn the_release_is_named_by_the_document_unless_told() {
    let release = compile_named("web-app", &format!("  resources:\n{CONFIG}"))
        .unwrap()
        .artifact
        .files["release.json"]
        .clone();
    assert!(release.contains("\"release\": \"web-app\""), "{release}");
    assert!(release.contains("\"namespace\": \"default\""), "{release}");

    let err = compile_named("web_app", &format!("  resources:\n{CONFIG}")).unwrap_err();
    assert_eq!(err.rule, "invalid-name");

    let files = compile_named(
        "web_app",
        &format!(
            "  resources:\n{CONFIG}    - id: r\n      type: release\n      params: {{name: web-app}}\n    - id: c\n      type: chart\n      params: {{name: web-app}}\n"
        ),
    )
    .unwrap()
    .artifact
    .files;
    assert!(files["release.json"].contains("\"release\": \"web-app\""));
}

#[test]
fn names_and_objects_are_checked() {
    let err = refusal(
        "  resources:\n    - id: c\n      type: v1/ConfigMap\n      params:\n        metadata: {name: Not_Valid}\n        data: {k: v}\n",
    );
    assert_eq!(err.rule, "invalid-name");

    let err = refusal(
        "  resources:\n    - id: _partial\n      type: v1/ConfigMap\n      params: {data: {k: v}}\n",
    );
    assert_eq!(err.rule, "invalid-name");

    let err = refusal(
        "  resources:\n    - id: web_config\n      type: v1/ConfigMap\n      params: {data: {k: v}}\n    - id: web-config\n      type: v1/ConfigMap\n      params: {data: {k: w}}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(
        err.msg,
        "resources \"web_config\" and \"web-config\" are both the ConfigMap named \"web-config\"; a release holds an object once"
    );

    let err = refusal("  resources:\n    - id: v\n      type: values\n      params: {a: 1}\n");
    assert_eq!(err.rule, "empty-document");
}

#[test]
fn state_and_runtime_conditions_are_refused() {
    let err = refusal(
        "  resources:\n    - id: c\n      type: v1/ConfigMap\n      state: absent\n      params: {data: {k: v}}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
}

#[test]
fn an_overlay_becomes_a_kustomization_over_what_helm_renders() {
    let spec = format!(
        "  resources:\n{CONFIG}    - id: release\n      type: release\n      params: {{name: shop}}\n    - id: overlay\n      type: kustomization\n      params:\n        namePrefix: ${{release.name}}-\n        namespace: store\n        commonAnnotations: {{owner: team-a}}\n        patches:\n          - target: {{kind: ConfigMap, name: \"${{settings}}\"}}\n            patch:\n              apiVersion: v1\n              kind: ConfigMap\n              metadata: {{name: \"${{settings}}\"}}\n              data: {{mode: slow}}\n          - patch: \"- op: remove\\n  path: /data/mode\\n\"\n            target: {{kind: ConfigMap}}\n"
    );
    let text = file(&spec, "kustomize/kustomization.yaml");
    assert_eq!(
        text,
        "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - helm-rendered.yaml\ncommonAnnotations:\n  owner: team-a\nnamePrefix: shop-\nnamespace: store\npatches:\n  - patch: |\n      apiVersion: v1\n      data:\n        mode: slow\n      kind: ConfigMap\n      metadata:\n        name: settings\n    target:\n      kind: ConfigMap\n      name: settings\n  - patch: |\n      - op: remove\n        path: /data/mode\n    target:\n      kind: ConfigMap\n"
    );
    // The runner is told where the overlay moves each object.
    let release = file(&spec, "release.json");
    assert!(release.contains("\"kustomize\": true"), "{release}");
    assert!(release.contains("\"name\": \"shop-settings\""), "{release}");
    assert!(release.contains("\"namespace\": \"store\""), "{release}");
}

#[test]
fn an_overlay_cannot_name_other_files() {
    for field in [
        "resources: [extra.yaml]",
        "helmCharts: [{name: x}]",
        "kind: Kustomization",
    ] {
        let err = refusal(&format!(
            "  resources:\n{CONFIG}    - id: overlay\n      type: kustomization\n      params:\n        {field}\n"
        ));
        assert_eq!(err.rule, "unsupported-param", "{field}");
    }
    let err = refusal(&format!(
        "  resources:\n{CONFIG}    - id: overlay\n      type: kustomization\n      params:\n        patches: [{{path: patch.yaml}}]\n"
    ));
    assert_eq!(err.rule, "unsupported-param");
    assert!(err.msg.contains("under \"patch\""), "{}", err.msg);
}

#[test]
fn outputs_are_split_into_what_is_known_and_what_the_cluster_knows() {
    let spec = format!(
        "  resources:\n{VALUES}{CONFIG}    - id: release\n      type: release\n      params: {{name: shop, namespace: store}}\n    - id: keys\n      type: v1/Secret\n      params: {{stringData: {{token: abc}}}}\n    - id: svc\n      type: v1/Service\n      params: {{spec: {{ports: [{{port: 80}}]}}}}\n  outputs:\n    - name: url\n      value: http://${{svc.status.loadBalancer.ingress[0].ip}}:${{values.replicas}}/\n    - name: service\n      value: ${{svc}}.${{release.namespace}}\n    - name: token\n      value: ${{keys.data.token}}\n    - name: annotation\n      value: ${{svc.metadata.annotations[\"example.com/owner\"]}}\n"
    );
    let release = meridian_sdk::decode_value(file(&spec, "release.json").as_bytes()).unwrap();
    assert_eq!(
        release.as_map().unwrap()["outputs"].canonical(),
        concat!(
            r#"[{"name":"url","parts":[{"text":"http://"},{"path":["status","loadBalancer","ingress",0,"ip"],"resource":"svc"},{"text":":3/"}],"sensitive":false},"#,
            r#"{"name":"service","parts":[{"text":"svc.store"}],"sensitive":false},"#,
            // Whatever is read from a Secret is masked wherever it is shown.
            r#"{"name":"token","parts":[{"path":["data","token"],"resource":"keys"}],"sensitive":true},"#,
            r#"{"name":"annotation","parts":[{"path":["metadata","annotations","example.com/owner"],"resource":"svc"}],"sensitive":false}]"#
        )
    );
}

#[test]
fn an_output_that_refers_to_nothing_is_refused() {
    let err = compile_named(
        "app",
        &format!(
            "  resources:\n{CONFIG}  outputs:\n    - name: x\n      value: ${{nothing.status}}\n"
        ),
    )
    .unwrap_err();
    assert_eq!(err.rule, "invalid-reference");
    assert!(err.msg.starts_with("output \"x\": "), "{}", err.msg);
}

#[test]
fn compiling_twice_gives_the_same_bytes() {
    let graph = graph_from_file("examples/helm/web-app.yaml", "helm");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

/// The document with the given place made a secret called `db_password`.
fn with_secret(spec: &str, id: &str, path: &[&str]) -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: app\nspec:\n  target: helm\n{spec}"
    );
    let mut graph = graph_from_text("app.yaml", &text, "helm");
    secret_at(&mut graph, id, path, "db_password");
    graph
}

const LOGIN: &str = "    - id: login\n      type: v1/Secret\n      params:\n        stringData:\n          password: ${values.db.password}\n";

#[test]
fn a_secret_in_the_values_is_a_token_the_runner_fills_on_the_way_to_helm() {
    let graph = with_secret(
        &format!("  resources:\n{VALUES}        db: {{user: app, password: placeholder}}\n{LOGIN}"),
        "values",
        &["db", "password"],
    );
    let emitted = emit(&graph).unwrap();
    let values = &emitted.artifact.files["chart/values.yaml"];
    assert!(
        values.contains("db:\n  password: \"${meridian_secret_db_password}\"\n  user: app\n"),
        "{values}"
    );
    assert!(!values.contains("(secret "), "{values}");
    // The template reads the value as any other.
    let template = &emitted.artifact.files["chart/templates/login.yaml"];
    assert!(template.contains(".Values.db.password"), "{template}");
    assert_eq!(
        meridian_target_helm::secret_token("db_password"),
        "${meridian_secret_db_password}"
    );
    assert!(Helm.capabilities().secrets);
}

#[test]
fn a_secret_anywhere_but_the_values_is_refused_with_where_it_goes() {
    let cases: [(&str, &str, &[&str], &str); 4] = [
        (
            "    - id: chart\n      type: chart\n      params:\n        version: 1.0.0\n",
            "chart",
            &["version"],
            "the command line helm is given",
        ),
        (
            "    - id: release\n      type: release\n      params:\n        namespace: web\n",
            "release",
            &["namespace"],
            "the command line helm is given",
        ),
        (
            CONFIG,
            "settings",
            &["data", "mode"],
            "written into a template file as it stands",
        ),
        (
            &format!("{VALUES}    - id: overlay\n      type: kustomization\n      params:\n        namePrefix: prod-\n"),
            "values",
            &["replicas"],
            "has a kustomization",
        ),
    ];
    for (resources, id, path, said) in cases {
        let err = emit(&with_secret(
            &format!("  resources:\n{resources}"),
            id,
            path,
        ))
        .unwrap_err();
        assert_eq!(err.rule, "unsupported-secret-place", "{path:?}: {err}");
        assert!(err.msg.contains(said), "{path:?}: {err}");
        assert!(!err.msg.contains("(secret "), "{err}");
    }
}

#[test]
fn text_that_reads_as_a_secrets_token_is_refused() {
    let err = refusal(
        "  resources:\n    - id: values\n      type: values\n      params:\n        x: \"${meridian_secret_db_password}\"\n",
    );
    assert_eq!(err.rule, "reserved-name");
    assert!(err.msg.contains("choose other text"), "{err}");
}
