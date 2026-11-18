//! The compile half, without a process boundary.
//!
//! The golden files under `testdata` were recorded by the Go implementation
//! this plugin replaces. Matching them byte for byte is the pass mark for the
//! port.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{
    assert_golden, assert_golden_refusal, fixture, graph_from_text, secret_in_param,
    without_positions,
};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_puppet::Puppet;

const FIXTURES: &[&str] = &[
    "accounts",
    "restart_chain",
    "runtime_condition",
    "web_stack",
];
/// The fixtures Puppet cannot express, and says so.
const REFUSED: &[&str] = &["runtime_condition", "web_stack"];

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Puppet.emit(graph, &Data::new())
}

fn compile_named(name: &str, resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: {name}\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    );
    emit(&graph_from_text("site.yaml", &text, "puppet"))
}

fn compile(resources: &str) -> Result<Emitted, CompileError> {
    compile_named("web", resources)
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

fn manifest(resources: &str) -> String {
    compile(resources).unwrap().artifact.files["manifests/init.pp"].clone()
}

#[test]
fn every_fixture_compiles_or_is_refused_as_the_go_plugin_recorded() {
    for name in FIXTURES {
        let graph = without_positions(fixture(name, "puppet"));
        match emit(&graph) {
            Ok(emitted) => {
                assert!(!REFUSED.contains(name), "{name} should be refused");
                assert_golden(&golden(), name, &emitted);
            }
            Err(refusal) => {
                assert!(REFUSED.contains(name), "{name}: {refusal}");
                assert_golden_refusal(&golden(), name, &refusal);
            }
        }
    }
}

#[test]
fn a_refusal_of_a_notify_points_at_the_notifier() {
    let graph = fixture("web_stack", "puppet");
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.resource, "nginx_site");
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.pos.file, "fixtures/web_stack.yaml");
    assert!(err.pos.line >= graph.resource("nginx_site").unwrap().pos.line);
}

#[test]
fn a_type_puppet_cannot_refresh_cannot_be_notified() {
    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: pkg\n          action: restart\n    - id: pkg\n      type: package\n      params: {name: app}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(
        err.msg,
        "notifies resource \"pkg\", which this target cannot refresh: Puppet refreshes \
         only the types that define a refresh, and package is not one of them"
    );
}

#[test]
fn an_action_outside_the_table_names_the_ones_supported() {
    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: svc\n          action: bounce\n    - id: svc\n      type: service\n      params: {name: app}\n",
    );
    assert_eq!(
        err.msg,
        "notifies resource \"svc\" with action \"bounce\", which resource type \"service\" \
         does not support on this target (supported: refresh, restart)"
    );
}

#[test]
fn a_service_cannot_be_left_restarted() {
    let err = refusal(
        "    - id: svc\n      type: service\n      state: restarted\n      params: {name: app}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert_eq!(
        err.msg,
        "resource type \"service\" does not accept state \"restarted\" on this target \
         (accepted: running, started, stopped)"
    );
}

#[test]
fn two_resources_managing_one_thing_are_refused_here_not_on_the_node() {
    let err = refusal(
        "    - id: a\n      type: package\n      params: {name: nginx}\n    - id: b\n      type: package\n      state: latest\n      params: {name: nginx}\n",
    );
    assert_eq!(err.rule, "duplicate-namevar");
    assert_eq!(err.resource, "b");
    assert!(err
        .msg
        .starts_with("this package manages \"nginx\", which resource \"a\" already manages"));
}

#[test]
fn a_document_name_that_cannot_be_a_class_is_refused() {
    let err = compile_named(
        "web-server",
        "    - id: a\n      type: package\n      params: {name: nginx}\n",
    )
    .unwrap_err();
    assert_eq!(err.rule, "invalid-class-name");
    assert_eq!(
        err.msg,
        "document name \"web-server\" cannot become a Puppet class name: \"-\" is not allowed, \
         and a class name may hold only lower-case letters, digits and underscores"
    );
}

#[test]
fn a_notify_already_orders_so_no_require_repeats_it() {
    let text = manifest(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      params: {name: app}\n      dependsOn:\n        - resource: conf\n",
    );
    assert!(text.contains("notify  => Service['svc'],"), "{text}");
    assert!(!text.contains("require"), "{text}");
}

#[test]
fn strings_are_quoted_so_puppet_reads_what_was_written() {
    let text = manifest(
        "    - id: conf\n      type: file\n      params:\n        path: /etc/motd\n        content: \"it's $HOME\\n\"\n        owner: $user\n    - id: shell\n      type: exec\n      params:\n        command: echo hi | tee /tmp/x\n        shell: true\n        chdir: /tmp\n",
    );
    assert!(text.contains("content => \"it's \\$HOME\\n\","), "{text}");
    assert!(text.contains("owner   => '$user',"), "{text}");
    assert!(text.contains("provider => shell,"), "{text}");
    assert!(text.contains("cwd      => '/tmp',"), "{text}");
}

#[test]
fn numbers_lists_and_maps_are_written_as_puppet_source() {
    let text = manifest(
        "    - id: u\n      type: user\n      params:\n        name: app\n        uid: 1200\n        system: true\n        groups: [a, b]\n",
    );
    assert!(text.contains("uid    => 1200,"), "{text}");
    assert!(text.contains("system => true,"), "{text}");
    assert!(text.contains("groups => ['a', 'b'],"), "{text}");
}

#[test]
fn a_title_with_a_quote_is_refused() {
    let mut graph = fixture("accounts", "puppet");
    graph.resources[0].id = "it's".to_owned();
    graph.edges.clear();
    assert_eq!(emit(&graph).unwrap_err().rule, "quoted-title");
}

#[test]
fn a_reference_becomes_the_value_the_other_resource_declares() {
    let text = manifest(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n        mode: \"0755\"\n    - id: page\n      type: file\n      params:\n        path: ${root.path}/index.html\n        mode: ${root.mode}\n        content: \"served from ${root.path}, costs $${PRICE} or $5\"\n",
    );
    assert!(text.contains("path    => '/srv/app/index.html',"), "{text}");
    assert!(text.contains("mode    => '0755',"), "{text}");
    assert!(
        text.contains("content => 'served from /srv/app, costs ${PRICE} or $5',"),
        "{text}"
    );
}

#[test]
fn a_whole_value_reference_keeps_its_type_and_references_chain() {
    let text = manifest(
        "    - id: a\n      type: user\n      params:\n        name: app\n        uid: 1200\n        groups: [x, y]\n    - id: b\n      type: user\n      params:\n        name: ${a.name}2\n        uid: ${a.uid}\n        groups: ${a.groups}\n    - id: c\n      type: group\n      params:\n        name: ${b.name}\n",
    );
    assert!(text.contains("name   => 'app2',"), "{text}");
    assert!(text.contains("uid    => 1200,"), "{text}");
    assert!(text.contains("groups => ['x', 'y'],"), "{text}");
    assert!(
        text.contains("group { 'c':\n    ensure => present,\n    name   => 'app2',"),
        "{text}"
    );
}

#[test]
fn a_reference_that_cannot_be_settled_is_refused() {
    let page = |value: &str| {
        refusal(&format!(
            "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: page\n      type: file\n      params:\n        path: /srv/app/index.html\n        content: \"{value}\"\n"
        ))
    };
    for (value, says) in [
        ("${nowhere.path}", "does not declare"),
        ("${root}", "not which of its params"),
        ("${root.owner}", "does not set"),
        ("${root.path.x}", "nothing deeper"),
        ("${upper(root.path)}", "no expression language"),
        ("${page.content}", "refers back to itself"),
        ("${root.path", "never closed"),
    ] {
        let err = page(value);
        assert_eq!(err.rule, "invalid-reference", "{value}");
        assert_eq!(err.resource, "page", "{value}");
        assert!(err.msg.contains(says), "{value}: {}", err.msg);
    }
}

#[test]
fn a_directory_with_content_is_refused_here_not_on_the_node() {
    let err = refusal(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n        content: \"x\\n\"\n",
    );
    assert_eq!(err.rule, "param-conflicts-with-state");
    assert_eq!(err.resource, "root");
    assert_eq!(
        err.msg,
        "param \"content\" cannot be used with state \"directory\": a directory has no content; \
         remove the param or the state"
    );
}

#[test]
fn a_host_that_is_not_a_name_is_refused_rather_than_written_into_site_pp() {
    // Issue 134: a quote in the host would end the node statement, and what
    // followed would be Puppet code. The core refuses such a host first;
    // this is the plugin's own line of defence, and site.pp is written
    // through the same quoting as every other string.
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: web01\n  resources:\n    - id: a\n      type: package\n      params: {name: nginx}\n";
    let mut graph = graph_from_text("site.yaml", text, "puppet");
    graph.host = "x' { } exec { 'x': command => 'touch /tmp/pwned' } node 'y".to_owned();
    let refused = emit(&graph).expect_err("refused");
    assert_eq!(refused.rule, "invalid-host");
    assert!(refused.msg.contains("site.pp"), "{refused}");

    let site = &compile("    - id: a\n      type: package\n      params: {name: nginx}\n")
        .unwrap()
        .artifact
        .files[meridian_target_puppet::SITE_PATH];
    assert!(site.contains("node 'web01.example.com' {"), "{site}");
}

// ---- secrets ------------------------------------------------------------

/// The document a secret is put into: a file whose content the runner hands
/// over through Hiera.
fn with_secret(param: &str) -> ResourceGraph {
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n    - id: env_file\n      type: file\n      params: {path: /etc/app/env, content: placeholder, mode: \"0600\"}\n";
    let mut graph = graph_from_text("site.yaml", text, "puppet");
    secret_in_param(&mut graph, "env_file", param, "db_password");
    graph
}

#[test]
fn a_secret_in_a_param_is_a_sensitive_lookup_the_runner_answers_through_hiera() {
    let emitted = emit(&with_secret("content")).unwrap();
    let manifest = &emitted.artifact.files["manifests/init.pp"];
    assert!(
        manifest.contains("content => Sensitive(lookup('meridian_secret_db_password')),"),
        "{manifest}"
    );
    assert!(!manifest.contains("placeholder"), "{manifest}");
    assert_eq!(
        meridian_target_puppet::secret_key("db_password"),
        "meridian_secret_db_password"
    );
    assert_eq!(
        meridian_target_puppet::secret_reference("db_password"),
        "Sensitive(lookup('meridian_secret_db_password'))"
    );
    assert!(Puppet.capabilities().secrets);
}

#[test]
fn a_secret_inside_a_list_or_a_map_is_refused_because_a_lookup_is_a_whole_value() {
    let mut graph = with_secret("content");
    let slot = graph.secrets[0].secret;
    let file = graph
        .resources
        .iter_mut()
        .find(|r| r.id == "env_file")
        .unwrap();
    file.params.insert(
        "content".to_owned(),
        meridian_sdk::Value::List(vec![meridian_sdk::Value::Secret(slot)]),
    );
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unsupported-secret-place");
    assert_eq!(err.resource, "env_file");
    assert!(err.msg.contains("inside a list or a map"), "{err}");
}

#[test]
fn a_secret_the_graph_does_not_name_is_refused_not_masked() {
    let mut graph = with_secret("content");
    graph.secrets.clear();
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.rule, "unnamed-secret");
}

// ---- the credential a host is reached with -----------------------------

/// A document reaching its host as the credential `entry` (a YAML mapping
/// on one line), with the secrets it may read declared.
fn with_credential(entry: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: web01.example.com\n    credential: deploy\n  secrets:\n    - {{name: deploy_key, from: env, key: DEPLOY_KEY}}\n    - {{name: deploy_password, from: env, key: DEPLOY_PASSWORD}}\n  credentials:\n    - {entry}\n  resources:\n    - id: app_pkg\n      type: package\n      params: {{name: nginx}}\n"
    );
    emit(&graph_from_text("site.yaml", &text, "puppet"))
}

#[test]
fn a_credential_bolt_can_take_is_accepted_and_leaves_nothing_in_the_manifest() {
    for entry in [
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\"}",
        "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", passphrase: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: ssh, user: deploy, password: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: usernamePassword, user: deploy, password: \"${{ secret.deploy_password }}\"}",
        "{name: deploy, type: ssh, agent: true}",
        "{name: deploy, type: ssh, from: ambient, user: ops}",
    ] {
        let emitted = with_credential(entry).unwrap_or_else(|e| panic!("{entry}: {e}"));
        // The runner writes the credential into Bolt's inventory; the
        // manifest holds nothing of it.
        for (file, text) in &emitted.artifact.files {
            assert!(!text.contains("deploy"), "{file} names the credential:\n{text}");
        }
    }
}

#[test]
fn what_bolt_cannot_take_of_a_credential_is_refused_with_the_reason() {
    for (entry, reason) in [
        (
            "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\", certificate: cert.pub}",
            "bolt takes no certificate for a key from Meridian",
        ),
        (
            "{name: deploy, type: usernamePassword, user: admin, password: \"${{ secret.deploy_password }}\", domain: CORP}",
            "bolt over SSH takes no domain",
        ),
        (
            "{name: deploy, type: ssh, user: \"dep loy\", privateKey: \"${{ secret.deploy_key }}\"}",
            "cannot be given to bolt",
        ),
    ] {
        let err = with_credential(entry).expect_err(entry);
        assert_eq!(err.rule, "unsupported-credential", "{entry}: {err}");
        assert!(err.msg.contains(reason), "{entry}: {err}");
        assert_eq!(err.target, "puppet");
        assert!(err.msg.starts_with("the credential \"deploy\""), "{err}");
    }
}

// ---- Puppet's own types beside the six -------------------------------------

/// The document behind the `native_types` golden files: Puppet's own types
/// beside the shared ones, a core type, a defined type, the native `file`
/// reached through `native/`, with a state, notified and notifying.
const NATIVE_TYPES: &str = "    - id: nginx_pkg\n      type: package\n      params:\n        name: nginx\n    - id: html\n      type: native/file\n      state: directory\n      params:\n        path: /var/www/html\n        source: puppet:///modules/web/html\n        recurse: true\n      dependsOn:\n        - resource: nginx_pkg\n    - id: logrotate\n      type: cron\n      state: present\n      params:\n        command: /usr/sbin/logrotate /etc/logrotate.conf\n        user: root\n        hour: 2\n        minute: 0\n      notifies:\n        - resource: nginx_svc\n          action: restart\n        - resource: app_db\n          action: refresh\n    - id: nginx_svc\n      type: service\n      params:\n        name: nginx\n    - id: app_db\n      type: mysql::db\n      params:\n        user: app\n        password: placeholder\n        host: localhost\n        grant: [SELECT, INSERT]\n    - id: tag\n      type: mysql::server::tag\n";

#[test]
fn a_puppet_type_is_declared_with_its_attributes_as_written() {
    let emitted = compile(NATIVE_TYPES).unwrap_or_else(|e| panic!("{e}"));
    assert_golden(&golden(), "native_types", &emitted);
    let manifest = &emitted.artifact.files["manifests/init.pp"];
    // The attributes are the type's own, as written, and the state is its
    // ensure, bare when it is a word.
    assert!(manifest.contains("  cron { 'logrotate':\n    ensure  => present,\n    command => '/usr/sbin/logrotate /etc/logrotate.conf',\n    hour    => 2,"), "{manifest}");
    // native/file is Puppet's file, with the attributes the shared type does
    // not take.
    assert!(manifest.contains("  file { 'html':\n    ensure  => directory,\n    path    => '/var/www/html',\n    recurse => true,\n    source  => 'puppet:///modules/web/html',\n    require => Package['nginx_pkg'],"), "{manifest}");
    // A reference to a defined type capitalises each segment.
    assert!(
        manifest.contains("notify  => [Mysql::Db['app_db'], Service['nginx_svc']],"),
        "{manifest}"
    );
    // A type with nothing to say is still a declaration.
    assert!(
        manifest.contains("  mysql::server::tag { 'tag':\n  }\n"),
        "{manifest}"
    );
}

#[test]
fn a_puppet_type_takes_its_ensure_once_and_a_value_that_is_not_a_word_is_quoted() {
    let err = refusal(
        "    - id: a\n      type: cron\n      state: absent\n      params:\n        command: /bin/true\n        ensure: present\n",
    );
    assert_eq!(err.rule, "param-conflicts-with-state");

    let err = refusal("    - id: a\n      type: cron\n      params:\n        command: ~\n");
    assert_eq!(err.rule, "empty-param");

    let manifest = manifest(
        "    - id: a\n      type: native/package\n      state: 1.2.3-1\n      params:\n        name: nginx\n        install_options: ['--no-install-recommends']\n",
    );
    assert!(
        manifest.contains("ensure          => '1.2.3-1',"),
        "{manifest}"
    );
    assert!(
        manifest.contains("install_options => ['--no-install-recommends'],"),
        "{manifest}"
    );
}

#[test]
fn a_name_that_is_not_a_puppet_type_is_refused() {
    for r#type in [
        "Cron",
        "mysql::",
        "ansible.builtin.cron",
        "native/cron",
        "native/File",
    ] {
        let err = refusal(&format!(
            "    - id: a\n      type: '{type}'\n      params:\n        command: /bin/true\n"
        ));
        assert_eq!(err.rule, "unsupported-resource-type", "{type}");
        assert!(
            err.msg.contains("mysql::db") || err.msg.contains("reaches only a Puppet type"),
            "{type}: {}",
            err.msg
        );
    }
}

#[test]
fn a_puppet_type_can_only_be_refreshed_when_notified() {
    let err = refusal(
        "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: job\n          action: restart\n    - id: job\n      type: cron\n      params: {command: /bin/true}\n",
    );
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "conf");
    assert_eq!(
        err.msg,
        "notifies resource \"job\" with action \"restart\", but cron is a Puppet type, which \
         can only be refreshed when notified (action \"refresh\")"
    );
}

#[test]
fn a_native_file_and_the_shared_file_managing_one_path_are_refused_together() {
    let err = refusal(
        "    - id: a\n      type: file\n      params: {path: /etc/motd, content: hi}\n    - id: b\n      type: native/file\n      params: {path: /etc/motd, source: puppet:///modules/web/motd}\n",
    );
    assert_eq!(err.rule, "duplicate-namevar");
    assert_eq!(err.resource, "b");
}

#[test]
fn a_secret_handed_to_a_puppet_type_is_a_sensitive_lookup() {
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: puppet\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n    - id: db\n      type: mysql::db\n      params:\n        user: app\n        password: placeholder\n";
    let mut graph = graph_from_text("site.yaml", text, "puppet");
    secret_in_param(&mut graph, "db", "password", "db_password");
    let manifest = emit(&graph).unwrap().artifact.files["manifests/init.pp"].clone();
    assert!(
        manifest.contains("password => Sensitive(lookup('meridian_secret_db_password')),"),
        "{manifest}"
    );
    assert!(!manifest.contains("placeholder"), "{manifest}");
}

#[test]
fn the_schemas_offer_any_puppet_type_as_an_open_pattern() {
    let schemas = Puppet.resource_schemas();
    for pattern in meridian_target_puppet::NATIVE_PATTERNS {
        let schema = schemas
            .iter()
            .find(|s| s.r#type == *pattern)
            .expect("the pattern is declared");
        assert!(schema.open && schema.is_pattern() && schema.params.is_empty());
        assert_eq!(schema.actions, ["refresh"]);
    }
    assert_eq!(
        Puppet.supported_resource_types(),
        [
            "<type>",
            "<module>::<type>",
            "exec",
            "file",
            "group",
            "package",
            "service",
            "user"
        ]
    );
}
