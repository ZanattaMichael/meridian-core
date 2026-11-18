//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_in_param};
use meridian_sdk::{
    decode_value, Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph,
    Value,
};
use meridian_target_packer::{Packer, BUILD_PATH, CONFIG_PATH};

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Packer.emit(graph, &Data::new())
}

/// An infrastructure document with the given `spec` body, compiled.
fn compile(spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: img\nspec:\n  target: packer\n{spec}"
    );
    emit(&graph_from_text("image.yaml", &text, "packer"))
}

/// The resources every test needs so that its document is a build at all.
const SOURCE: &str = "    - id: box\n      type: source.null\n      params: {communicator: none}\n";

/// A document whose resources are `SOURCE` followed by `more`.
fn with_source(more: &str) -> String {
    format!("  resources:\n{SOURCE}{more}")
}

fn template(spec: &str) -> String {
    compile(spec).unwrap().artifact.files[CONFIG_PATH].clone()
}

/// The template as data, for asserting on its shape instead of its spelling.
fn parsed(spec: &str) -> Value {
    decode_value(template(spec).as_bytes()).unwrap()
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |v, key| match v {
        Value::Map(map) => map
            .get(*key)
            .unwrap_or_else(|| panic!("no {key:?} in {v:?}")),
        Value::List(items) => &items[key.parse::<usize>().unwrap()],
        other => panic!("{key:?} looked up in {other:?}"),
    })
}

fn refusal(spec: &str) -> CompileError {
    compile(spec).expect_err("the document should be refused")
}

/// The single-key objects of a list of labelled blocks, as their labels.
fn labels(value: &Value) -> Vec<&str> {
    value
        .as_list()
        .unwrap()
        .iter()
        .map(|step| step.as_map().unwrap().keys().next().unwrap().as_str())
        .collect()
}

#[test]
fn the_plugin_says_what_it_is() {
    assert_eq!(Packer.name(), "packer");
    assert_eq!(Packer.category(), Category::InfrastructureAsCode);
    assert_eq!(
        Packer.capabilities(),
        Capabilities {
            secrets: true,
            ..Capabilities::default()
        }
    );
    let types = Packer.supported_resource_types();
    let mut sorted = types.clone();
    sorted.sort();
    assert_eq!(types, sorted, "the resource types are listed sorted");
    assert!(types.contains(&"source.<builder>".to_owned()));
}

#[test]
fn the_examples_compile_to_their_golden_templates() {
    for case in ["web-image", "app-container", "hello"] {
        let graph = graph_from_file(&format!("examples/packer/{case}.yaml"), "packer");
        assert_golden(&golden(), case, &emit(&graph).unwrap());
    }
}

#[test]
fn compiling_is_deterministic() {
    let graph = graph_from_file("examples/packer/web-image.yaml", "packer");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn every_template_is_json_packer_can_read() {
    for case in ["web-image", "app-container", "hello"] {
        let graph = graph_from_file(&format!("examples/packer/{case}.yaml"), "packer");
        let emitted = emit(&graph).unwrap();
        assert_eq!(
            emitted.artifact.files.keys().collect::<Vec<_>>(),
            [CONFIG_PATH, BUILD_PATH]
        );
        for text in emitted.artifact.files.values() {
            assert!(decode_value(text.as_bytes()).is_ok(), "{text}");
        }
    }
}

#[test]
fn a_document_is_one_build_named_after_it_that_builds_every_source() {
    let json = parsed(
        "  resources:\n    - id: ubuntu\n      type: source.amazon-ebs\n      params: {region: us-east-1}\n    - id: app\n      type: source.docker\n      params: {image: debian}\n",
    );
    assert_eq!(at(&json, &["build", "name"]), &Value::String("img".into()));
    assert_eq!(
        at(&json, &["build", "sources"]),
        &Value::List(vec![
            Value::String("source.amazon-ebs.ubuntu".into()),
            Value::String("source.docker.app".into()),
        ])
    );
    assert_eq!(
        at(&json, &["source", "amazon-ebs", "ubuntu", "region"]),
        &Value::String("us-east-1".into())
    );
    assert_eq!(
        at(&json, &["source", "docker", "app", "image"]),
        &Value::String("debian".into())
    );
}

#[test]
fn references_become_packers_addresses() {
    let text = template(
        "  resources:\n    - id: region\n      type: variable\n      params: {type: string, default: us-east-1}\n    - id: base\n      type: data.amazon-ami\n      params: {region: \"${region}\"}\n    - id: stamp\n      type: local\n      params: {value: \"${base.name}-${region}\"}\n    - id: ubuntu\n      type: source.amazon-ebs\n      params:\n        region: ${region}\n        source_ami: ${base.id}\n        ami_name: web-${stamp}\n        tags: {From: \"${base.tags[\\\"Name\\\"]}\", All: [\"${stamp}\"]}\n",
    );
    for want in [
        "\"region\": \"${var.region}\"",
        "\"source_ami\": \"${data.amazon-ami.base.id}\"",
        "\"ami_name\": \"web-${local.stamp}\"",
        "\"stamp\": \"${data.amazon-ami.base.name}-${var.region}\"",
        "\"From\": \"${data.amazon-ami.base.tags[\\\"Name\\\"]}\"",
        "\"${local.stamp}\"",
    ] {
        assert!(text.contains(want), "{want} is missing from:\n{text}");
    }
}

#[test]
fn packers_own_expressions_are_handed_over_as_written() {
    let text = template(&with_source(
        "    - id: say\n      type: provisioner.shell-local\n      params:\n        inline:\n          - echo ${source.name} ${build.ID} ${path.root} ${packer.version}\n          - echo ${upper(var.undeclared)} ${local.elsewhere} ${data.amazon-ami.other.id}\n          - echo {{ .Vars }} {{ .Path }}\n",
    ));
    assert!(
        text.contains("echo ${source.name} ${build.ID} ${path.root} ${packer.version}"),
        "{text}"
    );
    assert!(
        text.contains(
            "echo ${upper(var.undeclared)} ${local.elsewhere} ${data.amazon-ami.other.id}"
        ),
        "{text}"
    );
    assert!(text.contains("echo {{ .Vars }} {{ .Path }}"), "{text}");
}

#[test]
fn text_that_only_looks_like_a_template_stays_literal() {
    let text = template(&with_source(
        "    - id: say\n      type: provisioner.shell\n      params:\n        inline: [\"echo $${HOME} %{if} $5\"]\n",
    ));
    assert!(text.contains("\"echo $${HOME} %%{if} $5\""), "{text}");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(&with_source(
        "    - id: say\n      type: provisioner.shell\n      params:\n        inline: [\"echo ${regoin}\"]\n",
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.target, "packer");
    assert_eq!(err.resource, "say");
    assert_eq!(
        err.msg,
        "param \"inline\": ${regoin} refers to \"regoin\", which is not a resource in this document"
    );
    assert_eq!(err.pos.file, "image.yaml");
    assert!(err.pos.line > 0);

    let err = refusal("  resources:\n    - id: broken\n      type: source.null\n      params: {communicator: \"${oops\"}\n");
    assert_eq!(err.rule, "invalid-reference");
    assert!(err.msg.contains("never closed"), "{}", err.msg);
}

#[test]
fn a_source_or_a_step_has_nothing_to_read_in_a_param() {
    let err = refusal(&with_source(
        "    - id: say\n      type: provisioner.shell\n      params:\n        inline: [\"echo ${box.id}\"]\n",
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(
        err.msg.starts_with(
            "param \"inline\": ${box.id} refers to the source \"box\", which has nothing to \
             read while it is being built."
        ),
        "{}",
        err.msg
    );

    let err = refusal(&with_source(
        "    - id: one\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n    - id: two\n      type: provisioner.shell\n      params: {inline: [\"echo ${one.status}\"]}\n",
    ));
    assert_eq!(
        err.msg,
        "param \"inline\": ${one.status} refers to \"one\", which is a provisioner and has no \
         attributes to read"
    );
}

#[test]
fn provisioners_run_in_dependency_order_not_declaration_order() {
    let json = parsed(&with_source(
        "    - id: c\n      type: provisioner.breakpoint\n      params: {note: c}\n      dependsOn:\n        - resource: b\n    - id: b\n      type: provisioner.file\n      params: {source: b, destination: /b}\n      dependsOn:\n        - resource: a\n    - id: a\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n",
    ));
    assert_eq!(
        labels(at(&json, &["build", "provisioner"])),
        ["shell", "file", "breakpoint"]
    );
}

#[test]
fn provisioners_without_dependencies_keep_the_order_they_were_written_in() {
    let json = parsed(&with_source(
        "    - id: z\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n    - id: a\n      type: provisioner.file\n      params: {source: b, destination: /b}\n    - id: m\n      type: provisioner.breakpoint\n      params: {note: m}\n",
    ));
    assert_eq!(
        labels(at(&json, &["build", "provisioner"])),
        ["shell", "file", "breakpoint"]
    );
}

#[test]
fn post_processors_that_depend_on_each_other_form_a_sequence() {
    let json = parsed(&with_source(
        "    - id: push\n      type: post-processor.docker-push\n      params: {login: false}\n      dependsOn:\n        - resource: tag\n    - id: sum\n      type: post-processor.checksum\n      params: {checksum_types: [sha256]}\n    - id: tag\n      type: post-processor.docker-tag\n      params: {repository: team/app}\n    - id: sign\n      type: post-processor.shell-local\n      params: {inline: [\"true\"]}\n      dependsOn:\n        - resource: push\n",
    ));
    let sequences = at(&json, &["build", "post-processors"]).as_list().unwrap();
    let shape: Vec<Vec<&str>> = sequences
        .iter()
        .map(|s| labels(at(s, &["post-processor"])))
        .collect();
    assert_eq!(
        shape,
        [
            vec!["checksum"],
            vec!["docker-tag", "docker-push", "shell-local"],
            vec!["manifest"],
        ]
    );
}

#[test]
fn every_build_ends_with_meridians_manifest_in_a_sequence_of_its_own() {
    let json = parsed(&with_source(""));
    assert_eq!(
        at(
            &json,
            &[
                "build",
                "post-processors",
                "0",
                "post-processor",
                "0",
                "manifest"
            ]
        ),
        &Value::Map(
            [
                (
                    "output".to_owned(),
                    Value::String("meridian-manifest.json".into())
                ),
                ("strip_path".to_owned(), Value::Bool(true)),
            ]
            .into()
        )
    );
    // No provisioners were declared, so none are written.
    assert!(!template(&with_source("")).contains("\"provisioner\""));
}

#[test]
fn a_post_processor_takes_one_artifact_and_a_sequence_does_not_fork() {
    let three = |deps_of_c: &str, deps_of_b: &str| {
        with_source(&format!(
            "    - id: a\n      type: post-processor.compress\n      params: {{output: a}}\n    - id: b\n      type: post-processor.checksum\n      params: {{output: b}}\n{deps_of_b}    - id: c\n      type: post-processor.shell-local\n      params: {{inline: [\"true\"]}}\n{deps_of_c}"
        ))
    };
    let err = refusal(&three(
        "      dependsOn:\n        - resource: a\n        - resource: b\n",
        "",
    ));
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(err.resource, "c");
    assert_eq!(
        err.msg,
        "depends on the post-processors \"a\" and \"b\", but a post-processor takes the \
         artifact of one step only"
    );

    let err = refusal(&three(
        "      dependsOn:\n        - resource: a\n",
        "      dependsOn:\n        - resource: a\n",
    ));
    assert_eq!(err.rule, "unsupported-dependency");
    assert!(err.msg.contains("A sequence does not fork"), "{}", err.msg);
}

#[test]
fn a_dependency_that_restates_the_shape_of_a_build_adds_nothing() {
    let plain = with_source(
        "    - id: v\n      type: variable\n      params: {default: x}\n    - id: setup\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n    - id: pack\n      type: post-processor.compress\n      params: {output: out.tgz}\n",
    );
    let restated = with_source(
        "    - id: v\n      type: variable\n      params: {default: x}\n    - id: setup\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n      dependsOn:\n        - resource: box\n        - resource: v\n    - id: pack\n      type: post-processor.compress\n      params: {output: out.tgz}\n      dependsOn:\n        - resource: setup\n        - resource: box\n",
    );
    assert_eq!(template(&plain), template(&restated));
    assert!(!template(&restated).contains("depends"));
}

#[test]
fn a_dependency_that_contradicts_the_shape_of_a_build_is_refused() {
    let err = refusal(&with_source(
        "    - id: pack\n      type: post-processor.compress\n      params: {output: out.tgz}\n    - id: setup\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n      dependsOn:\n        - resource: pack\n",
    ));
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(err.resource, "setup");
    assert_eq!(
        err.msg,
        "is a provisioner and depends on \"pack\", a post-processor: Packer always deals with \
         a provisioner first, and that order cannot be changed"
    );

    let err = refusal(
        "  resources:\n    - id: one\n      type: source.null\n      params: {communicator: none}\n    - id: two\n      type: source.null\n      params: {communicator: none}\n      dependsOn:\n        - resource: one\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");
    assert!(
        err.msg
            .contains("Packer builds every source of a build at the same time"),
        "{}",
        err.msg
    );
}

#[test]
fn data_sources_and_locals_are_ordered_by_reference_only() {
    // The reference is the order, so the dependency is accepted.
    let ok = "  resources:\n    - id: base\n      type: data.amazon-ami\n      params: {most_recent: true}\n    - id: label\n      type: local\n      params: {value: \"${base.name}\"}\n      dependsOn:\n        - resource: base\n    - id: box\n      type: source.null\n      params: {communicator: none}\n";
    assert!(template(ok).contains("\"label\": \"${data.amazon-ami.base.name}\""));

    let err = refusal(
        "  resources:\n    - id: base\n      type: data.amazon-ami\n      params: {most_recent: true}\n    - id: label\n      type: local\n      params: {value: fixed}\n      dependsOn:\n        - resource: base\n    - id: box\n      type: source.null\n      params: {communicator: none}\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(err.resource, "label");
    assert!(
        err.msg
            .starts_with("depends on \"base\" without reading anything from it."),
        "{}",
        err.msg
    );
}

#[test]
fn the_error_cleanup_provisioner_is_one_and_nothing_follows_it() {
    let cleanup = "    - id: tidy\n      type: error-cleanup-provisioner.shell-local\n      params: {inline: [\"echo failed\"]}\n";
    let json = parsed(&with_source(cleanup));
    assert_eq!(
        at(
            &json,
            &[
                "build",
                "error-cleanup-provisioner",
                "shell-local",
                "inline",
                "0"
            ]
        ),
        &Value::String("echo failed".into())
    );

    let err = refusal(&with_source(&format!(
        "{cleanup}    - id: again\n      type: error-cleanup-provisioner.shell-local\n      params: {{inline: [\"true\"]}}\n"
    )));
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "again");

    let err = refusal(&with_source(&format!(
        "{cleanup}    - id: after\n      type: provisioner.shell\n      params: {{inline: [\"true\"]}}\n      dependsOn:\n        - resource: tidy\n"
    )));
    assert_eq!(err.rule, "unsupported-dependency");
    assert!(err
        .msg
        .contains("only runs when the build has already failed"));
}

#[test]
fn a_source_named_by_its_id_in_only_and_except_is_completed() {
    let json = parsed(
        "  resources:\n    - id: ubuntu\n      type: source.amazon-ebs\n      params: {region: us-east-1}\n    - id: app\n      type: source.docker\n      params: {image: debian}\n    - id: apt\n      type: provisioner.shell\n      params:\n        inline: [\"true\"]\n        only: [ubuntu, docker.app]\n    - id: tag\n      type: post-processor.docker-tag\n      params:\n        repository: team/app\n        except: [ubuntu]\n",
    );
    assert_eq!(
        at(&json, &["build", "provisioner", "0", "shell", "only"]),
        &Value::List(vec![
            Value::String("amazon-ebs.ubuntu".into()),
            Value::String("docker.app".into()),
        ])
    );
    assert_eq!(
        at(
            &json,
            &[
                "build",
                "post-processors",
                "0",
                "post-processor",
                "0",
                "docker-tag",
                "except"
            ]
        ),
        &Value::List(vec![Value::String("amazon-ebs.ubuntu".into())])
    );
}

#[test]
fn plugins_variables_and_locals_are_written_where_packer_expects_them() {
    let json = parsed(&with_source(
        "    - id: amazon\n      type: plugin\n      params: {source: github.com/hashicorp/amazon, version: \">= 1.3.0\"}\n    - id: region\n      type: variable\n      params: {type: string, default: us-east-1, sensitive: true}\n    - id: sizes\n      type: local\n      params:\n        value: {small: t3.small, ports: [80, 443]}\n",
    ));
    assert_eq!(
        at(&json, &["packer", "required_plugins", "amazon", "source"]),
        &Value::String("github.com/hashicorp/amazon".into())
    );
    assert_eq!(
        at(&json, &["variable", "region", "sensitive"]),
        &Value::Bool(true)
    );
    assert_eq!(
        at(&json, &["locals", "sizes", "ports", "1"]),
        &Value::Int(443)
    );
}

#[test]
fn a_plugin_or_a_local_with_the_wrong_params_is_refused() {
    let err = refusal(&with_source(
        "    - id: amazon\n      type: plugin\n      params: {source: github.com/hashicorp/amazon}\n",
    ));
    assert_eq!(err.rule, "missing-param");
    assert_eq!(
        err.msg,
        "resource type \"plugin\" requires the string param \"version\" on this target"
    );

    let err = refusal(&with_source(
        "    - id: amazon\n      type: plugin\n      params: {source: s, version: v, region: us-east-1}\n",
    ));
    assert_eq!(err.rule, "invalid-param");

    let err = refusal(&with_source(
        "    - id: stamp\n      type: local\n      params: {expression: x}\n",
    ));
    assert_eq!(err.rule, "missing-param");

    let err = refusal(&with_source(
        "    - id: stamp\n      type: local\n      params: {value: x, sensitive: true}\n",
    ));
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "a local takes the param \"value\" only, not \"sensitive\""
    );
}

#[test]
fn a_type_that_is_not_packers_is_refused() {
    for bad in [
        "package",
        "aws_instance",
        "source",
        "source.",
        "source.Amazon",
        "builder.null",
    ] {
        let err = refusal(&format!(
            "  resources:\n    - id: x\n      type: {bad}\n      params: {{name: nginx}}\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{bad}");
        assert!(
            err.msg.ends_with(
                "is not part of a Packer build. Use source.<builder>, such as \
                 source.amazon-ebs, or one of: provisioner.<type>, post-processor.<type>, \
                 error-cleanup-provisioner.<type>, data.<type>, variable, local, plugin"
            ),
            "{}",
            err.msg
        );
    }
}

#[test]
fn a_build_without_a_source_is_refused() {
    let err = refusal(
        "  resources:\n    - id: say\n      type: provisioner.shell-local\n      params: {inline: [\"true\"]}\n",
    );
    assert_eq!(err.rule, "missing-param");
    assert!(err.msg.starts_with("a build needs at least one source"));
    assert_eq!(err.pos.file, "image.yaml");
}

#[test]
fn an_id_that_cannot_be_a_packer_name_is_refused() {
    let err = refusal(
        "  resources:\n    - id: 22-04\n      type: source.null\n      params: {communicator: none}\n",
    );
    assert_eq!(err.rule, "invalid-name");
    assert_eq!(err.resource, "22-04");
}

#[test]
fn absent_and_runtime_conditions_are_refused_with_what_to_do_instead() {
    let err = refusal(
        "  resources:\n    - id: old\n      type: source.null\n      state: absent\n      params: {communicator: none}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert!(
        err.msg.contains("remove the resource from the document"),
        "{}",
        err.msg
    );

    let err = refusal(
        "  resources:\n    - id: old\n      type: source.null\n      state: stopped\n      params: {communicator: none}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert!(err.msg.contains("(accepted: present)"), "{}", err.msg);
}

#[test]
fn outputs_are_recorded_for_the_runner_and_checked_against_the_sources() {
    let emitted = compile(&format!(
        "{}  outputs:\n    - name: image\n      value: ${{box.id}}\n    - name: note\n      value: \"$${{literal}} ${{ box.builder_type }}\"\n",
        with_source("")
    ))
    .unwrap();
    let described = decode_value(emitted.artifact.files[BUILD_PATH].as_bytes()).unwrap();
    assert_eq!(
        at(&described, &["outputs", "image"]),
        &Value::String("${box.id}".into())
    );
    assert_eq!(
        at(&described, &["outputs", "note"]),
        &Value::String("$${literal} ${box.builder_type}".into())
    );
    assert_eq!(
        at(&described, &["sources", "0", "type"]),
        &Value::String("null".into())
    );
    assert_eq!(at(&described, &["build"]), &Value::String("img".into()));
    // Outputs are Meridian's business: Packer's template does not mention them.
    assert!(!emitted.artifact.files[CONFIG_PATH].contains("image"));

    let err = refusal(&format!(
        "{}  outputs:\n    - name: ip\n      value: ${{box.public_ip}}\n",
        with_source("")
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(
        err.msg,
        "output \"ip\": ${box.public_ip} reads \".public_ip\" of the source \"box\", which a \
         build does not report (attributes: id, artifact_id, builder_type, build_time, files, \
         packer_run_uuid)"
    );

    let err = refusal(&format!(
        "{}    - id: region\n      type: variable\n      params: {{default: x}}\n  outputs:\n    - name: region\n      value: ${{region}}\n",
        with_source("")
    ));
    assert!(
        err.msg.starts_with(
            "output \"region\": ${region} does not refer to a source of this document."
        ),
        "{}",
        err.msg
    );
}

#[test]
fn a_notify_is_refused_because_nothing_in_a_build_reacts_to_a_change() {
    let err = refusal(&with_source(
        "    - id: conf\n      type: provisioner.file\n      params: {source: a, destination: /a}\n      notifies:\n        - resource: reload\n          action: restart\n    - id: reload\n      type: provisioner.shell\n      params: {inline: [\"true\"]}\n",
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "conf");
    assert_eq!(
        err.msg,
        "notifies resource \"reload\" with action \"restart\", which this target cannot \
         express: a Packer build runs each step once on a machine it then discards, so nothing \
         in it can react to a change"
    );
}

// ---- secrets ------------------------------------------------------------

/// The build with a file provisioner whose content is a secret.
fn with_secret() -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: img\nspec:\n  target: packer\n{}",
        with_source(
            "    - id: conf\n      type: provisioner.file\n      params:\n        destination: /etc/app.conf\n        content: placeholder\n"
        )
    );
    let mut graph = graph_from_text("image.yaml", &text, "packer");
    secret_in_param(&mut graph, "conf", "content", "db_password");
    graph
}

#[test]
fn a_secret_in_a_param_is_a_sensitive_variable_the_runner_fills_from_a_file() {
    let emitted = emit(&with_secret()).unwrap();
    let text = &emitted.artifact.files[CONFIG_PATH];
    assert!(
        text.contains("\"content\": \"${var.meridian_secret_db_password}\""),
        "{text}"
    );
    assert!(!text.contains("(secret "), "{text}");
    let root = decode_value(text.as_bytes()).unwrap();
    let variable =
        &root.as_map().unwrap()["variable"].as_map().unwrap()["meridian_secret_db_password"];
    assert_eq!(
        variable,
        &Value::Map(
            [
                ("sensitive".to_owned(), Value::Bool(true)),
                ("type".to_owned(), Value::from("string")),
            ]
            .into()
        )
    );
    assert_eq!(
        meridian_target_packer::secret_variable("db_password"),
        "meridian_secret_db_password"
    );
    assert_eq!(
        meridian_target_packer::secret_reference("db_password"),
        "${var.meridian_secret_db_password}"
    );
    assert!(Packer.capabilities().secrets);
}

#[test]
fn a_secret_in_a_variables_declaration_is_refused_because_it_would_be_written_as_it_stands() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: img\nspec:\n  target: packer\n{}",
        with_source("    - id: token\n      type: variable\n      params:\n        type: string\n        default: placeholder\n")
    );
    let mut graph = graph_from_text("image.yaml", &text, "packer");
    secret_in_param(&mut graph, "token", "default", "db_password");
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-secret-place");
    assert!(
        err.msg
            .starts_with("param \"default\" of the variable holds a secret"),
        "{}",
        err.msg
    );
}
