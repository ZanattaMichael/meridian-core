//! The server, spoken to the way a client speaks to it: JSON-RPC messages,
//! one per line, with real plugins loaded in process.

use std::io::Cursor;

use meridian_mcp::{serve, Server};
use meridian_plugin_host::Registry;
use meridian_tooling::catalog::Catalog;
use serde_json::{json, Value};

const ANSIBLE_DOC: &str = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx.conf
        content: x
        mode: \"0644\"
      dependsOn:
        - resource: nginx_pkg
    - id: nginx_svc
      type: service
      state: rebooted
      params:
        name: nginx
        colour: blue
";

fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Box::new(meridian_target_ansible::Ansible))
        .unwrap();
    registry
        .register_with_importer(
            Box::new(meridian_target_terraform::Terraform),
            Some(Box::new(meridian_target_terraform::TerraformImporter)),
        )
        .unwrap();
    registry
        .register(Box::new(meridian_target_fake::Fake))
        .unwrap();
    registry
}

fn server() -> Server {
    let registry = registry();
    let catalog = Catalog::from_registry(&registry);
    Server::new(registry, catalog, env!("CARGO_MANIFEST_DIR"))
}

/// Sends `messages` to a fresh server and returns every reply, in order.
fn talk(server: &mut Server, messages: &[Value]) -> Vec<Value> {
    let input: String = messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap() + "\n")
        .collect();
    let mut output = Vec::new();
    serve(&mut Cursor::new(input), &mut output, server).unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: i64, tool: &str, args: Value) -> Value {
    request(id, "tools/call", json!({ "name": tool, "arguments": args }))
}

fn initialize() -> Value {
    request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
    )
}

/// The structured content of a tool result, and whether it was an error.
fn structured(reply: &Value) -> (Value, bool) {
    let result = &reply["result"];
    assert!(reply.get("error").is_none(), "{reply}");
    (
        result["structuredContent"].clone(),
        result["isError"].as_bool().unwrap(),
    )
}

#[test]
fn initialize_answers_the_protocol_version_asked_for() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            request(
                1,
                "initialize",
                json!({ "protocolVersion": "2024-11-05", "capabilities": {} }),
            ),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            request(2, "ping", json!({})),
        ],
    );
    assert_eq!(
        replies.len(),
        2,
        "a notification gets no reply: {replies:?}"
    );
    assert_eq!(replies[0]["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "meridian");
    assert!(replies[0]["result"]["capabilities"]["tools"].is_object());
    assert_eq!(replies[1]["id"], 2);

    // A version this server does not know gets the newest it does.
    let replies = talk(
        &mut server,
        &[request(
            3,
            "initialize",
            json!({ "protocolVersion": "2099-01-01" }),
        )],
    );
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn unknown_methods_and_bad_lines_are_protocol_errors() {
    let mut server = server();
    let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"nothing/here\"}\n[]\n";
    let mut output = Vec::new();
    serve(&mut Cursor::new(input), &mut output, &mut server).unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["error"]["code"], -32601);
    assert_eq!(replies[1]["id"], 5);
    assert_eq!(replies[2]["error"]["code"], -32600);
}

#[test]
fn every_tool_is_listed_with_a_schema() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[initialize(), request(2, "tools/list", json!({}))],
    );
    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "list_targets",
            "list_resource_types",
            "describe_resource_type",
            "describe_document",
            "get_example",
            "validate",
            "compile",
            "lint",
            "format",
            "explain",
            "import",
            "plan",
            "diagnose",
        ]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{}", tool["name"]);
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
}

#[test]
fn list_targets_reads_the_plugins() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[initialize(), call(2, "list_targets", json!({}))],
    );
    let (body, is_error) = structured(&replies[1]);
    assert!(!is_error);
    let targets = body["targets"].as_array().unwrap();
    let names: Vec<&str> = targets
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ansible", "fake", "terraform"]);
    let ansible = &targets[0];
    assert_eq!(ansible["category"], "configuration-management");
    assert_eq!(ansible["kinds"], json!(["ResourceSet"]));
    assert!(ansible["resourceTypes"].as_u64().unwrap() >= 6);
    let terraform = &targets[2];
    assert_eq!(terraform["kinds"], json!(["Infrastructure"]));
    assert_eq!(terraform["imports"][0]["name"], "hcl");
}

#[test]
fn resource_types_are_paged_and_searched() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(
                2,
                "list_resource_types",
                json!({ "target": "ansible", "pageSize": 2 }),
            ),
            call(
                3,
                "list_resource_types",
                json!({ "target": "ansible", "pageSize": 2, "page": 2 }),
            ),
            call(
                4,
                "list_resource_types",
                json!({ "target": "ansible", "search": "service" }),
            ),
            call(5, "list_resource_types", json!({ "target": "nope" })),
        ],
    );
    let (first, _) = structured(&replies[1]);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["nextPage"], 2);
    let (second, _) = structured(&replies[2]);
    assert_eq!(second["page"], 2);
    assert_ne!(first["items"][0]["type"], second["items"][0]["type"]);
    let (found, _) = structured(&replies[3]);
    assert!(found["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["type"] == "service"));
    let (missing, is_error) = structured(&replies[4]);
    assert!(is_error);
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ansible, fake, terraform"));
}

#[test]
fn a_resource_type_is_described_with_a_valid_example() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(
                2,
                "describe_resource_type",
                json!({ "target": "ansible", "type": "service" }),
            ),
            call(
                3,
                "describe_resource_type",
                json!({ "target": "terraform", "type": "aws_instance" }),
            ),
            call(
                4,
                "describe_resource_type",
                json!({ "target": "ansible", "type": "spaceship" }),
            ),
        ],
    );
    let (service, is_error) = structured(&replies[1]);
    assert!(!is_error);
    assert_eq!(service["type"], "service");
    assert!(service["states"]
        .as_array()
        .unwrap()
        .contains(&json!("running")));
    let params = service["params"].as_array().unwrap();
    let name = params.iter().find(|p| p["name"] == "name").unwrap();
    assert_eq!(name["kind"], "string");
    assert_eq!(name["required"], true);
    let example = service["example"].as_str().unwrap();
    assert!(example.contains("kind: ResourceSet"), "{example}");
    assert!(example.contains("targetHost:"), "{example}");
    assert!(example.contains("type: service"), "{example}");
    assert_eq!(service["exampleValid"], true, "{example}");

    let (instance, is_error) = structured(&replies[2]);
    assert!(!is_error);
    assert_eq!(instance["pattern"], true);
    assert!(instance["example"]
        .as_str()
        .unwrap()
        .contains("kind: Infrastructure"));

    let (missing, is_error) = structured(&replies[3]);
    assert!(is_error);
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("it has: "));
}

#[test]
fn the_document_kinds_come_from_the_language_tables() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(2, "describe_document", json!({})),
            call(3, "describe_document", json!({ "kind": "ResourceSet" })),
            call(4, "describe_document", json!({ "kind": "Spaceship" })),
        ],
    );
    let (kinds, _) = structured(&replies[1]);
    assert_eq!(kinds["kinds"][0]["kind"], "Infrastructure");
    let (rs, _) = structured(&replies[2]);
    let fields = rs["fields"].as_array().unwrap();
    let spec = fields.iter().find(|f| f["name"] == "spec").unwrap();
    assert_eq!(spec["required"], true);
    let spec_fields = spec["fields"].as_array().unwrap();
    assert!(spec_fields.iter().any(|f| f["name"] == "targetHost"));
    let kind = fields.iter().find(|f| f["name"] == "kind").unwrap();
    assert_eq!(kind["type"], "one of");
    assert!(kind["values"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["value"] == "ResourceSet"));
    let (_, is_error) = structured(&replies[3]);
    assert!(is_error);
}

#[test]
fn examples_are_found_by_target_and_name() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(2, "get_example", json!({})),
            call(
                3,
                "get_example",
                json!({ "name": "ansible/web-server.yaml" }),
            ),
            call(
                4,
                "get_example",
                json!({ "target": "kubernetes", "search": "nothing-matches-this" }),
            ),
        ],
    );
    let (all, _) = structured(&replies[1]);
    assert!(all["total"].as_u64().unwrap() > 20);
    let (one, is_error) = structured(&replies[2]);
    assert!(!is_error);
    assert_eq!(one["targets"], json!(["ansible"]));
    assert!(one["text"]
        .as_str()
        .unwrap()
        .contains("apiVersion: meridian/v1"));
    let (none, _) = structured(&replies[3]);
    assert_eq!(none["total"], 0);
}

#[test]
fn diagnostics_carry_the_rule_the_resource_and_what_is_accepted() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(2, "validate", json!({ "document": ANSIBLE_DOC })),
        ],
    );
    let (body, is_error) = structured(&replies[1]);
    assert!(is_error);
    assert_eq!(body["ok"], false);
    assert_eq!(body["documents"][0]["target"], "ansible");
    let diagnostics = body["diagnostics"].as_array().unwrap();
    let state = diagnostics
        .iter()
        .find(|d| d["code"] == "unsupported-state")
        .expect("the state is refused");
    assert_eq!(state["resource"], "nginx_svc");
    assert_eq!(state["severity"], "error");
    assert_eq!(state["source"], "ansible");
    assert_eq!(state["line"], 25);
    assert!(state["accepted"]
        .as_array()
        .unwrap()
        .contains(&json!("running")));
    assert_eq!(
        state["explain"],
        "explain(rule='unsupported-state', target='ansible')"
    );
    let param = diagnostics
        .iter()
        .find(|d| d["code"] == "unsupported-param")
        .expect("the param is refused");
    assert_eq!(param["resource"], "nginx_svc");
    assert!(param["accepted"]
        .as_array()
        .unwrap()
        .contains(&json!("enabled")));
    assert_eq!(body["summary"]["errors"], 2);
}

#[test]
fn lint_offers_fixes_and_applies_them() {
    let mut server = server();
    let doc = ANSIBLE_DOC
        .replace("state: rebooted", "state: running")
        .replace("        colour: blue\n", "")
        .replace("mode: \"0644\"", "mode: 0644");
    let replies = talk(
        &mut server,
        &[initialize(), call(2, "lint", json!({ "document": doc }))],
    );
    let (body, is_error) = structured(&replies[1]);
    // Ansible refuses the number outright; the lint says how to fix it.
    assert!(is_error, "{body}");
    assert_eq!(body["ok"], false);
    let mode = body["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "unquoted-mode")
        .expect("the mode is linted");
    assert_eq!(mode["severity"], "warning");
    assert_eq!(mode["fixable"], true);
    assert_eq!(mode["fix"]["replace"], "\"0644\"");
    assert_eq!(body["fixesApplied"], 1);
    assert!(body["fixed"].as_str().unwrap().contains("mode: \"0644\""));
}

#[test]
fn compile_returns_the_emitted_files_and_writes_nothing() {
    let mut server = server();
    let doc = ANSIBLE_DOC
        .replace("state: rebooted", "state: running")
        .replace("        colour: blue\n", "");
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(2, "compile", json!({ "document": doc })),
            call(3, "compile", json!({ "document": ANSIBLE_DOC })),
        ],
    );
    let (body, is_error) = structured(&replies[1]);
    assert!(!is_error, "{body}");
    assert_eq!(body["ok"], true);
    let files = body["documents"][0]["files"].as_array().unwrap();
    assert!(!files.is_empty());
    let playbook = files
        .iter()
        .find(|f| f["name"].as_str().unwrap().ends_with(".yml"))
        .expect("a playbook is emitted");
    assert!(playbook["text"].as_str().unwrap().contains("nginx"));
    assert!(playbook.get("omittedChars").is_none());

    let (refused, is_error) = structured(&replies[2]);
    assert!(is_error);
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["documents"], json!([]));
    assert!(!refused["diagnostics"].as_array().unwrap().is_empty());
}

#[test]
fn a_document_that_needs_data_is_checked_but_not_compiled() {
    let mut server = server();
    let doc = ANSIBLE_DOC
        .replace("state: rebooted", "state: running")
        .replace("        colour: blue\n", "")
        .replace("content: x", "content: \"${{ data.banner }}\"");
    let replies = talk(
        &mut server,
        &[initialize(), call(2, "compile", json!({ "document": doc }))],
    );
    let (body, is_error) = structured(&replies[1]);
    assert!(!is_error, "{body}");
    assert_eq!(body["documents"][0]["files"], json!([]));
    let note = &body["diagnostics"][0];
    assert_eq!(note["code"], "needs-data");
    assert_eq!(note["severity"], "info");
}

#[test]
fn format_and_explain_answer_from_the_same_sources_as_the_command() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(
                2,
                "format",
                json!({ "document": "apiVersion: meridian/v1\nkind: ResourceSet\nspec:\n  target: ansible\n  resources: []\nmetadata:\n  name: a\n" }),
            ),
            call(
                3,
                "explain",
                json!({ "rule": "unsupported-state", "target": "ansible" }),
            ),
            call(4, "explain", json!({ "rule": "unquoted-mode" })),
            call(5, "explain", json!({ "rule": "some-targets-own-rule" })),
            call(6, "format", json!({ "document": "a: [" })),
        ],
    );
    let (formatted, _) = structured(&replies[1]);
    assert_eq!(formatted["changed"], true);
    assert!(formatted["document"]
        .as_str()
        .unwrap()
        .starts_with("apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:"));
    let (compiler, _) = structured(&replies[2]);
    assert_eq!(compiler["kind"], "compiler");
    assert_eq!(compiler["target"]["name"], "ansible");
    assert_eq!(compiler["reference"], "meridian://docs/targets/ansible");
    let (lint, _) = structured(&replies[3]);
    assert_eq!(lint["kind"], "lint");
    assert_eq!(
        lint["meaning"],
        meridian_tooling::lint::rule("unquoted-mode")
            .unwrap()
            .summary
    );
    let (own, _) = structured(&replies[4]);
    assert_eq!(own["kind"], "target");
    let (_, is_error) = structured(&replies[5]);
    assert!(is_error);
}

#[test]
fn import_turns_terraform_into_a_document() {
    let mut server = server();
    let hcl = "resource \"aws_instance\" \"web\" {\n  ami = \"ami-123\"\n  instance_type = \"t3.micro\"\n}\n";
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(
                2,
                "import",
                json!({ "target": "terraform", "source": hcl, "fileName": "main.tf" }),
            ),
            call(
                3,
                "import",
                json!({ "target": "ansible", "source": "- hosts: all\n" }),
            ),
            call(
                4,
                "import",
                json!({ "target": "terraform", "source": "resource {{{{" }),
            ),
        ],
    );
    let (body, is_error) = structured(&replies[1]);
    assert!(!is_error, "{body}");
    let text = body["documents"][0]["text"].as_str().unwrap();
    assert!(text.contains("aws_instance"), "{text}");
    assert!(text.contains("target: terraform"), "{text}");
    let (no_importer, is_error) = structured(&replies[2]);
    assert!(is_error);
    assert!(no_importer["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not import"));
    let (bad, is_error) = structured(&replies[3]);
    assert!(is_error);
    assert_eq!(bad["diagnostics"][0]["code"], "import-source");
}

#[test]
fn plan_is_off_unless_allowed_and_files_are_confined() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(2, "plan", json!({ "file": "Cargo.toml" })),
            call(3, "validate", json!({ "file": "../../Cargo.toml" })),
            call(4, "validate", json!({ "file": "/etc/hostname" })),
            call(5, "validate", json!({})),
        ],
    );
    let (off, is_error) = structured(&replies[1]);
    assert!(is_error);
    assert!(off["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--allow-plan"));
    let (outside, is_error) = structured(&replies[2]);
    assert!(is_error);
    assert!(outside["error"]["message"]
        .as_str()
        .unwrap()
        .contains("outside"));
    let (_, is_error) = structured(&replies[3]);
    assert!(is_error);
    let (none, is_error) = structured(&replies[4]);
    assert!(is_error);
    assert!(none["error"]["message"]
        .as_str()
        .unwrap()
        .contains("`document`"));
}

#[test]
fn the_prompt_and_the_resources_are_served() {
    let mut server = server();
    let replies = talk(
        &mut server,
        &[
            initialize(),
            request(2, "prompts/list", json!({})),
            request(
                3,
                "prompts/get",
                json!({ "name": "authoring-guide", "arguments": { "target": "ansible", "task": "an nginx server" } }),
            ),
            request(4, "resources/list", json!({})),
            request(
                5,
                "resources/read",
                json!({ "uri": "meridian://docs/reference/dsl/resources" }),
            ),
            request(
                6,
                "resources/read",
                json!({ "uri": "meridian://docs/nothing" }),
            ),
        ],
    );
    assert_eq!(
        replies[1]["result"]["prompts"][0]["name"],
        "authoring-guide"
    );
    let text = replies[2]["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("`validate`"));
    assert!(text.contains("The target for this task is `ansible`"));
    assert!(text.contains("an nginx server"));
    let resources = replies[3]["result"]["resources"].as_array().unwrap();
    assert!(resources
        .iter()
        .any(|r| r["uri"] == "meridian://docs/reference/capability-matrix"));
    assert!(resources
        .iter()
        .any(|r| r["uri"] == "meridian://docs/targets/ansible"));
    let page = replies[4]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    assert!(page.starts_with("# "));
    assert_eq!(replies[5]["error"]["code"], -32602);
}

#[test]
fn without_plugins_the_language_is_still_checked() {
    let mut server = Server::language_only(env!("CARGO_MANIFEST_DIR"));
    let replies = talk(
        &mut server,
        &[
            initialize(),
            call(2, "list_targets", json!({})),
            call(
                3,
                "validate",
                json!({ "document": "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: a\nspec:\n  target: ansible\n  resources:\n    - id: x\n      type: package\n      dependsOn:\n        - resource: missing\n" }),
            ),
            call(4, "compile", json!({ "document": ANSIBLE_DOC })),
        ],
    );
    assert!(replies[0]["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("No target plugin is loaded"));
    let (targets, _) = structured(&replies[1]);
    assert_eq!(targets["targets"], json!([]));
    let (body, is_error) = structured(&replies[2]);
    assert!(is_error);
    assert_eq!(body["diagnostics"][0]["code"], "invalid-document");
    let (_, is_error) = structured(&replies[3]);
    assert!(is_error);
}
