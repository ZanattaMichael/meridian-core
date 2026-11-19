//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_in_param};
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
    assert_eq!(err.rule, "duplicate-resource");
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

#[test]
fn a_secret_in_a_param_is_a_sensitive_variable_the_runner_hands_over() {
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: db\nspec:\n  target: terraform\n  resources:\n    - id: main\n      type: aws_db_instance\n      params:\n        engine: postgres\n        password: placeholder\n";
    let mut graph = graph_from_text("infra.yaml", text, "terraform");
    secret_in_param(&mut graph, "main", "password", "db_password");
    let config = emit(&graph).unwrap().artifact.files["main.tf.json"].clone();
    assert!(
        config.contains("\"password\": \"${var.meridian_secret_db_password}\""),
        "{config}"
    );
    assert!(
        config.contains(
            "\"variable\": {\n    \"meridian_secret_db_password\": {\n      \"sensitive\": true,\n      \"type\": \"string\"\n    }\n  }"
        ),
        "{config}"
    );
    assert!(!config.contains("(sensitive)"), "{config}");
}

// ---- the common infrastructure types ------------------------------------

#[test]
fn the_common_types_compile_to_their_golden_configuration_on_aws_and_azure() {
    for name in ["web-aws", "web-azure", "web-gcp"] {
        let graph = graph_from_file(&format!("examples/common/{name}.yaml"), "terraform");
        assert_golden(&golden(), &format!("common-{name}"), &emit(&graph).unwrap());
    }
}

const COMMON_NET: &str = "    - id: net\n      type: network\n      params: {cidr: 10.0.0.0/16}\n";

#[test]
fn the_cloud_is_the_one_provider_the_document_configures() {
    let text = config(&format!(
        "  resources:\n    - id: azurerm\n      type: provider\n      params: {{features: {{}}}}\n{COMMON_NET}  settings:\n    location: westeurope\n    resourceGroup: rg\n"
    ));
    assert!(text.contains("azurerm_virtual_network"), "{text}");
    let text = config(&format!(
        "  resources:\n    - id: aws\n      type: provider\n      params: {{region: eu-west-1}}\n{COMMON_NET}"
    ));
    assert!(text.contains("\"aws_vpc\""), "{text}");
}

#[test]
fn a_document_that_does_not_say_its_cloud_is_refused_with_how_to_say_it() {
    let err = refusal(&format!("  resources:\n{COMMON_NET}"));
    assert_eq!(err.rule, "missing-resource");
    assert_eq!(err.target, "terraform");
    assert!(
        err.msg.contains("spec.settings.cloud (aws, azure, gcp)"),
        "{}",
        err.msg
    );
}

#[test]
fn a_cloud_this_target_does_not_map_is_refused() {
    let err = refusal(&format!(
        "  settings:\n    cloud: oracle\n  resources:\n{COMMON_NET}"
    ));
    assert_eq!(err.rule, "invalid-setting");
    assert!(err.msg.contains("aws, azure, gcp"), "{}", err.msg);
}

#[test]
fn azure_settings_without_common_types_are_refused() {
    let err = refusal(
        "  settings:\n    resourceGroup: rg\n  resources:\n    - id: rg\n      type: azurerm_resource_group\n      params: {name: rg, location: westeurope}\n",
    );
    assert_eq!(err.rule, "invalid-setting");
}

#[test]
fn a_size_is_the_smallest_that_fits_and_an_image_is_looked_up_by_its_publisher() {
    let text = config(&format!(
        "  settings:\n    cloud: aws\n  resources:\n{COMMON_NET}    - id: sub\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24}}\n    - id: vm\n      type: vm\n      params:\n        subnet: \"${{sub}}\"\n        size: {{cpus: 3, memoryGb: 1}}\n        image: {{family: debian, version: \"12\"}}\n"
    ));
    assert!(text.contains("\"instance_type\": \"t3.xlarge\""), "{text}");
    assert!(text.contains("\"136693071363\""), "{text}");
    assert!(
        text.contains("\"ami\": \"${data.aws_ami.vm_image.id}\""),
        "{text}"
    );
    assert!(!text.contains("key_name"), "no key, no key pair: {text}");
}

#[test]
fn a_firewall_with_no_outbound_rule_lets_everything_out() {
    let text = config(&format!(
        "  settings:\n    cloud: aws\n  resources:\n{COMMON_NET}    - id: fw\n      type: firewall\n      params: {{network: \"${{net}}\"}}\n"
    ));
    assert!(text.contains("fw_out_all"), "{text}");
    let text = config(&format!(
        "  settings:\n    cloud: aws\n  resources:\n{COMMON_NET}    - id: fw\n      type: firewall\n      params:\n        network: \"${{net}}\"\n        rules: [{{direction: out, cidr: \"::/0\"}}]\n"
    ));
    assert!(!text.contains("fw_out_all"), "{text}");
    assert!(text.contains("\"cidr_ipv6\": \"::/0\""), "{text}");
    assert!(text.contains("\"to_port\": 65535"), "{text}");
}

#[test]
fn a_common_resource_mixes_with_native_ones_and_keeps_its_id() {
    let text = config(&format!(
        "  settings:\n    cloud: aws\n  resources:\n{COMMON_NET}    - id: flow\n      type: aws_flow_log\n      params:\n        vpc_id: ${{net.id}}\n        traffic_type: ALL\n      dependsOn:\n        - resource: net\n"
    ));
    assert!(text.contains("\"vpc_id\": \"${aws_vpc.net.id}\""), "{text}");
}

#[test]
fn adopt_becomes_an_import_block_for_a_managed_resource() {
    let text = config(
        "  resources:\n    - id: logs\n      type: aws_s3_bucket\n      adopt: example-logs\n      params: {bucket: example-logs}\n    - id: web\n      type: aws_instance\n      adopt: i-0abc123\n      params: {ami: ami-1}\n",
    );
    assert!(
        text.contains(
            "\"import\": [\n    {\n      \"id\": \"example-logs\",\n      \"to\": \"aws_s3_bucket.logs\"\n    },\n    {\n      \"id\": \"i-0abc123\",\n      \"to\": \"aws_instance.web\"\n    }\n  ]"
        ),
        "{text}"
    );
}

#[test]
fn adopt_on_a_data_source_or_a_provider_is_refused() {
    for (r#type, params) in [
        ("data.aws_ami", "{owners: [self]}"),
        ("provider", "{region: us-east-1}"),
    ] {
        let e = refusal(&format!(
            "  resources:\n    - id: x\n      type: {type}\n      adopt: whatever\n      params: {params}\n"
        ));
        assert_eq!(e.rule, "invalid-adopt", "{e}");
        assert!(
            e.msg.contains("only a managed resource can be adopted"),
            "{e}"
        );
    }
}

#[test]
fn the_adopt_example_compiles_to_its_golden_configuration() {
    let graph = graph_from_file("examples/terraform/adopt.yaml", "terraform");
    assert_golden(&golden(), "adopt", &emit(&graph).unwrap());
}

// ---- the common types on Google Cloud ----

const GCP: &str = "  settings:\n    cloud: gcp\n    location: europe-west1\n  resources:\n";

#[test]
fn a_gcp_firewall_rule_applies_to_the_machines_that_carry_its_tag() {
    let text = config(&format!(
        "{GCP}{COMMON_NET}    - id: web_fw\n      type: firewall\n      params:\n        network: \"${{net}}\"\n        rules: [{{port: \"80\", cidr: 0.0.0.0/0}}, {{direction: out, protocol: any, cidr: 10.0.0.0/8}}]\n    - id: s\n      type: subnet\n      params: {{network: \"${{net}}\", cidr: 10.0.1.0/24}}\n    - id: vm\n      type: vm\n      params: {{subnet: \"${{s}}\", size: e2-small, image: my-image, firewalls: [\"${{web_fw}}\"]}}\n"
    ));
    assert_eq!(text.matches("\"meridian-web-fw\"").count(), 4, "{text}");
    assert!(text.contains("\"EGRESS\""), "{text}");
    assert!(text.contains("\"priority\": 65534"), "{text}");
    assert!(text.contains("\"zone\": \"europe-west1-b\""), "{text}");
}

#[test]
fn a_gcp_network_takes_no_location_and_a_firewall_needs_a_rule() {
    let err = refusal(&format!(
        "{GCP}    - id: net\n      type: network\n      params: {{cidr: 10.0.0.0/16, location: europe-west1}}\n"
    ));
    assert_eq!(err.rule, "unsupported-param");
    let err = refusal(&format!(
        "{GCP}{COMMON_NET}    - id: fw\n      type: firewall\n      params: {{network: \"${{net}}\"}}\n"
    ));
    assert_eq!(err.rule, "missing-param");
}

#[test]
fn a_gcp_resource_group_setting_is_refused() {
    let err = refusal(&format!(
        "  settings:\n    cloud: gcp\n    location: europe-west1\n    resourceGroup: rg\n  resources:\n{COMMON_NET}"
    ));
    assert_eq!(err.rule, "invalid-setting");
}
