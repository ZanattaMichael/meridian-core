//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_in_param};
use meridian_sdk::{Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_pulumi::Pulumi;

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Pulumi.emit(graph, &Data::new())
}

/// An infrastructure document called `name` with the given `spec` body.
fn compile_named(name: &str, spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: {name}\nspec:\n  target: pulumi\n{spec}"
    );
    emit(&graph_from_text("infra.yaml", &text, "pulumi"))
}

fn compile(spec: &str) -> Result<Emitted, CompileError> {
    compile_named("net", spec)
}

fn program(spec: &str) -> String {
    compile(spec).unwrap().artifact.files["Pulumi.yaml"].clone()
}

fn settings(spec: &str) -> String {
    compile(spec).unwrap().artifact.files["stack.json"].clone()
}

fn refusal(spec: &str) -> CompileError {
    compile(spec).expect_err("the document should be refused")
}

#[test]
fn the_plugin_says_what_it_is() {
    assert_eq!(Pulumi.name(), "pulumi");
    assert_eq!(Pulumi.category(), Category::InfrastructureAsCode);
    // No notify and no apply-time conditional is claimed, so the core never
    // promises either on this target; a secret is taken.
    assert_eq!(
        Pulumi.capabilities(),
        Capabilities {
            secrets: true,
            adopt: meridian_sdk::Adoption::Import,
            ..Capabilities::default()
        }
    );
    assert_eq!(
        Pulumi.supported_resource_types(),
        [
            "<package>:<module>:<Type>",
            "config",
            "invoke.<function>",
            "stack"
        ]
    );
}

#[test]
fn the_examples_compile_to_their_golden_programs() {
    for case in ["web-vm", "pets"] {
        let graph = graph_from_file(&format!("examples/pulumi/{case}.yaml"), "pulumi");
        assert_golden(&golden(), case, &emit(&graph).unwrap());
    }
}

#[test]
fn compiling_is_deterministic() {
    let graph = graph_from_file("examples/pulumi/web-vm.yaml", "pulumi");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn a_program_has_its_sections_in_pulumis_order() {
    let text = program(
        "  resources:\n    - id: bucket\n      type: aws:s3:Bucket\n      params: {bucket: logs}\n    - id: zone\n      type: invoke.aws:getAvailabilityZones\n      params: {state: available}\n    - id: size\n      type: config\n      params: {type: string, default: small}\n  outputs:\n    - name: arn\n      value: ${bucket.arn}\n",
    );
    assert_eq!(
        text,
        "name: net\n\
         runtime: yaml\n\
         description: Compiled by Meridian from the document net. Do not edit.\n\
         config:\n\
         \x20 size:\n\
         \x20   default: small\n\
         \x20   type: string\n\
         variables:\n\
         \x20 zone:\n\
         \x20   fn::invoke:\n\
         \x20     function: aws:getAvailabilityZones\n\
         \x20     arguments:\n\
         \x20       state: available\n\
         resources:\n\
         \x20 bucket:\n\
         \x20   type: aws:s3:Bucket\n\
         \x20   properties:\n\
         \x20     bucket: logs\n\
         outputs:\n\
         \x20 arn: ${bucket.arn}\n"
    );
}

#[test]
fn a_reference_orders_without_depends_on() {
    let text = program(
        "  resources:\n    - id: vpc\n      type: aws:ec2:Vpc\n      params: {cidrBlock: 10.0.0.0/16}\n    - id: subnet\n      type: aws:ec2:Subnet\n      params:\n        vpcId: ${vpc.id}\n        cidrBlock: 10.0.1.0/24\n      dependsOn:\n        - resource: vpc\n",
    );
    assert!(text.contains("vpcId: ${vpc.id}\n"), "{text}");
    assert!(!text.contains("dependsOn"), "{text}");
}

#[test]
fn an_order_the_properties_do_not_show_becomes_depends_on() {
    let text = program(
        "  resources:\n    - id: role\n      type: aws:iam:Role\n      params: {name: app}\n    - id: policy\n      type: aws:iam:Policy\n      params: {name: app}\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1, instanceType: t3.micro}\n      dependsOn:\n        - resource: role\n        - resource: policy\n",
    );
    assert!(
        text.contains("    options:\n      dependsOn:\n        - ${policy}\n        - ${role}\n"),
        "{text}"
    );
}

#[test]
fn a_stated_order_joins_the_options_the_author_wrote() {
    let text = program(
        "  resources:\n    - id: role\n      type: aws:iam:Role\n      params: {name: app}\n    - id: key\n      type: aws:kms:Key\n      params: {}\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        ami: ami-1\n        resourceOptions:\n          protect: true\n          dependsOn: [\"${key}\"]\n      dependsOn:\n        - resource: role\n        - resource: key\n",
    );
    // `key` is already there, by the author's hand, and is not repeated.
    assert!(
        text.contains(
            "    options:\n      dependsOn:\n        - ${key}\n        - ${role}\n      protect: true\n"
        ),
        "{text}"
    );
    assert!(!text.contains("resourceOptions"), "{text}");
}

#[test]
fn an_explicit_provider_is_a_resource_and_is_handed_over_whole() {
    let text = program(
        "  resources:\n    - id: west\n      type: pulumi:providers:aws\n      params: {region: us-west-2}\n    - id: bucket\n      type: aws:s3:Bucket\n      params:\n        bucket: logs\n        resourceOptions:\n          provider: ${west}\n",
    );
    assert!(
        text.contains(
            "  west:\n    type: pulumi:providers:aws\n    properties:\n      region: us-west-2\n"
        ),
        "{text}"
    );
    assert!(text.contains("      provider: ${west}\n"), "{text}");
}

#[test]
fn a_function_call_takes_arguments_and_options() {
    let text = program(
        "  resources:\n    - id: west\n      type: pulumi:providers:aws\n      params: {region: us-west-2}\n    - id: ubuntu\n      type: invoke.aws:ec2:getAmi\n      params:\n        mostRecent: true\n        invokeOptions:\n          provider: ${west}\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        ami: ${ubuntu.id}\n",
    );
    assert!(
        text.contains(
            "variables:\n  ubuntu:\n    fn::invoke:\n      function: aws:ec2:getAmi\n      arguments:\n        mostRecent: true\n      options:\n        provider: ${west}\n"
        ),
        "{text}"
    );
    assert!(text.contains("      ami: ${ubuntu.id}\n"), "{text}");
}

#[test]
fn pulumis_functions_pass_through_as_written() {
    let text = program(
        "  resources:\n    - id: db\n      type: aws:rds:Instance\n      params:\n        password:\n          fn::secret: hunter2\n        tags:\n          fn::fromJSON: '{\"a\": 1}'\n",
    );
    assert!(
        text.contains("      password:\n        fn::secret: hunter2\n"),
        "{text}"
    );
    assert!(
        text.contains("        fn::fromJSON: '{\"a\": 1}'\n"),
        "{text}"
    );
}

#[test]
fn text_that_only_looks_like_a_reference_stays_literal() {
    let text = program(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        userData: \"echo $${HOME} for $5\"\n        tags:\n          Name: web-${pulumi.stack}\n",
    );
    assert!(text.contains("userData: echo $${HOME} for $5\n"), "{text}");
    // Pulumi's own names are handed over as written.
    assert!(text.contains("Name: web-${pulumi.stack}\n"), "{text}");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        subnetId: ${subnett.id}\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.target, "pulumi");
    assert_eq!(err.resource, "vm");
    assert_eq!(
        err.msg,
        "param \"subnetId\": ${subnett.id} refers to \"subnett\", which is not a resource in this document"
    );
    assert_eq!(err.pos.file, "infra.yaml");

    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n  outputs:\n    - name: ip\n      value: ${vn.publicIp}\n",
    );
    assert_eq!(
        err.msg,
        "output \"ip\": ${vn.publicIp} refers to \"vn\", which is not a resource in this document"
    );

    // A reference inside an option is checked like any other.
    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        resourceOptions:\n          provider: ${wst}\n",
    );
    assert_eq!(err.rule, "invalid-reference");
}

#[test]
fn an_expression_pulumi_yaml_cannot_evaluate_is_refused() {
    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        ami: ${upper(var.image)}\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(
        err.msg,
        "param \"ami\": ${upper(var.image)} is not a reference. Pulumi YAML reads only a name \
         and its properties inside ${}; anything computed is written as one of its fn:: functions"
    );
}

#[test]
fn a_type_that_is_not_pulumis_is_refused() {
    for t in [
        "package",
        "aws_instance",
        "AWS::EC2::Instance",
        "invoke.getAmi",
    ] {
        let err = refusal(&format!(
            "  resources:\n    - id: thing\n      type: \"{t}\"\n      params: {{name: nginx}}\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{t}");
        assert_eq!(
            err.msg,
            format!(
                "{t:?} is not a Pulumi type. Use a provider's own type token, such as \
                 aws:ec2:Instance, or one of: config, stack, invoke.<function>"
            )
        );
    }
}

#[test]
fn absent_is_refused_with_what_to_do_instead() {
    let err = refusal(
        "  resources:\n    - id: old\n      type: aws:ec2:Instance\n      state: absent\n      params: {ami: ami-1}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert!(
        err.msg.contains("remove the resource from the document"),
        "{}",
        err.msg
    );
}

#[test]
fn a_notify_is_refused_with_what_pulumi_offers_instead() {
    let err = refusal(
        "  resources:\n    - id: cert\n      type: aws:acm:Certificate\n      params: {domainName: example.com}\n      notifies:\n        - resource: lb\n          action: replace\n    - id: lb\n      type: aws:lb:LoadBalancer\n      params: {name: web}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "cert");
    assert!(err.msg.contains("replaceOnChanges"), "{}", err.msg);
}

#[test]
fn the_stack_resource_holds_the_deployments_settings() {
    assert_eq!(
        settings("  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n"),
        "{\n  \"backend\": \"\",\n  \"project\": \"net\",\n  \"secretsProvider\": \"\",\n  \"stack\": \"dev\"\n}\n"
    );

    let spec = "  resources:\n    - id: deployment\n      type: stack\n      params:\n        name: acme/prod\n        backend: s3://acme-state\n        secretsProvider: awskms://alias/pulumi\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n";
    assert_eq!(
        settings(spec),
        "{\n  \"backend\": \"s3://acme-state\",\n  \"project\": \"net\",\n  \"secretsProvider\": \"awskms://alias/pulumi\",\n  \"stack\": \"acme/prod\"\n}\n"
    );
    let text = program(spec);
    assert!(
        text.contains("backend:\n  url: s3://acme-state\n"),
        "{text}"
    );
    // The stack is not part of the program.
    assert!(!text.contains("deployment"), "{text}");
}

#[test]
fn the_stacks_settings_are_checked() {
    let stack = |params: &str| {
        refusal(&format!(
            "  resources:\n    - id: deployment\n      type: stack\n      params: {params}\n"
        ))
    };
    let err = stack("{name: prod, region: us-east-1}");
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "a stack has no setting \"region\" (settings: backend, name, secretsProvider)"
    );
    assert_eq!(stack("{name: 7}").rule, "invalid-param");
    assert_eq!(stack("{name: prod eu}").rule, "invalid-name");
    assert!(stack("{name: \"${vm.id}\"}")
        .msg
        .contains("must be plain text"));

    let err = refusal(
        "  resources:\n    - id: one\n      type: stack\n      params: {name: a}\n    - id: two\n      type: stack\n      params: {name: b}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "two");

    let err = refusal(
        "  resources:\n    - id: deployment\n      type: stack\n      params: {name: prod}\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        tags: {Stack: \"${deployment.name}\"}\n",
    );
    assert!(err.msg.contains("${pulumi.stack}"), "{}", err.msg);
}

#[test]
fn a_configuration_value_is_declared_and_read_by_name() {
    let text = program(
        "  resources:\n    - id: size\n      type: config\n      params: {name: instanceType, type: string, default: t3.small}\n    - id: token\n      type: config\n      params: {type: string, secret: true}\n    - id: region\n      type: config\n      params: {name: \"aws:region\", value: us-east-1}\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        instanceType: ${size}\n        userData: ${token}\n",
    );
    assert!(
        text.contains(
            "config:\n  instanceType:\n    default: t3.small\n    type: string\n  token:\n    secret: true\n    type: string\n  aws:region:\n    value: us-east-1\n"
        ),
        "{text}"
    );
    assert!(text.contains("instanceType: ${instanceType}\n"), "{text}");
    assert!(text.contains("userData: ${token}\n"), "{text}");
}

#[test]
fn a_configuration_values_declaration_is_checked() {
    let config = |params: &str| {
        refusal(&format!(
            "  resources:\n    - id: size\n      type: config\n      params: {params}\n    - id: vm\n      type: aws:ec2:Instance\n      params: {{ami: ami-1}}\n"
        ))
    };
    let err = config("{}");
    assert_eq!(err.rule, "missing-param");
    let err = config("{type: string, defualt: x}");
    assert_eq!(err.rule, "invalid-param");
    assert!(err.msg.contains("\"defualt\""), "{}", err.msg);
    let err = config("{name: \"aws:region\", type: string}");
    assert!(err.msg.contains("takes only \"value\""), "{}", err.msg);
    let err = config("{type: string, default: \"${vm.id}\"}");
    assert_eq!(err.rule, "invalid-reference");

    // A provider's setting is the provider's to read.
    let err = refusal(
        "  resources:\n    - id: region\n      type: config\n      params: {name: \"aws:region\", value: us-east-1}\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        availabilityZone: ${region}a\n",
    );
    assert!(
        err.msg.contains("only the provider reads it"),
        "{}",
        err.msg
    );
}

#[test]
fn names_that_pulumi_would_not_accept_are_refused() {
    let err = refusal(
        "  resources:\n    - id: size\n      type: config\n      params: {name: vm, type: string}\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "vm");

    let err = refusal(
        "  resources:\n    - id: pulumi\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n",
    );
    assert_eq!(err.rule, "invalid-name");

    let err = refusal(
        "  resources:\n    - id: size\n      type: config\n      params: {name: instance type, type: string}\n",
    );
    assert_eq!(err.rule, "invalid-name");
    assert!(err.msg.starts_with("name \"instance type\""), "{}", err.msg);
}

#[test]
fn only_resources_can_be_ordered_by_hand() {
    let err = refusal(
        "  resources:\n    - id: size\n      type: config\n      params: {type: string, default: small}\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n      dependsOn:\n        - resource: size\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");
    assert!(
        err.msg.starts_with("\"size\" is a configuration value"),
        "{}",
        err.msg
    );

    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n    - id: zones\n      type: invoke.aws:getAvailabilityZones\n      params: {}\n      dependsOn:\n        - resource: vm\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");
    assert!(
        err.msg.starts_with("\"zones\" is a function call"),
        "{}",
        err.msg
    );

    let err = refusal(
        "  resources:\n    - id: deployment\n      type: stack\n      params: {name: prod}\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n      dependsOn:\n        - resource: deployment\n",
    );
    assert_eq!(err.rule, "unsupported-dependency");

    // An order a reference already implies is fine for any of them.
    let text = program(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n    - id: eni\n      type: invoke.aws:ec2:getNetworkInterface\n      params:\n        id: ${vm.primaryNetworkInterfaceId}\n      dependsOn:\n        - resource: vm\n",
    );
    assert!(!text.contains("dependsOn"), "{text}");
}

#[test]
fn options_must_be_a_map() {
    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        resourceOptions: protect\n",
    );
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "param \"resourceOptions\" must be a map of Pulumi's options, not a string"
    );

    let err = refusal(
        "  resources:\n    - id: role\n      type: aws:iam:Role\n      params: {}\n    - id: vm\n      type: aws:ec2:Instance\n      params:\n        resourceOptions:\n          dependsOn: ${role}\n      dependsOn:\n        - resource: other\n    - id: other\n      type: aws:iam:Role\n      params: {}\n",
    );
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(err.msg, "the resource option \"dependsOn\" must be a list");
}

// A document name Pulumi cannot use is one the core refuses first (issue 133:
// every name of letters, digits, `.`, `_` and `-` is a Pulumi project name),
// so the plugin's own check is covered by its unit test alone.
#[test]
fn runtime_when_is_refused() {
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: pulumi\n  resources:\n    - id: vm\n      type: aws:ec2:Instance\n      params: {ami: ami-1}\n";
    let mut graph = graph_from_text("infra.yaml", text, "pulumi");
    graph.resources[0].runtime_when = "facts.os == 'linux'".to_owned();
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-runtime-condition");
}

/// The user documentation shows the example and what it compiles to. Both are
/// compared with the real files, so the page cannot drift from the plugin.
#[test]
fn the_documentation_shows_the_real_example_and_its_real_output() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("reading {path}: {e}"))
    };
    let page = read("docs/targets/pulumi.md");

    let compiled = read("plugins/pulumi/testdata/pets.Pulumi_yaml");
    assert!(
        page.contains(&format!("```yaml\n{compiled}```")),
        "docs/targets/pulumi.md does not show what pets.yaml compiles to"
    );

    // The example on the page is the example file without its comments.
    let example: String = read("examples/pulumi/pets.yaml")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(
        page.contains(&format!("```yaml\n{example}```")),
        "docs/targets/pulumi.md does not show examples/pulumi/pets.yaml as it is"
    );
}

// ---- secrets ------------------------------------------------------------

#[test]
fn a_secret_in_a_param_is_a_secret_configuration_value_the_runner_sets_from_stdin() {
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: pets\nspec:\n  target: pulumi\n  resources:\n    - id: pet\n      type: random:index:RandomPet\n      params:\n        prefix: placeholder\n        length: 2\n";
    let mut graph = graph_from_text("pets.yaml", text, "pulumi");
    secret_in_param(&mut graph, "pet", "prefix", "db_password");
    let emitted = emit(&graph).unwrap();
    let program = &emitted.artifact.files["Pulumi.yaml"];
    assert!(
        program.contains(
            "config:\n  meridian_secret_db_password:\n    secret: true\n    type: string\n"
        ),
        "{program}"
    );
    assert!(
        program.contains("prefix: ${meridian_secret_db_password}"),
        "{program}"
    );
    assert!(!program.contains("(secret "), "{program}");
    assert!(!program.contains("dependsOn"), "{program}");
    assert_eq!(
        meridian_target_pulumi::secret_key("db_password"),
        "meridian_secret_db_password"
    );
    assert_eq!(
        meridian_target_pulumi::secret_reference("db_password"),
        "${meridian_secret_db_password}"
    );
    assert!(Pulumi.capabilities().secrets);
}

#[test]
fn a_secret_in_the_stacks_settings_or_a_configuration_declaration_is_refused() {
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: pets\nspec:\n  target: pulumi\n  resources:\n    - id: deployment\n      type: stack\n      params:\n        name: prod\n    - id: prefix\n      type: config\n      params:\n        type: string\n        default: web\n    - id: pet\n      type: random:index:RandomPet\n      params:\n        prefix: ${prefix}\n";
    for (id, param) in [("deployment", "name"), ("prefix", "default")] {
        let mut graph = graph_from_text("pets.yaml", text, "pulumi");
        secret_in_param(&mut graph, id, param, "db_password");
        let err = emit(&graph).unwrap_err();
        assert_eq!(err.rule, "unsupported-secret-place", "{id}");
        assert!(
            err.msg.starts_with(&format!("param {param:?} of the")),
            "{}",
            err.msg
        );
    }
}

#[test]
fn adopt_becomes_the_import_option() {
    let text = program(
        "  resources:\n    - id: logs\n      type: aws:s3/bucket:Bucket\n      adopt: example-logs\n      params:\n        bucket: example-logs\n        resourceOptions: {protect: true}\n",
    );
    assert!(
        text.contains("    options:\n      import: example-logs\n      protect: true\n"),
        "{text}"
    );
}

#[test]
fn adopt_that_contradicts_the_import_option_is_refused() {
    let e = refusal(
        "  resources:\n    - id: logs\n      type: aws:s3/bucket:Bucket\n      adopt: example-logs\n      params:\n        resourceOptions: {import: other-logs}\n",
    );
    assert_eq!(e.rule, "invalid-adopt", "{e}");
    // Saying the same thing twice is not a contradiction.
    program(
        "  resources:\n    - id: logs\n      type: aws:s3/bucket:Bucket\n      adopt: example-logs\n      params:\n        resourceOptions: {import: example-logs}\n",
    );
}

#[test]
fn adopt_on_a_configuration_value_or_a_provider_is_refused() {
    for (r#type, params) in [
        ("config", "{type: string}"),
        ("pulumi:providers:aws", "{region: us-east-1}"),
    ] {
        let e = refusal(&format!(
            "  resources:\n    - id: x\n      type: {type}\n      adopt: whatever\n      params: {params}\n"
        ));
        assert_eq!(e.rule, "invalid-adopt", "{e}");
    }
}
