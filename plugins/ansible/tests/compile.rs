//! The compile half, without a process boundary.
//!
//! The golden files under `testdata` were recorded by the Go implementation
//! this plugin replaced. Matching them byte for byte is the pass mark for the
//! port: an author sees no difference between the two.

#![allow(clippy::result_large_err)]

use std::path::Path;

use meridian_plugin_testkit::{
    assert_golden, fixture, graph_from_text, secret_in_param, without_positions,
};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, Position, ResourceGraph, Value};
use meridian_target_ansible::Ansible;

const FIXTURES: &[&str] = &[
    "accounts",
    "restart_chain",
    "runtime_condition",
    "web_stack",
];

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Ansible.emit(graph, &Data::new())
}

/// A resource set with the given resources block, compiled.
fn compile(resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    );
    emit(&graph_from_text("site.yaml", &text, "ansible"))
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

#[test]
fn every_fixture_compiles_to_the_bytes_the_go_plugin_recorded() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    for name in FIXTURES {
        let graph = without_positions(fixture(name, "ansible"));
        let emitted = emit(&graph).unwrap_or_else(|e| panic!("compiling {name}: {e}"));
        assert_golden(&golden, name, &emitted);
    }
}

#[test]
fn compiling_is_deterministic() {
    let graph = fixture("web_stack", "ansible");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn an_unknown_type_names_the_types_that_work_and_the_line() {
    let err = refusal("    - id: fw\n      type: firewall_rule\n      params:\n        port: 80\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(err.resource, "fw");
    assert_eq!(err.target, "ansible");
    assert_eq!(
        err.msg,
        "unsupported resource type \"firewall_rule\" on this target \
         (supported: exec, file, group, package, service, user)"
    );
    assert_eq!((err.pos.file.as_str(), err.pos.line), ("site.yaml", 11));
}

#[test]
fn a_param_the_module_does_not_take_is_refused_not_dropped() {
    let err = refusal(
        "    - id: nginx\n      type: package\n      params:\n        name: nginx\n        version: '1.2'\n",
    );
    assert_eq!(err.rule, "unsupported-param");
    assert_eq!(
        err.msg,
        "resource type \"package\" does not accept param \"version\" on this target (accepted: name)"
    );
}

#[test]
fn a_missing_required_param_is_refused() {
    let err = refusal("    - id: conf\n      type: file\n      params:\n        owner: root\n");
    assert_eq!(err.rule, "missing-param");
    assert_eq!(
        err.msg,
        "resource type \"file\" requires param \"path\" on this target"
    );
}

#[test]
fn a_state_outside_the_vocabulary_names_the_ones_accepted() {
    let err = refusal(
        "    - id: nginx\n      type: service\n      state: paused\n      params:\n        name: nginx\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert_eq!(
        err.msg,
        "resource type \"service\" does not accept state \"paused\" on this target \
         (accepted: reloaded, restarted, running, started, stopped)"
    );
}

#[test]
fn literal_content_cannot_be_a_directory() {
    let err = refusal(
        "    - id: conf\n      type: file\n      state: directory\n      params:\n        path: /etc/app\n        content: hello\n",
    );
    assert_eq!(err.rule, "param-conflicts-with-state");
    assert_eq!(
        err.msg,
        "param \"content\" cannot be used with state \"directory\": a file with content is \
         written as it is given, and only a file in state \"file\" or \"present\" has content"
    );
}

#[test]
fn exec_takes_no_state() {
    let err = refusal(
        "    - id: run\n      type: exec\n      state: present\n      params:\n        command: /bin/true\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert_eq!(
        err.msg,
        "resource type \"exec\" accepts no state on this target, got \"present\""
    );
}

#[test]
fn a_notify_action_the_type_cannot_perform_is_refused() {
    let resources = "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: x\n      notifies:\n        - resource: svc\n          action: bounce\n    - id: svc\n      type: service\n      params:\n        name: app\n";
    let err = refusal(resources);
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "svc");
    assert_eq!(
        err.msg,
        "resource type \"service\" does not support notify action \"bounce\" on this target \
         (supported: reload, restart, start, stop)"
    );

    let err = refusal(&resources.replace("type: service", "type: package"));
    assert_eq!(
        err.msg,
        "resource type \"package\" cannot be notified on this target, got action \"bounce\""
    );
}

#[test]
fn two_notifiers_of_one_change_share_a_handler() {
    let emitted = compile(
        "    - id: a\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: svc\n          action: restart\n    - id: b\n      type: file\n      params: {path: /b, content: b}\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      params: {name: app}\n",
    )
    .unwrap();
    let playbook = &emitted.artifact.files["playbook.yml"];
    assert_eq!(
        playbook.matches("- name: restart svc").count(),
        1,
        "{playbook}"
    );
    assert_eq!(playbook.matches("- restart svc").count(), 2, "{playbook}");
}

#[test]
fn a_handler_does_not_inherit_a_runtime_condition() {
    let emitted = compile(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: job\n          action: run\n    - id: job\n      type: exec\n      runtimeWhen: ansible_facts['os_family'] == 'Debian'\n      params: {command: /bin/reindex}\n",
    )
    .unwrap();
    let playbook = &emitted.artifact.files["playbook.yml"];
    assert_eq!(playbook.matches("when:").count(), 1, "{playbook}");
}

#[test]
fn values_that_look_like_other_types_stay_strings() {
    let emitted = compile(
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        mode: '0644'\n        owner: 'yes'\n        content: \"a: b\\n# not a comment\\n\"\n",
    )
    .unwrap();
    let playbook = &emitted.artifact.files["playbook.yml"];
    assert!(playbook.contains("mode: \"0644\""), "{playbook}");
    assert!(playbook.contains("owner: \"yes\""), "{playbook}");
    assert!(
        playbook.contains("content: |\n          a: b\n          # not a comment\n"),
        "{playbook}"
    );
}

#[test]
fn a_tree_whose_order_breaks_a_dependency_is_refused() {
    let mut graph = fixture("restart_chain", "ansible");
    graph.resources.reverse();
    let err = emit(&graph).expect_err("the order contradicts the edges");
    assert_eq!(err.rule, "ordering-violated");
}

#[test]
fn a_play_without_a_host_is_refused() {
    let mut graph = fixture("accounts", "ansible");
    graph.host.clear();
    assert_eq!(emit(&graph).unwrap_err().rule, "missing-host");
}

#[test]
fn a_param_with_no_value_is_refused() {
    let mut graph = fixture("accounts", "ansible");
    let first = &mut graph.resources[0];
    let key = first.params.keys().next().unwrap().clone();
    first.params.insert(key, Value::Null);
    first.pos = Position::default();
    assert_eq!(emit(&graph).unwrap_err().rule, "empty-param");
}

#[test]
fn a_reference_is_settled_from_the_document_or_refused() {
    let emitted = compile(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: page\n      type: file\n      params:\n        path: ${root.path}/index.html\n        content: \"costs $${PRICE}\"\n",
    )
    .unwrap();
    let all: String = emitted.artifact.files.values().cloned().collect();
    assert!(all.contains("/srv/app/index.html"), "{all}");
    assert!(all.contains("costs ${PRICE}"), "{all}");
    assert!(!all.contains("${root"), "{all}");
    assert!(!all.contains("$${"), "{all}");

    let err = refusal(
        "    - id: page\n      type: file\n      params:\n        path: ${nowhere.path}/index.html\n        content: x\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "page");
    assert!(err.msg.contains("does not declare"), "{}", err.msg);
}

#[test]
fn a_secret_in_a_param_is_written_as_the_variable_the_runner_hands_over() {
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n    - id: env_file\n      type: file\n      params:\n        path: /etc/app/env\n        content: placeholder\n";
    let mut graph = graph_from_text("site.yaml", text, "ansible");
    secret_in_param(&mut graph, "env_file", "content", "db_password");
    let playbook = emit(&graph).unwrap().artifact.files["playbook.yml"].clone();
    assert!(
        playbook.contains("content: '{{ meridian_secret_db_password }}'"),
        "{playbook}"
    );
    assert!(!playbook.contains("(sensitive)"), "{playbook}");
    // The task is not allowed to print what it was handed.
    assert!(playbook.contains("    no_log: true\n"), "{playbook}");
    assert_eq!(playbook.matches("no_log").count(), 1, "{playbook}");
    assert_eq!(
        meridian_target_ansible::secret_variable("db_password"),
        "meridian_secret_db_password"
    );
}

#[test]
fn a_host_that_is_not_a_name_is_refused_rather_than_written_into_the_inventory() {
    // Issue 134: a space in the host would add a host variable to
    // inventory.ini, and a line break a section. The core refuses such a
    // host first; this is the plugin's own line of defence.
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: web01\n  resources:\n    - id: a\n      type: package\n      params: {name: nginx}\n";
    let mut graph = graph_from_text("site.yaml", text, "ansible");
    graph.host = "web01 ansible_connection=local\n[all:vars]\nansible_python_interpreter=/tmp/evil"
        .to_owned();
    let refused = emit(&graph).expect_err("refused");
    assert_eq!(refused.rule, "invalid-host");
    assert!(
        refused.msg.contains("inventory") && refused.msg.contains("whitespace"),
        "{refused}"
    );
}

#[test]
fn the_plugin_declares_jinjas_openings_so_placed_text_stays_text() {
    // Issue 135: Ansible renders every task argument through Jinja, so the
    // core is told what to escape in a value placed from data. What the
    // escape is written as must be an expression that yields the opening.
    let declared = Ansible.placed_text();
    let texts: Vec<&str> = declared.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["{{", "{%", "{#"]);
    for escape in &declared {
        assert!(!escape.only_at_start);
        assert_eq!(escape.written, format!("{{{{ '{}' }}}}", escape.text));
    }
    // And what the core hands over is written as it is: the playbook holds
    // the escaped form, which Jinja renders back to the data's text.
    let emitted = compile(
        "    - id: motd\n      type: file\n      params:\n        path: /etc/motd\n        content: \"hello {{ '{{' }} lookup('pipe','id') }}\"\n",
    )
    .unwrap();
    assert!(
        emitted.artifact.files["playbook.yml"].contains("{{ '{{' }} lookup"),
        "{}",
        emitted.artifact.files["playbook.yml"]
    );
}

// ---- the credential a host is reached with -----------------------------

/// A document reaching its host as the credential `entry` (a YAML mapping
/// on one line), with the two secrets it may read declared.
fn with_credential(entry: &str) -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: web01.example.com\n    credential: deploy\n  secrets:\n    - {{name: deploy_key, from: env, key: DEPLOY_KEY}}\n    - {{name: deploy_password, from: env, key: DEPLOY_PASSWORD}}\n  credentials:\n    - {entry}\n  resources:\n    - id: a\n      type: package\n      params: {{name: nginx}}\n"
    );
    graph_from_text("site.yaml", &text, "ansible")
}

fn inventory(graph: &ResourceGraph) -> String {
    emit(graph).unwrap().artifact.files["inventory.ini"].clone()
}

#[test]
fn a_credentials_user_is_written_into_the_inventory_and_its_secrets_are_not() {
    let graph = with_credential(
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\"}",
    );
    let credential = graph.credential.as_ref().unwrap();
    assert_eq!(credential.secret_fields, ["privateKey"]);
    let emitted = emit(&graph).unwrap();
    assert_eq!(
        emitted.artifact.files["inventory.ini"],
        "[meridian]\nweb01.example.com ansible_user=deploy\n"
    );
    // The playbook does not mention the credential: the runner hands its
    // key over when it runs.
    let playbook = &emitted.artifact.files["playbook.yml"];
    assert!(!playbook.contains("deploy"), "{playbook}");
    assert!(!playbook.contains("private"), "{playbook}");

    // A password credential is the same in the inventory; the password
    // goes in the private extra-vars file at run time.
    let graph = with_credential(
        "{name: deploy, type: usernamePassword, user: admin, password: \"${{ secret.deploy_password }}\"}",
    );
    assert_eq!(
        inventory(&graph),
        "[meridian]\nweb01.example.com ansible_user=admin\n"
    );
    // One with no user leaves the tool's own.
    let graph = with_credential("{name: deploy, type: ssh, agent: true}");
    assert_eq!(inventory(&graph), "[meridian]\nweb01.example.com\n");
    // So does an ambient one.
    let graph = with_credential("{name: deploy, type: ssh, from: ambient, user: ops}");
    assert_eq!(
        inventory(&graph),
        "[meridian]\nweb01.example.com ansible_user=ops\n"
    );
}

#[test]
fn what_ansible_playbook_cannot_take_of_a_credential_is_refused_with_the_reason() {
    for (entry, reason) in [
        (
            "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", passphrase: \"${{ secret.deploy_password }}\"}",
            "takes no passphrase for a private key",
        ),
        (
            "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", certificate: /etc/ssh/deploy-cert.pub}",
            "takes no certificate for a key",
        ),
        (
            "{name: deploy, type: usernamePassword, user: admin, domain: CORP, password: \"${{ secret.deploy_password }}\"}",
            "takes no domain",
        ),
        (
            "{name: deploy, type: ssh, user: \"deploy ansible_become=true\", agent: true}",
            "cannot be written into the inventory",
        ),
        (
            "{name: deploy, type: ssh, user: \"[all:vars]\", agent: true}",
            "cannot be written into the inventory",
        ),
    ] {
        let refused = emit(&with_credential(entry)).expect_err(entry);
        assert_eq!(refused.rule, "unsupported-credential", "{entry}");
        assert!(
            refused.msg.starts_with("the credential \"deploy\": ") && refused.msg.contains(reason),
            "{entry}: {}",
            refused.msg
        );
        assert!(refused.resource.is_empty());
    }
    // The core only lets an ssh or usernamePassword credential reach a
    // host; the plugin refuses any other for itself.
    let mut graph = with_credential("{name: deploy, type: ssh, agent: true}");
    graph.credential = Some(meridian_sdk::Credential {
        name: "cloud".to_owned(),
        kind: "aws".to_owned(),
        ambient: true,
        text: Default::default(),
        flags: Default::default(),
        secret_fields: Vec::new(),
    });
    let refused = emit(&graph).unwrap_err();
    assert_eq!(refused.rule, "unsupported-credential");
    assert!(refused.msg.contains("not aws"), "{}", refused.msg);
}
