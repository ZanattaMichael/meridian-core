//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{
    assert_golden, fixture, graph_from_file, graph_from_text, secret_in_param,
};
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

    let err = refusal("    - id: m\n      type: 'mysql::db'\n      params: {path: /mnt}\n");
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

// ---- secrets ------------------------------------------------------------

fn with_secret(resources: &str, id: &str, param: &str, name: &str) -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    );
    let mut graph = graph_from_text("site.yaml", &text, "chef");
    secret_in_param(&mut graph, id, param, name);
    graph
}

#[test]
fn a_secret_in_a_param_is_read_from_the_node_and_the_resource_is_sensitive() {
    let graph = with_secret(
        "    - id: env_file\n      type: file\n      params: {path: /etc/app/env, content: placeholder, mode: \"0600\"}\n    - id: conf\n      type: file\n      params: {path: /etc/app/app.conf, content: \"a = 1\\n\"}\n",
        "env_file",
        "content",
        "db_password",
    );
    let text = emit(&graph).unwrap().artifact.files[&recipe_path("meridian_web")].clone();
    assert!(
        text.contains(
            "file 'env_file' do\n  content node['meridian_secret_db_password']\n  mode '0600'\n  path '/etc/app/env'\n  sensitive true\n  action :create\nend\n"
        ),
        "{text}"
    );
    // The resource with no secret is not made sensitive: its diff still
    // shows.
    assert!(
        text.contains("file 'conf' do\n  content \"a = 1\\n\"\n  path '/etc/app/app.conf'\n  action :create\nend\n"),
        "{text}"
    );
    assert!(!text.contains("placeholder"), "{text}");
    assert_eq!(
        meridian_target_chef::secret_reference("db_password"),
        "node['meridian_secret_db_password']"
    );
    assert_eq!(
        meridian_target_chef::secret_key("db_password"),
        "meridian_secret_db_password"
    );
    assert!(Chef.capabilities().secrets);
}

// ---- the credential a host is reached with -----------------------------

/// A document reaching its host as the credential `entry` (a YAML mapping
/// on one line), with the secrets it may read declared.
fn with_credential(entry: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: web01.example.com\n    credential: deploy\n  secrets:\n    - {{name: deploy_key, from: env, key: DEPLOY_KEY}}\n    - {{name: deploy_password, from: env, key: DEPLOY_PASSWORD}}\n  credentials:\n    - {entry}\n  resources:\n    - id: nginx_pkg\n      type: package\n      params: {{name: nginx}}\n"
    );
    emit(&meridian_plugin_testkit::graph_from_text(
        "site.yaml",
        &text,
        "chef",
    ))
}

#[test]
fn a_credential_with_a_key_is_taken_and_leaves_nothing_in_the_cookbook() {
    for entry in [
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\"}",
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", passphrase: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: ssh, agent: true}",
        "{name: deploy, type: ssh, from: ambient, user: ops}",
    ] {
        let emitted = with_credential(entry).unwrap_or_else(|e| panic!("{entry}: {e}"));
        for (file, text) in &emitted.artifact.files {
            assert!(!text.contains("deploy"), "{file} names the credential:\n{text}");
        }
    }
}

#[test]
fn what_chef_run_cannot_take_of_a_credential_is_refused_with_the_reason() {
    for (entry, reason) in [
        (
            "{name: deploy, type: usernamePassword, user: admin, password: \"${{ secret.deploy_password }}\"}",
            "chef-run takes no password for a host: chef-run takes a password on its command line only",
        ),
        (
            "{name: deploy, type: ssh, user: deploy, password: \"${{ secret.deploy_password }}\"}",
            "chef-run takes no password for a host",
        ),
        (
            "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", certificate: cert.pub}",
            "takes no certificate for a key from Meridian",
        ),
        (
            "{name: deploy, type: ssh, user: \"dep loy\", privateKey: \"${{ secret.deploy_key }}\"}",
            "cannot be given to chef-run",
        ),
    ] {
        let err = with_credential(entry).expect_err(entry);
        assert_eq!(err.rule, "unsupported-credential", "{entry}: {err}");
        assert!(err.msg.contains(reason), "{entry}: {err}");
        assert!(err.msg.starts_with("the credential \"deploy\""), "{err}");
    }
}

// ---- Chef's own resources beside the six -----------------------------------

/// The document behind the `native_resources` golden files: Chef's own
/// resources beside the shared ones, with a state, a guard, notified and
/// notifying, and the native `package` reached through `native/`.
const NATIVE_RESOURCES: &str = "    - id: nginx_pkg\n      type: native/package\n      state: upgrade\n      params:\n        package_name: nginx\n        options: '--no-install-recommends'\n    - id: nginx_conf\n      type: template\n      params:\n        path: /etc/nginx/nginx.conf\n        source: nginx.conf.erb\n        variables: {workers: 4}\n        mode: '0644'\n      dependsOn:\n        - resource: nginx_pkg\n      notifies:\n        - resource: nginx_svc\n          action: reload\n        - resource: reindex\n          action: run\n    - id: nginx_svc\n      type: service\n      params:\n        name: nginx\n    - id: reindex\n      type: apt_update\n      runtimeWhen: \"node['platform_family'] == 'debian'\"\n    - id: timer\n      type: systemd_unit\n      state: enable\n      params:\n        unit_name: reindex.timer\n";

#[test]
fn a_chef_resource_is_written_with_its_properties_as_written() {
    let emitted = compile_named("web", NATIVE_RESOURCES).unwrap_or_else(|e| panic!("{e}"));
    assert_golden(&golden(), "native_resources", &emitted);
    let text = &emitted.artifact.files[&recipe_path("meridian_web")];
    // The properties are the resource's own, as written, and the state is
    // its action.
    assert!(
        text.contains("package 'nginx_pkg' do\n  options '--no-install-recommends'\n  package_name 'nginx'\n  action :upgrade\nend\n"),
        "{text}"
    );
    // A hash is written in parentheses, or Ruby reads it as a block.
    assert!(text.contains("  variables({ 'workers' => 4 })\n"), "{text}");
    // A resource with no state takes its default action: no action line.
    assert!(
        text.contains(
            "apt_update 'reindex' do\n  only_if { node['platform_family'] == 'debian' }\nend\n"
        ),
        "{text}"
    );
    // A notified resource takes the action named.
    assert!(
        text.contains("  notifies :run, 'apt_update[reindex]', :delayed\n"),
        "{text}"
    );
    assert!(
        text.contains("  notifies :reload, 'service[nginx_svc]', :delayed\n"),
        "{text}"
    );
}

#[test]
fn a_chef_resource_takes_its_action_once_and_as_a_word() {
    let err = refusal(
        "    - id: a\n      type: template\n      state: create\n      params:\n        path: /x\n        action: create\n",
    );
    assert_eq!(err.rule, "param-conflicts-with-state");

    let err = refusal(
        "    - id: a\n      type: template\n      state: Create\n      params:\n        path: /x\n",
    );
    assert_eq!(err.rule, "unsupported-state");

    let err = refusal("    - id: a\n      type: template\n      params:\n        path: ~\n");
    assert_eq!(err.rule, "empty-param");

    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: t\n          action: Run\n    - id: t\n      type: template\n      params: {path: /x}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "conf");
    assert!(err.msg.contains("not a Chef action"), "{}", err.msg);
}

#[test]
fn a_name_that_is_not_a_chef_resource_is_refused() {
    for r#type in [
        "Template",
        "ansible.builtin.cron",
        "mysql::db",
        "native/cron",
    ] {
        let err = refusal(&format!(
            "    - id: a\n      type: '{type}'\n      params:\n        path: /x\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{type}");
    }
    // Chef runs a command with execute, so there is no exec to reach.
    let err = refusal(
        "    - id: a\n      type: native/exec\n      params:\n        command: /bin/true\n",
    );
    assert_eq!(err.rule, "unsupported-resource-type");
    assert!(err.msg.contains("execute"), "{}", err.msg);
}

#[test]
fn a_native_package_and_the_shared_package_managing_one_name_are_refused_together() {
    let err = refusal(
        "    - id: a\n      type: package\n      params: {name: nginx}\n    - id: b\n      type: native/package\n      params: {package_name: nginx, version: '1.2'}\n",
    );
    assert_eq!(err.rule, "duplicate-resource");
    assert_eq!(err.resource, "b");
}

#[test]
fn a_secret_handed_to_a_chef_resource_is_read_from_the_node_and_the_block_is_sensitive() {
    let graph = with_secret(
        "    - id: db\n      type: mysql_database\n      params:\n        database_name: app\n        password: placeholder\n",
        "db",
        "password",
        "db_password",
    );
    let text = emit(&graph).unwrap().artifact.files[&recipe_path("meridian_web")].clone();
    assert!(
        text.contains("  password node['meridian_secret_db_password']\n"),
        "{text}"
    );
    assert!(text.contains("  sensitive true\n"), "{text}");
    assert!(!text.contains("placeholder"), "{text}");
}

#[test]
fn the_schemas_offer_any_chef_resource_as_an_open_pattern() {
    let schemas = Chef.resource_schemas();
    let pattern = schemas
        .iter()
        .find(|s| s.r#type == meridian_target_chef::NATIVE_PATTERN)
        .expect("the resource pattern is declared");
    assert!(pattern.open && pattern.is_pattern() && pattern.params.is_empty());
    assert_eq!(
        Chef.supported_resource_types(),
        [
            "<resource>",
            "exec",
            "file",
            "group",
            "package",
            "service",
            "user"
        ]
    );
}
