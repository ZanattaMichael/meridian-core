//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{
    assert_golden, assert_golden_refusal, graph_from_file, graph_from_text, secret_in_param,
};
use meridian_sdk::{
    decode_value, Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph,
    Value,
};
use meridian_target_opentofu::{OpenTofu, CONFIG_PATH, NAME};

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    OpenTofu.emit(graph, &Data::new())
}

/// An infrastructure document with the given `spec` body, compiled.
fn compile(spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: opentofu\n{spec}"
    );
    emit(&graph_from_text("infra.yaml", &text, NAME))
}

fn config(spec: &str) -> String {
    compile(spec).unwrap().artifact.files[CONFIG_PATH].clone()
}

/// The compiled configuration, read back the way OpenTofu would read it.
fn parsed(spec: &str) -> Value {
    decode_value(config(spec).as_bytes()).expect("the configuration is JSON")
}

/// The value at a dotted path of a configuration.
fn at<'a>(value: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(value, |v, key| {
        v.as_map()
            .and_then(|m| m.get(key))
            .unwrap_or_else(|| panic!("no {key:?} on the way to {path:?} in {value:?}"))
    })
}

fn text(value: &Value, path: &str) -> String {
    at(value, path)
        .as_str()
        .unwrap_or_else(|| panic!("{path} is not a string"))
        .to_owned()
}

fn refusal(spec: &str) -> CompileError {
    compile(spec).expect_err("the document should be refused")
}

/// The encryption block every encryption test starts from.
const ENCRYPTION: &str = "    - id: passphrase\n      type: variable\n      params: {type: string, sensitive: true}\n    - id: sealed\n      type: encryption\n      params:\n        key_provider:\n          pbkdf2:\n            main:\n              passphrase: ${passphrase}\n        method:\n          aes_gcm:\n            main:\n              keys: ${key_provider.pbkdf2.main}\n";

// ---- what the plugin says about itself ---------------------------------

#[test]
fn the_plugin_describes_itself_as_infrastructure_as_code() {
    assert_eq!(OpenTofu.name(), "opentofu");
    assert_eq!(OpenTofu.category(), Category::InfrastructureAsCode);
    assert_eq!(
        OpenTofu.capabilities(),
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
            secrets: true,
            credentials: false,
            cloud_credentials: true,
            adopt: meridian_sdk::Adoption::Import,
        }
    );
    assert_eq!(
        OpenTofu.supported_resource_types(),
        [
            "<provider>_<type>",
            "backend",
            "data.<type>",
            "encryption",
            "ephemeral.<type>",
            "local",
            "module",
            "provider",
            "variable",
        ]
    );
}

// ---- golden files -------------------------------------------------------

#[test]
fn the_examples_compile_to_their_golden_configurations() {
    for case in ["web-vm", "encrypted-state"] {
        let graph = graph_from_file(&format!("examples/opentofu/{case}.yaml"), NAME);
        assert_golden(&golden(), case, &emit(&graph).unwrap());
    }
}

#[test]
fn a_document_using_every_kind_of_block_compiles_to_its_golden_configuration() {
    let graph = graph_from_file("plugins/opentofu/tests/documents/every-block.yaml", NAME);
    assert_golden(&golden(), "every-block", &emit(&graph).unwrap());
}

#[test]
fn a_notify_compiles_to_its_golden_configuration_and_warning() {
    let graph = graph_from_file("plugins/opentofu/tests/documents/notify.yaml", NAME);
    assert_golden(&golden(), "notify", &emit(&graph).unwrap());
}

#[test]
fn refusals_read_the_way_their_golden_files_record() {
    for case in ["refused-encryption", "refused-reference"] {
        let graph = graph_from_file(
            &format!("plugins/opentofu/tests/documents/{case}.yaml"),
            NAME,
        );
        assert_golden_refusal(&golden(), case, &emit(&graph).unwrap_err());
    }
}

#[test]
fn compiling_is_deterministic() {
    for path in [
        "examples/opentofu/web-vm.yaml",
        "examples/opentofu/encrypted-state.yaml",
        "plugins/opentofu/tests/documents/every-block.yaml",
    ] {
        let graph = graph_from_file(path, NAME);
        assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap(), "{path}");
    }
}

#[test]
fn the_configuration_is_one_file_only_opentofu_reads() {
    let emitted = compile(
        "  resources:\n    - id: note\n      type: terraform_data\n      params: {input: hello}\n",
    )
    .unwrap();
    assert_eq!(
        emitted.artifact.files.keys().collect::<Vec<_>>(),
        ["main.tofu.json"]
    );
    assert!(emitted.warnings.is_empty());
    assert!(emitted.artifact.files[CONFIG_PATH].ends_with("}\n"));
}

// ---- references ---------------------------------------------------------

#[test]
fn a_reference_becomes_opentofus_address_and_orders_without_depends_on() {
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
fn every_kind_of_block_is_referred_to_by_its_own_address() {
    let config = parsed(
        "  resources:\n    - id: zone\n      type: variable\n      params: {type: string}\n    - id: prefix\n      type: local\n      params: {value: web}\n    - id: image\n      type: data.aws_ami\n      params: {most_recent: true}\n    - id: token\n      type: ephemeral.vault_token\n      params: {ttl: 60}\n    - id: network\n      type: module\n      params: {source: ./network}\n    - id: disk\n      type: aws_ebs_volume\n      params: {size: 8}\n    - id: vm\n      type: aws_instance\n      params:\n        availability_zone: ${zone}\n        ami: ${image.id}\n        subnet_id: ${network.subnet_ids[0]}\n        user_data: ${token.value}\n        tags:\n          Name: ${prefix}-${disk.id}\n",
    );
    let vm = at(&config, "resource.aws_instance.vm");
    assert_eq!(text(vm, "availability_zone"), "${var.zone}");
    assert_eq!(text(vm, "ami"), "${data.aws_ami.image.id}");
    assert_eq!(text(vm, "subnet_id"), "${module.network.subnet_ids[0]}");
    assert_eq!(
        text(vm, "user_data"),
        "${ephemeral.vault_token.token.value}"
    );
    assert_eq!(
        text(vm, "tags.Name"),
        "${local.prefix}-${aws_ebs_volume.disk.id}"
    );
}

#[test]
fn the_documents_names_are_found_inside_a_larger_expression() {
    let config = parsed(
        "  resources:\n    - id: big\n      type: variable\n      params: {type: bool, default: false}\n    - id: image\n      type: data.aws_ami\n      params: {most_recent: true}\n    - id: vm\n      type: aws_instance\n      params:\n        ami: ${upper(image.id)}\n        instance_type: '${big ? \"t3.large\" : \"t3.micro\"}'\n        tags: ${merge({image = image.name}, var.extra_tags)}\n      dependsOn:\n        - resource: image\n",
    );
    let vm = at(&config, "resource.aws_instance.vm");
    assert_eq!(text(vm, "ami"), "${upper(data.aws_ami.image.id)}");
    assert_eq!(
        text(vm, "instance_type"),
        "${var.big ? \"t3.large\" : \"t3.micro\"}"
    );
    // The key stays a key; the value becomes the address.
    assert_eq!(
        text(vm, "tags"),
        "${merge({image = data.aws_ami.image.name}, var.extra_tags)}"
    );
    // A name inside a call orders the two just as a plain reference does.
    assert!(vm.as_map().unwrap().get("depends_on").is_none());
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
    // A function call is OpenTofu's own language and is handed over as written.
    assert!(text.contains("\"ami\": \"${upper(var.image)}\""), "{text}");
}

#[test]
fn opentofus_own_namespaces_are_handed_over_untouched() {
    let config = parsed(
        "  resources:\n    - id: note\n      type: terraform_data\n      params:\n        input:\n          workspace: ${terraform.workspace}\n          also: ${tofu.workspace}\n          root: ${path.root}\n          each: ${each.key}\n          count: ${count.index}\n          self: ${self.id}\n          other: ${module.net.id}\n",
    );
    let input = at(&config, "resource.terraform_data.note.input");
    assert_eq!(text(input, "workspace"), "${terraform.workspace}");
    assert_eq!(text(input, "also"), "${tofu.workspace}");
    assert_eq!(text(input, "root"), "${path.root}");
    assert_eq!(text(input, "each"), "${each.key}");
    assert_eq!(text(input, "count"), "${count.index}");
    assert_eq!(text(input, "self"), "${self.id}");
    assert_eq!(text(input, "other"), "${module.net.id}");
}

#[test]
fn a_reference_to_nothing_is_refused_at_compile_time() {
    let err = refusal(
        "  resources:\n    - id: vm\n      type: aws_instance\n      params:\n        subnet_id: ${subnett.id}\n",
    );
    assert_eq!(err.target, "opentofu");
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
fn settings_have_no_attributes_to_read() {
    for (resource, what) in [
        (
            "    - id: aws\n      type: provider\n      params: {region: us-east-1}\n",
            "a provider block",
        ),
        (
            "    - id: s3\n      type: backend\n      params: {bucket: state}\n",
            "a backend block",
        ),
    ] {
        let id = if what.contains("provider") {
            "aws"
        } else {
            "s3"
        };
        let err = refusal(&format!(
            "  resources:\n{resource}    - id: vm\n      type: aws_instance\n      params:\n        ami: ${{{id}.region}}\n"
        ));
        assert_eq!(err.rule, "invalid-reference");
        assert_eq!(
            err.msg,
            format!(
                "param \"ami\": ${{{id}.region}} refers to {id:?}, which is {what} and has no attributes to read"
            )
        );
    }

    let err = refusal(&format!(
        "  resources:\n{ENCRYPTION}        state:\n          method: ${{method.aes_gcm.main}}\n    - id: note\n      type: terraform_data\n      params:\n        input: ${{sealed.state}}\n"
    ));
    assert_eq!(
        err.msg,
        "param \"input\": ${sealed.state} refers to \"sealed\", which is the encryption block and has no attributes to read"
    );
}

#[test]
fn an_id_opentofu_reserves_is_refused() {
    for id in [
        "var",
        "local",
        "data",
        "module",
        "tofu",
        "method",
        "ephemeral",
    ] {
        let err = refusal(&format!(
            "  resources:\n    - id: {id}\n      type: terraform_data\n      params: {{input: x}}\n"
        ));
        assert_eq!(err.rule, "reserved-name", "{id}");
        assert_eq!(err.resource, id);
    }
}

// ---- types, states and names --------------------------------------------

#[test]
fn a_type_that_is_not_opentofus_is_refused() {
    let err = refusal(
        "  resources:\n    - id: nginx\n      type: package\n      params: {name: nginx}\n",
    );
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(
        err.msg,
        "\"package\" is not an OpenTofu resource type. Use a provider's own type, such as \
         aws_instance, or one of: provider, backend, encryption, module, variable, local, \
         data.<type>, ephemeral.<type>"
    );

    for bad in ["data.ami", "ephemeral.token", "data.", "ephemeral.Aws_x"] {
        let err = refusal(&format!(
            "  resources:\n    - id: x\n      type: {bad}\n      params: {{a: 1}}\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{bad}");
    }
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
fn a_name_opentofu_would_not_accept_is_refused() {
    let err = refusal(
        "  resources:\n    - id: 9lives\n      type: terraform_data\n      params: {input: x}\n",
    );
    assert_eq!(err.rule, "invalid-name");
    assert!(
        err.msg.contains("cannot be an OpenTofu name"),
        "{}",
        err.msg
    );

    let err = refusal(
        "  resources:\n    - id: vm\n      type: terraform_data\n      params: {input: x}\n  outputs:\n    - name: my.ip\n      value: ${vm.output}\n",
    );
    assert_eq!(err.rule, "invalid-name");
    assert_eq!(err.msg, "output name \"my.ip\" cannot be an OpenTofu name");
}

// ---- ordering and notify --------------------------------------------------

#[test]
fn a_notify_becomes_a_forced_replacement_and_says_so() {
    let emitted = compile(
        "  resources:\n    - id: cert\n      type: aws_acm_certificate\n      params: {domain_name: example.com}\n      notifies:\n        - resource: lb\n          action: replace\n    - id: lb\n      type: aws_lb\n      params: {name: web}\n",
    )
    .unwrap();
    let text = &emitted.artifact.files[CONFIG_PATH];
    assert!(
        text.contains(
            "\"replace_triggered_by\": [\n            \"aws_acm_certificate.cert\"\n          ]"
        ),
        "{text}"
    );
    assert_eq!(emitted.warnings.len(), 1);
    assert_eq!(
        emitted.warnings[0].to_string(),
        "opentofu: lb: is destroyed and created again whenever \"cert\" changes; OpenTofu has \
         no gentler way to react to a change (replace_triggered_by)"
    );

    let err = refusal(
        "  resources:\n    - id: cert\n      type: aws_acm_certificate\n      params: {domain_name: example.com}\n      notifies:\n        - resource: lb\n          action: restart\n    - id: lb\n      type: aws_lb\n      params: {name: web}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "cert");
}

#[test]
fn a_notify_keeps_the_lifecycle_the_author_wrote() {
    let config = parsed(
        "  resources:\n    - id: cert\n      type: aws_acm_certificate\n      params: {domain_name: example.com}\n      notifies:\n        - resource: lb\n          action: replace\n    - id: lb\n      type: aws_lb\n      params:\n        name: web\n        lifecycle:\n          create_before_destroy: true\n",
    );
    let lifecycle = at(&config, "resource.aws_lb.lb.lifecycle");
    assert_eq!(at(lifecycle, "create_before_destroy"), &Value::Bool(true));
    assert_eq!(
        at(lifecycle, "replace_triggered_by"),
        &Value::List(vec![Value::String("aws_acm_certificate.cert".into())])
    );
}

#[test]
fn only_what_opentofu_orders_can_be_depended_on() {
    for (declaration, noun) in [
        (
            "    - id: aws\n      type: provider\n      params: {region: us-east-1}\n",
            "a provider block",
        ),
        (
            "    - id: zone\n      type: variable\n      params: {type: string}\n",
            "a variable",
        ),
        (
            "    - id: prefix\n      type: local\n      params: {value: web}\n",
            "a local value",
        ),
    ] {
        let id = declaration.split_whitespace().nth(2).unwrap();
        let err = refusal(&format!(
            "  resources:\n{declaration}    - id: vm\n      type: aws_instance\n      params: {{ami: ami-1}}\n      dependsOn:\n        - resource: {id}\n"
        ));
        assert_eq!(err.rule, "unsupported-dependency", "{noun}");
        assert_eq!(
            err.msg,
            format!(
                "{id:?} is {noun}, which OpenTofu works out before or alongside \
                 everything else; nothing can depend on it or be ordered by it"
            )
        );
    }

    // A data source, an ephemeral resource and a module can all be waited for.
    let config = parsed(
        "  resources:\n    - id: image\n      type: data.aws_ami\n      params: {most_recent: true}\n    - id: token\n      type: ephemeral.vault_token\n      params: {ttl: 60}\n    - id: network\n      type: module\n      params: {source: ./network}\n    - id: vm\n      type: aws_instance\n      params: {instance_type: t3.micro}\n      dependsOn:\n        - resource: image\n        - resource: token\n        - resource: network\n",
    );
    assert_eq!(
        at(&config, "resource.aws_instance.vm.depends_on"),
        &Value::List(
            [
                "data.aws_ami.image",
                "module.network",
                "ephemeral.vault_token.token"
            ]
            .map(|a| Value::String(a.into()))
            .to_vec()
        )
    );
}

// ---- providers, backends, modules, variables and locals -------------------

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
fn a_providers_source_and_version_go_to_required_providers() {
    let config = parsed(
        "  resources:\n    - id: aws\n      type: provider\n      params:\n        source: hashicorp/aws\n        version: \"~> 5.0\"\n        region: us-east-1\n",
    );
    let required = at(&config, "terraform.required_providers.aws");
    assert_eq!(text(required, "source"), "hashicorp/aws");
    assert_eq!(text(required, "version"), "~> 5.0");
    let provider = at(&config, "provider.aws").as_map().unwrap();
    assert_eq!(provider.keys().collect::<Vec<_>>(), ["region"]);
}

#[test]
fn a_backend_and_a_module_are_written_where_opentofu_expects_them() {
    let config = parsed(
        "  resources:\n    - id: s3\n      type: backend\n      params: {bucket: tfstate, key: net.tfstate, region: us-east-1}\n    - id: network\n      type: module\n      params:\n        source: terraform-aws-modules/vpc/aws\n        version: 5.1.0\n        cidr: 10.0.0.0/16\n    - id: vm\n      type: aws_instance\n      params:\n        subnet_id: ${network.public_subnets[0]}\n",
    );
    assert_eq!(text(&config, "terraform.backend.s3.bucket"), "tfstate");
    assert_eq!(text(&config, "module.network.version"), "5.1.0");
    assert_eq!(
        text(&config, "resource.aws_instance.vm.subnet_id"),
        "${module.network.public_subnets[0]}"
    );

    let err = refusal(
        "  resources:\n    - id: network\n      type: module\n      params: {cidr: 10.0.0.0/16}\n",
    );
    assert_eq!(err.rule, "missing-param");

    let err = refusal(
        "  resources:\n    - id: s3\n      type: backend\n      params: {bucket: a}\n    - id: gcs\n      type: backend\n      params: {bucket: b}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
}

#[test]
fn variables_and_locals_become_their_blocks() {
    let config = parsed(
        "  resources:\n    - id: zones\n      type: variable\n      params:\n        type: list(string)\n        default: [a, b]\n        description: Where to run\n    - id: first_zone\n      type: local\n      params:\n        value: ${zones[0]}\n    - id: tags\n      type: local\n      params:\n        value:\n          zone: ${first_zone}\n          team: web\n",
    );
    assert_eq!(text(&config, "variable.zones.type"), "list(string)");
    assert_eq!(text(&config, "variable.zones.description"), "Where to run");
    assert_eq!(text(&config, "locals.first_zone"), "${var.zones[0]}");
    assert_eq!(text(&config, "locals.tags.zone"), "${local.first_zone}");
    assert_eq!(text(&config, "locals.tags.team"), "web");
}

#[test]
fn a_local_has_a_value_and_nothing_else() {
    let err = refusal("  resources:\n    - id: prefix\n      type: local\n      params: {}\n");
    assert_eq!(err.rule, "missing-param");
    assert_eq!(
        err.msg,
        "resource type \"local\" requires param \"value\" on this target"
    );

    let err = refusal(
        "  resources:\n    - id: prefix\n      type: local\n      params: {value: web, description: The prefix}\n",
    );
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "a local value has one param, \"value\"; \"description\" has nowhere to go"
    );
}

// ---- outputs ---------------------------------------------------------------

#[test]
fn an_output_that_reads_a_sensitive_variable_says_so() {
    let config = parsed(
        "  resources:\n    - id: db_password\n      type: variable\n      params: {type: string, sensitive: true}\n    - id: db_name\n      type: variable\n      params: {type: string, default: app}\n    - id: db\n      type: aws_db_instance\n      params:\n        password: ${db_password}\n        db_name: ${db_name}\n  outputs:\n    - name: endpoint\n      value: ${db.endpoint}\n    - name: name\n      value: ${db_name}\n    - name: url\n      value: postgres://app:${db_password}@${db.endpoint}/${db_name}\n    - name: fingerprint\n      value: ${sha256(db_password)}\n",
    );
    let outputs = at(&config, "output");
    for (name, sensitive) in [
        ("endpoint", false),
        ("name", false),
        ("url", true),
        ("fingerprint", true),
    ] {
        let output = at(outputs, name).as_map().unwrap();
        assert_eq!(
            output.get("sensitive") == Some(&Value::Bool(true)),
            sensitive,
            "{name}"
        );
    }
    assert_eq!(
        text(outputs, "url.value"),
        "postgres://app:${var.db_password}@${aws_db_instance.db.endpoint}/${var.db_name}"
    );
    assert_eq!(
        text(outputs, "fingerprint.value"),
        "${sha256(var.db_password)}"
    );
}

// ---- encryption --------------------------------------------------------------

#[test]
fn encryption_is_written_inside_the_settings_block() {
    let config = parsed(&format!(
        "  resources:\n{ENCRYPTION}        state:\n          method: ${{method.aes_gcm.main}}\n          enforced: true\n        plan:\n          method: ${{method.aes_gcm.main}}\n"
    ));
    let encryption = at(&config, "terraform.encryption");
    assert_eq!(
        text(encryption, "key_provider.pbkdf2.main.passphrase"),
        "${var.passphrase}"
    );
    assert_eq!(
        text(encryption, "method.aes_gcm.main.keys"),
        "${key_provider.pbkdf2.main}"
    );
    // A method is a fixed name to OpenTofu, which refuses a template there.
    assert_eq!(text(encryption, "state.method"), "method.aes_gcm.main");
    assert_eq!(at(encryption, "state.enforced"), &Value::Bool(true));
    assert_eq!(text(encryption, "plan.method"), "method.aes_gcm.main");
}

#[test]
fn an_encryption_block_is_checked_before_opentofu_ever_runs() {
    let err = refusal(&format!(
        "  resources:\n{ENCRYPTION}        state:\n          method: ${{method.aes_gcm.primary}}\n"
    ));
    assert_eq!(err.target, "opentofu");
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "sealed");
    assert_eq!(err.pos.file, "infra.yaml");
    assert!(err.pos.line > 0);

    let err = refusal(&format!("  resources:\n{ENCRYPTION}"));
    assert_eq!(err.rule, "missing-param");
}

#[test]
fn encryptions_warnings_reach_the_person_compiling() {
    let emitted = compile(
        "  resources:\n    - id: sealed\n      type: encryption\n      params:\n        key_provider:\n          pbkdf2:\n            main:\n              passphrase: correct-horse-battery-staple\n        method:\n          aes_gcm:\n            main:\n              keys: ${key_provider.pbkdf2.main}\n        state:\n          method: ${method.aes_gcm.main}\n",
    )
    .unwrap();
    let warnings: Vec<String> = emitted.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].starts_with(
            "opentofu: sealed: key provider \"pbkdf2.main\" has its passphrase written in the document"
        ),
        "{}",
        warnings[0]
    );
    assert!(
        warnings[1].starts_with("opentofu: sealed: the state is encrypted and the plan is not"),
        "{}",
        warnings[1]
    );
}

#[test]
fn a_configuration_has_one_encryption_block() {
    let err = refusal(&format!(
        "  resources:\n{ENCRYPTION}        state:\n          method: ${{method.aes_gcm.main}}\n    - id: again\n      type: encryption\n      params:\n        state:\n          method: ${{method.aes_gcm.main}}\n"
    ));
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "again");
}

#[test]
fn nothing_can_be_ordered_by_the_encryption_block() {
    let err = refusal(&format!(
        "  resources:\n{ENCRYPTION}        state:\n          method: ${{method.aes_gcm.main}}\n        plan:\n          method: ${{method.aes_gcm.main}}\n    - id: note\n      type: terraform_data\n      params: {{input: x}}\n      dependsOn:\n        - resource: sealed\n"
    ));
    assert_eq!(err.rule, "unsupported-dependency");
    assert_eq!(
        err.msg,
        "\"sealed\" is the encryption block, which OpenTofu works out before or alongside \
         everything else; nothing can depend on it or be ordered by it"
    );
}

#[test]
fn a_runtime_condition_is_refused() {
    let graph = {
        let mut graph = graph_from_text(
            "infra.yaml",
            "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: opentofu\n  resources:\n    - id: note\n      type: terraform_data\n      params: {input: x}\n",
            NAME,
        );
        graph.resources[0].runtime_when = "facts.os == 'linux'".to_owned();
        graph
    };
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-runtime-condition");
    assert!(err.msg.contains("no apply-time conditional"), "{}", err.msg);
}

#[test]
fn every_golden_file_is_still_asserted_by_a_test() {
    // A golden file no test reads is a file nobody would notice going stale.
    let mut names: Vec<String> = std::fs::read_dir(golden())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "encrypted-state.main_tofu_json",
            "every-block.main_tofu_json",
            "notify.main_tofu_json",
            "notify.warnings",
            "refused-encryption.refused",
            "refused-reference.refused",
            "web-vm.main_tofu_json",
        ]
    );
}

#[test]
fn a_secret_in_a_param_is_a_sensitive_variable_the_runner_hands_over() {
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: db\nspec:\n  target: opentofu\n  resources:\n    - id: main\n      type: aws_db_instance\n      params:\n        engine: postgres\n        password: placeholder\n";
    let mut graph = graph_from_text("infra.yaml", text, "opentofu");
    secret_in_param(&mut graph, "main", "password", "db_password");
    let config = emit(&graph).unwrap().artifact.files["main.tofu.json"].clone();
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

#[test]
fn a_cloud_credential_leaves_nothing_in_the_configuration_and_a_role_is_refused() {
    let spec = |fields: &str| {
        format!(
            "  secrets:\n    - name: cloud_secret\n      from: env\n      key: CLOUD_SECRET\n  \
             credentials:\n    - {{name: prod, type: aws, {fields}}}\n  \
             endpoint: {{type: cloud, credential: prod}}\n  \
             resources:\n    - id: logs\n      type: aws_s3_bucket\n      params: {{bucket: logs}}\n"
        )
    };
    let compiled = compile(&spec(
        "accessKeyId: AKIA1, secretAccessKey: \"${{ secret.cloud_secret }}\", region: eu-west-1",
    ))
    .unwrap();
    let text: String = compiled.artifact.files.values().cloned().collect();
    for part in ["AKIA1", "eu-west-1", "cloud_secret", "prod"] {
        assert!(
            !text.contains(part),
            "{part} is in the configuration:\n{text}"
        );
    }
    let err = compile(&spec("roleArn: \"arn:aws:iam::1:role/x\"")).unwrap_err();
    assert_eq!(err.rule, "unsupported-credential");
    assert!(
        err.msg
            .starts_with("the credential \"prod\": tofu takes no roleArn from Meridian"),
        "{}",
        err.msg
    );
}

#[test]
fn adopt_becomes_an_import_block_for_a_managed_resource() {
    let config = parsed(
        "  resources:\n    - id: logs\n      type: aws_s3_bucket\n      adopt: example-logs\n      params: {bucket: example-logs}\n",
    );
    let imports = config.as_map().unwrap()["import"].as_list().unwrap();
    assert_eq!(imports.len(), 1);
    assert_eq!(at(&imports[0], "to"), &Value::from("aws_s3_bucket.logs"));
    assert_eq!(at(&imports[0], "id"), &Value::from("example-logs"));
}

#[test]
fn adopt_on_anything_but_a_managed_resource_is_refused() {
    let e = compile(
        "  resources:\n    - id: ubuntu\n      type: data.aws_ami\n      adopt: whatever\n      params: {owners: [self]}\n",
    )
    .expect_err("a data source cannot be adopted");
    assert_eq!(e.rule, "invalid-adopt", "{e}");
}
