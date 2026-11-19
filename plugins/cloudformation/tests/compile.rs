//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_in_param};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_cloudformation::CloudFormation;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    CloudFormation.emit(graph, &Data::new())
}

fn compile_named(name: &str, spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: {name}\nspec:\n  target: cloudformation\n{spec}"
    );
    emit(&graph_from_text("infra.yaml", &text, "cloudformation"))
}

fn template(spec: &str) -> String {
    compile_named("net", spec).unwrap().artifact.files["template.yaml"].clone()
}

fn refusal(spec: &str) -> CompileError {
    compile_named("net", spec).expect_err("the document should be refused")
}

const BUCKET: &str =
    "    - id: logs\n      type: AWS::S3::Bucket\n      params: {BucketName: logs}\n";

#[test]
fn the_example_compiles_to_its_golden_template() {
    let graph = graph_from_file("examples/cloudformation/web-vm.yaml", "cloudformation");
    assert_golden(&golden(), "web-vm", &emit(&graph).unwrap());
}

#[test]
fn references_become_ref_getatt_and_sub() {
    let text = template(&format!(
        "  resources:\n{BUCKET}    - id: reader\n      type: AWS::IAM::Policy\n      params:\n        Bucket: ${{logs}}\n        Arn: ${{logs.Arn}}\n        Resource: ${{logs.Arn}}/*\n        Literal: cost is $${{5}}\n        Region: ${{AWS::Region}}\n"
    ));
    assert!(text.contains("Bucket:\n        Ref: Logs\n"), "{text}");
    assert!(
        text.contains("Arn:\n        Fn::GetAtt:\n          - Logs\n          - Arn\n"),
        "{text}"
    );
    assert!(text.contains("Fn::Sub: ${Logs.Arn}/*\n"), "{text}");
    assert!(text.contains("Literal: cost is ${5}\n"), "{text}");
    assert!(
        text.contains("Region:\n        Ref: AWS::Region\n"),
        "{text}"
    );
    // Every one of those references already orders the policy after the bucket.
    assert!(!text.contains("DependsOn"), "{text}");
}

#[test]
fn a_literal_dollar_brace_inside_a_sub_is_escaped() {
    let text = template(&format!(
        "  resources:\n{BUCKET}    - id: fn\n      type: AWS::Lambda::Function\n      params:\n        Code: \"echo $${{HOME}} ${{logs}}\"\n"
    ));
    assert!(text.contains("Fn::Sub: echo ${!HOME} ${Logs}\n"), "{text}");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(&format!(
        "  resources:\n{BUCKET}    - id: fn\n      type: AWS::Lambda::Function\n      params:\n        Bucket: ${{log.Arn}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(
        err.msg,
        "param \"Bucket\": ${log.Arn} refers to \"log\", which is not a resource in this document"
    );
}

#[test]
fn a_notify_is_refused_because_nothing_could_carry_it() {
    let err = refusal(&format!(
        "  resources:\n{BUCKET}    - id: fn\n      type: AWS::Lambda::Function\n      params: {{Handler: x}}\n      notifies:\n        - resource: logs\n          action: restart\n"
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "fn");
}

#[test]
fn a_type_that_is_not_cloudformations_is_refused() {
    let err =
        refusal("  resources:\n    - id: vm\n      type: aws_instance\n      params: {ami: x}\n");
    assert_eq!(err.rule, "unsupported-resource-type");
}

#[test]
fn two_ids_that_collapse_to_one_logical_id_are_refused() {
    let err = refusal(
        "  resources:\n    - id: web_vm\n      type: AWS::EC2::Instance\n      params: {ImageId: a}\n    - id: web-vm\n      type: AWS::EC2::Instance\n      params: {ImageId: b}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(
        err.msg,
        "resource ids \"web_vm\" and \"web-vm\" both become the logical id \"WebVm\"; \
         CloudFormation's logical ids hold only letters and digits"
    );
}

#[test]
fn the_stack_is_named_by_the_document_unless_told() {
    let files = compile_named("web-vm", &format!("  resources:\n{BUCKET}"))
        .unwrap()
        .artifact
        .files;
    assert!(files["stack.json"].contains("\"name\": \"web-vm\""));

    let err = compile_named("web_vm", &format!("  resources:\n{BUCKET}")).unwrap_err();
    assert_eq!(err.rule, "invalid-name");

    let files = compile_named(
        "web_vm",
        &format!("  resources:\n{BUCKET}    - id: s\n      type: stack\n      params: {{name: web-vm}}\n"),
    )
    .unwrap()
    .artifact
    .files;
    assert!(files["stack.json"].contains("\"name\": \"web-vm\""));
    assert!(!files["template.yaml"].contains("stack"));
}

#[test]
fn stack_settings_are_checked() {
    let err = refusal(&format!(
        "  resources:\n{BUCKET}    - id: s\n      type: stack\n      params: {{capabilities: [CAPABILITY_EVERYTHING]}}\n"
    ));
    assert_eq!(err.rule, "invalid-param");
    let err = refusal(&format!(
        "  resources:\n{BUCKET}    - id: s\n      type: stack\n      params: {{profile: prod}}\n"
    ));
    assert_eq!(err.rule, "unsupported-param");
}

// ---- secrets ------------------------------------------------------------

/// A parameter in Systems Manager whose value is a secret.
fn with_secret(id: &str) -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: cloudformation\n  resources:\n{BUCKET}    - id: {id}\n      type: AWS::SSM::Parameter\n      params:\n        Name: /app/db\n        Type: String\n        Value: placeholder\n"
    );
    let mut graph = graph_from_text("infra.yaml", &text, "cloudformation");
    secret_in_param(&mut graph, id, "Value", "db_password");
    graph
}

#[test]
fn a_secret_in_a_param_is_a_noecho_parameter_the_runner_fills_from_a_file() {
    let emitted = emit(&with_secret("db")).unwrap();
    let text = &emitted.artifact.files["template.yaml"];
    assert!(
        text.contains(
            "Parameters:\n  MeridianSecretDbPassword:\n    Type: String\n    NoEcho: true\n"
        ),
        "{text}"
    );
    assert!(text.contains("Ref: MeridianSecretDbPassword"), "{text}");
    assert!(!text.contains("(secret "), "{text}");
    // A parameter is nothing to order after.
    assert!(!text.contains("DependsOn"), "{text}");
    assert_eq!(
        meridian_target_cloudformation::secret_parameter("db_password").as_deref(),
        Some("MeridianSecretDbPassword")
    );
    assert_eq!(
        meridian_target_cloudformation::secret_reference("db_password"),
        "${meridian_secret_db_password}"
    );
    assert!(CloudFormation.capabilities().secrets);
}

#[test]
fn a_secret_whose_parameter_name_is_also_a_resources_logical_id_is_refused() {
    let err = emit(&with_secret("meridian_secret_db_password")).unwrap_err();
    assert_eq!(err.rule, "invalid-name");
    assert!(
        err.msg
            .contains("secret \"db_password\" would be the parameter MeridianSecretDbPassword"),
        "{}",
        err.msg
    );
}

#[test]
fn a_secret_in_the_stacks_settings_is_refused_because_they_are_the_command_line() {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: cloudformation\n  resources:\n{BUCKET}    - id: s\n      type: stack\n      params:\n        region: us-east-1\n"
    );
    let mut graph = graph_from_text("infra.yaml", &text, "cloudformation");
    secret_in_param(&mut graph, "s", "region", "db_password");
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-secret-place");
    assert!(
        err.msg.starts_with("param \"region\" of the stack holds a secret, and the stack's settings are the command line"),
        "{}",
        err.msg
    );
}

#[test]
fn an_aws_credential_leaves_nothing_in_the_template_and_another_cloud_is_refused() {
    let spec = |kind: &str, fields: &str| {
        format!(
            "  secrets:\n    - name: cloud_secret\n      from: env\n      key: CLOUD_SECRET\n  \
             credentials:\n    - {{name: prod, type: {kind}, {fields}}}\n  \
             endpoint: {{type: cloud, credential: prod}}\n  \
             resources:\n{BUCKET}"
        )
    };
    let compiled = compile_named(
        "net",
        &spec(
            "aws",
            "accessKeyId: AKIA1, secretAccessKey: \"${{ secret.cloud_secret }}\", region: eu-west-1",
        ),
    )
    .unwrap();
    let text: String = compiled.artifact.files.values().cloned().collect();
    for part in ["AKIA1", "cloud_secret", "prod"] {
        assert!(!text.contains(part), "{part} is in the template:\n{text}");
    }
    let err = refusal(&spec(
        "azure",
        "clientId: c, tenantId: t, clientSecret: \"${{ secret.cloud_secret }}\"",
    ));
    assert_eq!(err.rule, "unsupported-credential");
    assert_eq!(
        err.msg,
        "the credential \"prod\": aws cloudformation reaches AWS only, as an aws credential, \
         not azure"
    );
}

// ---- the common infrastructure types ----

#[test]
fn the_common_types_compile_to_their_golden_template() {
    let graph = graph_from_file("examples/common/web-cloudformation.yaml", "cloudformation");
    let emitted = emit(&graph).unwrap();
    assert!(!emitted.artifact.files["template.yaml"].contains("${"));
    assert_golden(&golden(), "common-web-cloudformation", &emitted);
}

const NET: &str = "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n";

#[test]
fn a_public_subnet_routes_through_a_gateway_only_once_it_is_attached() {
    let text = template(&format!(
        "  resources:\n{NET}    - id: a\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24, public: true}}\n    - id: b\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.2.0/24, public: true}}\n"
    ));
    assert_eq!(
        text.matches("Type: AWS::EC2::InternetGateway").count(),
        1,
        "{text}"
    );
    assert_eq!(text.matches("- NetGatewayLink").count(), 2, "{text}");
}

#[test]
fn a_firewall_with_no_outbound_rule_keeps_the_default_and_one_with_one_replaces_it() {
    let text = template(&format!(
        "  resources:\n{NET}    - id: fw\n      type: firewall\n      params:\n        network: \"${{net}}\"\n        rules: [{{port: \"22\", cidr: 10.0.0.0/8}}]\n"
    ));
    assert!(!text.contains("SecurityGroupEgress"), "{text}");
    let text = template(&format!(
        "  resources:\n{NET}    - id: fw\n      type: firewall\n      params:\n        network: \"${{net}}\"\n        rules: [{{direction: out, protocol: any, cidr: \"::/0\"}}]\n"
    ));
    assert!(text.contains("SecurityGroupEgress"), "{text}");
    assert!(text.contains("CidrIpv6: ::/0"), "{text}");
    assert!(text.contains("IpProtocol: \"-1\""), "{text}");
}

#[test]
fn an_image_by_family_is_read_from_the_vendors_public_parameter() {
    let text = template(&format!(
        "  resources:\n{NET}    - id: s\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24}}\n    - id: vm\n      type: vm\n      params: {{subnet: \"${{s}}\", size: t3.micro, image: {{family: debian, version: \"12\"}}}}\n"
    ));
    assert!(
        text.contains("'{{resolve:ssm:/aws/service/debian/release/12/latest/amd64}}'"),
        "{text}"
    );
}

#[test]
fn a_location_on_anything_but_a_subnet_is_refused() {
    let err = refusal(
        "  resources:\n    - id: ip\n      type: address\n      params: {location: eu-west-1}\n",
    );
    assert_eq!(err.target, "cloudformation");
    assert_eq!(err.rule, "unsupported-param");
    assert!(err.msg.contains("stack resource"), "{}", err.msg);
}
