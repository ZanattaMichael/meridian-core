//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, fixture, graph_from_file, graph_from_text};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_chef::{recipe_path, Chef};

/// The fixtures every configuration management target compiles.
const FIXTURES: &[&str] = &[
    "accounts",
    "restart_chain",
    "runtime_condition",
    "web_stack",
];

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Chef.emit(graph, &Data::new())
}

fn compile_named(name: &str, resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: {name}\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    );
    emit(&graph_from_text("site.yaml", &text, "chef"))
}

fn recipe(resources: &str) -> String {
    compile_named("web", resources).unwrap().artifact.files[&recipe_path("meridian_web")].clone()
}

fn refusal(resources: &str) -> CompileError {
    compile_named("web", resources).expect_err("the document should be refused")
}

#[test]
fn every_fixture_compiles_to_its_golden_files() {
    for name in FIXTURES {
        let emitted = emit(&fixture(name, "chef")).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(emitted.warnings.is_empty(), "{name}");
        assert_golden(&golden(), name, &emitted);
    }
}

#[test]
fn the_example_compiles_to_its_golden_files() {
    let graph = graph_from_file("examples/chef/web-server.yaml", "chef");
    assert_golden(&golden(), "web-server", &emit(&graph).unwrap());
}

#[test]
fn a_block_is_properties_then_notifications_then_the_action() {
    let text = recipe(
        "    - id: conf\n      type: file\n      params: {path: /etc/app.conf, content: \"a = 1\\n\", mode: \"0640\"}\n      notifies:\n        - resource: svc\n          action: reload\n    - id: svc\n      type: service\n      params: {name: app, enabled: true}\n",
    );
    assert!(
        text.ends_with(
            "\nfile 'conf' do\n  content \"a = 1\\n\"\n  mode '0640'\n  path '/etc/app.conf'\n  notifies :reload, 'service[svc]', :delayed\n  action :create\nend\n\nservice 'svc' do\n  service_name 'app'\n  action [:enable, :start]\nend\n"
        ),
        "{text}"
    );
}

#[test]
fn a_directory_is_its_own_resource_and_has_no_content() {
    let text = recipe(
        "    - id: root\n      type: file\n      state: directory\n      params: {path: /srv/app, owner: deploy}\n",
    );
    assert!(
        text.contains(
            "directory 'root' do\n  owner 'deploy'\n  path '/srv/app'\n  action :create\nend\n"
        ),
        "{text}"
    );

    let err = refusal(
        "    - id: root\n      type: file\n      state: directory\n      params: {path: /srv/app, content: x}\n",
    );
    assert_eq!(err.rule, "invalid-param");
}

#[test]
fn supplementary_groups_are_memberships_that_leave_other_members_alone() {
    let text = recipe(
        "    - id: deploy\n      type: user\n      params: {name: deploy, groups: [sudo]}\n",
    );
    assert!(
        text.contains("user 'deploy' do\n  username 'deploy'\n  action :create\nend\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "group 'deploy:sudo' do\n  append true\n  group_name 'sudo'\n  members ['deploy']\n  action :modify\nend\n"
        ),
        "{text}"
    );
}

#[test]
fn a_runtime_condition_is_a_guard() {
    let text = recipe(
        "    - id: warm\n      type: exec\n      params: {command: /usr/local/bin/warm, chdir: /srv}\n      runtimeWhen: \"node['platform_family'] == 'debian'\"\n",
    );
    assert!(
        text.contains(
            "execute 'warm' do\n  command '/usr/local/bin/warm'\n  cwd '/srv'\n  only_if { node['platform_family'] == 'debian' }\n  action :run\nend\n"
        ),
        "{text}"
    );
}

#[test]
fn a_type_with_nothing_to_do_cannot_be_notified() {
    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: pkg\n          action: restart\n    - id: pkg\n      type: package\n      params: {name: app}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "conf");
    assert!(
        err.msg.contains("has nothing to do when notified"),
        "{}",
        err.msg
    );

    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: svc\n          action: bounce\n    - id: svc\n      type: service\n      params: {name: app}\n",
    );
    assert_eq!(
        err.msg,
        "notifies resource \"svc\" with action \"bounce\", which resource type \"service\" \
         does not support on this target (supported: reload, restart, start, stop)"
    );
}

#[test]
fn states_params_and_types_outside_the_table_are_refused() {
    let err = refusal(
        "    - id: svc\n      type: service\n      state: restarted\n      params: {name: app}\n",
    );
    assert_eq!(
        err.msg,
        "resource type \"service\" does not accept state \"restarted\" on this target \
         (accepted: running, started, stopped)"
    );

    let err =
        refusal("    - id: p\n      type: package\n      params: {name: a, version: \"1\"}\n");
    assert_eq!(err.rule, "unsupported-param");

    let err = refusal("    - id: p\n      type: package\n      params: {}\n");
    assert_eq!(err.rule, "missing-param");

    let err = refusal("    - id: m\n      type: mount\n      params: {path: /mnt}\n");
    assert_eq!(err.rule, "unsupported-resource-type");
}

#[test]
fn two_resources_managing_one_thing_are_refused() {
    let err = refusal(
        "    - id: a\n      type: package\n      params: {name: nginx}\n    - id: b\n      type: package\n      state: latest\n      params: {name: nginx}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "b");
    assert_eq!(
        err.msg,
        "manages the same package as resource \"a\": both set package_name to \"nginx\""
    );
}

#[test]
fn a_document_name_that_cannot_be_a_cookbook_is_refused() {
    // A file-safe name the core accepts (issue 133), but not a cookbook name.
    let err = compile_named(
        "Web-Server",
        "    - id: a\n      type: package\n      params: {name: nginx}\n",
    )
    .unwrap_err();
    assert_eq!(err.rule, "invalid-cookbook-name");
}

#[test]
fn a_reference_is_settled_from_the_document_or_refused() {
    let text = recipe(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: page\n      type: file\n      params:\n        path: ${root.path}/index.html\n        content: \"costs $${PRICE}\"\n",
    );
    assert!(text.contains("/srv/app/index.html"), "{text}");
    assert!(text.contains("costs ${PRICE}"), "{text}");
    assert!(!text.contains("${root"), "{text}");
    assert!(!text.contains("$${"), "{text}");

    let err = refusal(
        "    - id: page\n      type: file\n      params:\n        path: ${nowhere.path}/index.html\n        content: x\n",
    );
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "page");
    assert!(err.msg.contains("does not declare"), "{}", err.msg);
}
