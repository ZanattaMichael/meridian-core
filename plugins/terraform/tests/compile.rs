//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_terraform::Terraform;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Terraform.emit(graph, &Data::new())
}

/// An infrastructure document with the given `spec` body, compiled.
fn compile(spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: terraform\n{spec}"
    );
    emit(&graph_from_text("infra.yaml", &text, "terraform"))
}

fn config(spec: &str) -> String {
    compile(spec).unwrap().artifact.files["main.tf.json"].clone()
}

fn refusal(spec: &str) -> CompileError {
    compile(spec).expect_err("the document should be refused")
}

#[test]
fn the_example_compiles_to_its_golden_configuration() {
    let graph = graph_from_file("examples/terraform/web-vm.yaml", "terraform");
    assert_golden(&golden(), "web-vm", &emit(&graph).unwrap());
}

#[test]
fn compiling_is_deterministic() {
    let graph = graph_from_file("examples/terraform/web-vm.yaml", "terraform");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn a_reference_becomes_terraforms_address_and_orders_without_depends_on() {
    let text = config(
        "  resources:\n    - id: vpc\n      type: aws_vpc\n      params: {cidr_block: 10.0.0.0/16}\n    - id: subnet\n      type: aws_subnet\n      params:\n        vpc_id: ${vpc.id}\n        cidr_block: 10.0.1.0/24\n      dependsOn:\n        - resource: vpc\n",
    );
    assert!(text.contains("\"vpc_id\": \"${aws_vpc.vpc.id}\""), "{text}");
    assert!(!text.contains("depends_on"), "{text}");
}

#[test]
fn an_order_the_arguments_do_not_show_becomes_depends_on() {
    let text = config(
        "  resources:\n    - id: role\n      type: aws_iam_role\n      params: {name: app}\n    - id: vm\n      type: aws_instance\n      params: {ami: ami-1, instance_type: t3.micro}\n      dependsOn:\n        - resource: role\n",
    );
    assert!(
        text.contains("\"depends_on\": [\n          \"aws_iam_role.role\"\n        ]"),
        "{text}"
    );
}

#[test]
fn text_that_only_looks_like_a_template_stays_literal() {
    let text = config(
        "  resources:\n    - id: vm\n      type: aws_instance\n      params:\n        user_data: \"echo $${HOME} %{if}\"\n        ami: ${upper(var.image)}\n",
    );
    assert!(
        text.contains("\"user_data\": \"echo $${HOME} %%{if}\""),
        "{text}"
    );
    // A function call is Terraform's own language and is handed over as written.
    assert!(text.contains("\"ami\": \"${upper(var.image)}\""), "{text}");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws_instance\n      params:\n        subnet_id: ${subnett.id}\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "vm");
    assert_eq!(
        err.msg,
        "param \"subnet_id\": ${subnett.id} refers to \"subnett\", which is not a resource in this document"
    );
    assert_eq!(err.pos.file, "infra.yaml");

    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws_instance\n      params: {ami: ami-1}\n  outputs:\n    - name: ip\n      value: ${vn.public_ip}\n",
    );
    assert_eq!(
        err.msg,
        "output \"ip\": ${vn.public_ip} refers to \"vn\", which is not a resource in this document"
    );
}

#[test]
fn a_type_that_is_not_terraforms_is_refused() {
    let err = refusal(
        "  resources:\n    - id: nginx\n      type: package\n      params: {name: nginx}\n",
    );
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(
        err.msg,
        "\"package\" is not a Terraform resource type. Use a provider's own type, such as \
         aws_instance, or one of: provider, backend, module, variable, local, data.<type>"
    );
}

#[test]
fn absent_is_refused_with_what_to_do_instead() {
    let err = refusal(
        "  resources:\n    - id: old\n      type: aws_instance\n      state: absent\n      params: {ami: ami-1}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert!(
        err.msg.contains("remove the resource from the document"),
        "{}",
        err.msg
    );
}

#[test]
fn a_notify_becomes_a_forced_replacement_and_says_so() {
    let emitted = compile(
        "  resources:\n    - id: cert\n      type: aws_acm_certificate\n      params: {domain_name: example.com}\n      notifies:\n        - resource: lb\n          action: replace\n    - id: lb\n      type: aws_lb\n      params: {name: web}\n",
    )
    .unwrap();
    let text = &emitted.artifact.files["main.tf.json"];
    assert!(
        text.contains(
            "\"replace_triggered_by\": [\n            \"aws_acm_certificate.cert\"\n          ]"
        ),
        "{text}"
    );
    assert_eq!(emitted.warnings.len(), 1);
    assert_eq!(
        emitted.warnings[0].to_string(),
        "terraform: lb: is destroyed and created again whenever \"cert\" changes; Terraform has \
         no gentler way to react to a change (replace_triggered_by)"
    );

    let err = refusal(
        "  resources:\n    - id: cert\n      type: aws_acm_certificate\n      params: {domain_name: example.com}\n      notifies:\n        - resource: lb\n          action: restart\n    - id: lb\n      type: aws_lb\n      params: {name: web}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "cert");
}

#[test]
fn two_configurations_of_one_provider_need_aliases() {
    let text = config(
        "  resources:\n    - id: east\n      type: provider\n      params: {name: aws, region: us-east-1}\n    - id: west\n      type: provider\n      params: {name: aws, alias: west, region: us-west-2}\n    - id: bucket\n      type: aws_s3_bucket\n      params: {bucket: logs, provider: aws.west}\n",
    );
    assert!(text.contains("\"provider\": {\n    \"aws\": [\n"), "{text}");
    assert!(text.contains("\"alias\": \"west\""), "{text}");

    let err = refusal(
        "  resources:\n    - id: east\n      type: provider\n      params: {name: aws, region: us-east-1}\n    - id: west\n      type: provider\n      params: {name: aws, region: us-west-2}\n",
    );
    assert_eq!(err.rule, "duplicate-provider");
    assert_eq!(
        err.msg,
        "provider \"aws\" is configured twice; give all but one an alias"
    );
}

#[test]
fn a_backend_and_a_module_are_written_where_terraform_expects_them() {
    let text = config(
        "  resources:\n    - id: s3\n      type: backend\n      params: {bucket: tfstate, key: net.tfstate, region: us-east-1}\n    - id: network\n      type: module\n      params:\n        source: terraform-aws-modules/vpc/aws\n        version: 5.1.0\n        cidr: 10.0.0.0/16\n    - id: vm\n      type: aws_instance\n      params:\n        subnet_id: ${network.public_subnets[0]}\n",
    );
    assert!(text.contains("\"backend\": {\n      \"s3\": {"), "{text}");
    assert!(text.contains("\"module\": {\n    \"network\": {"), "{text}");
    assert!(
        text.contains("\"subnet_id\": \"${module.network.public_subnets[0]}\""),
        "{text}"
    );

    let err = refusal(
        "  resources:\n    - id: network\n      type: module\n      params: {cidr: 10.0.0.0/16}\n",
    );
    assert_eq!(err.rule, "missing-param");
}

#[test]
fn nothing_can_depend_on_a_provider() {
    let err = refusal(
        "  resources:\n    - id: aws\n      type: provider\n      params: {region: us-east-1}\n    - id: vm\n      type: aws_instance\n      params: {ami: ami-1}\n      dependsOn:\n        - resource: aws\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");
}

#[test]
fn a_variable_is_declared_and_read_as_var() {
    let text = config(
        "  resources:\n    - id: user_data\n      type: variable\n      params:\n        type: string\n        sensitive: true\n    - id: sizes\n      type: variable\n      params:\n        type: map(string)\n        default: {web: t3.micro}\n    - id: web\n      type: aws_instance\n      params:\n        ami: ami-1\n        instance_type: ${sizes.web}\n        user_data: ${user_data}\n  outputs:\n    - name: size\n      value: ${sizes.web}\n",
    );
    assert!(
        text.contains("\"variable\": {\n    \"sizes\": {\n      \"default\": {\n        \"web\": \"t3.micro\"\n      },\n      \"type\": \"map(string)\"\n    },\n    \"user_data\": {\n      \"sensitive\": true,\n      \"type\": \"string\"\n    }\n  }"),
        "{text}"
    );
    assert!(
        text.contains("\"user_data\": \"${var.user_data}\""),
        "{text}"
    );
    assert!(
        text.contains("\"instance_type\": \"${var.sizes.web}\""),
        "{text}"
    );
    assert!(text.contains("\"value\": \"${var.sizes.web}\""), "{text}");
    // A variable is known before anything is created, so it orders nothing.
    assert!(!text.contains("depends_on"), "{text}");
}

#[test]
fn nothing_can_depend_on_a_variable() {
    let err = refusal(
        "  resources:\n    - id: region\n      type: variable\n      params: {type: string}\n    - id: vm\n      type: aws_instance\n      params: {ami: ami-1}\n      dependsOn:\n        - resource: region\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(
        err.msg,
        "\"region\" is a variable, which Terraform works out before or alongside everything \
         else; nothing can depend on it or be ordered by it"
    );
}
