//! The compile half, without a process boundary.
//!
//! OpenVox reads the language Puppet reads, so this plugin compiles with the
//! Puppet plugin's stages. Three things are held here: the output is recorded
//! in golden files of this plugin's own, it never drifts from what the Puppet
//! plugin emits, and everything a person reads names the target they chose.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{
    assert_golden, assert_golden_refusal, fixture, graph_from_file, graph_from_text,
    secret_in_param, without_positions,
};
use meridian_sdk::{Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_openvox::{OpenVox, MANIFEST_PATH, NAME, SITE_PATH};
use meridian_target_puppet::Puppet;

const FIXTURES: &[&str] = &[
    "accounts",
    "restart_chain",
    "runtime_condition",
    "web_stack",
];
/// The fixtures the language cannot express, and says so.
const REFUSED: &[&str] = &["runtime_condition", "web_stack"];
/// The documents the user documentation shows, compiled as written.
const EXAMPLES: &[&str] = &["web", "local"];

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    OpenVox.emit(graph, &Data::new())
}

fn example(name: &str) -> ResourceGraph {
    graph_from_file(&format!("examples/openvox/{name}.yaml"), NAME)
}

fn compile(resources: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: openvox\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    );
    emit(&graph_from_text("site.yaml", &text, NAME))
}

#[test]
fn the_plugin_describes_itself_as_openvox() {
    assert_eq!(OpenVox.name(), "openvox");
    assert_eq!(OpenVox.category(), Category::ConfigurationManagement);
    assert_eq!(
        OpenVox.capabilities(),
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: false,
            secrets: true,
            credentials: false,
        }
    );
    assert_eq!(
        OpenVox.supported_resource_types(),
        ["exec", "file", "group", "package", "service", "user"]
    );
}

#[test]
fn every_fixture_compiles_or_is_refused_as_recorded() {
    for name in FIXTURES {
        let graph = without_positions(fixture(name, NAME));
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
fn the_documented_examples_compile_to_what_the_documentation_shows() {
    for name in EXAMPLES {
        let emitted = emit(&example(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(emitted.warnings.is_empty(), "{name}");
        assert_golden(&golden(), &format!("example_{name}"), &emitted);
    }
}

#[test]
fn the_output_is_what_the_puppet_plugin_emits_byte_for_byte() {
    let graphs = FIXTURES
        .iter()
        .map(|name| (*name, fixture(name, NAME)))
        .chain(EXAMPLES.iter().map(|name| (*name, example(name))));
    for (name, graph) in graphs {
        match (emit(&graph), Puppet.emit(&graph, &Data::new())) {
            (Ok(ours), Ok(theirs)) => {
                assert_eq!(ours.artifact, theirs.artifact, "{name}");
                assert_eq!(ours.warnings, theirs.warnings, "{name}");
            }
            (Err(ours), Err(theirs)) => {
                // The same refusal in every respect but the target it names.
                assert_eq!(ours.target, "openvox", "{name}");
                assert_eq!(theirs.target, "puppet", "{name}");
                assert_eq!(
                    CompileError {
                        target: theirs.target.clone(),
                        ..ours
                    },
                    theirs,
                    "{name}"
                );
            }
            (ours, theirs) => panic!("{name}: openvox gave {ours:?}, puppet gave {theirs:?}"),
        }
    }
}

#[test]
fn compiling_twice_gives_the_same_bytes() {
    for name in EXAMPLES {
        let graph = example(name);
        assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap(), "{name}");
    }
}

#[test]
fn an_artifact_is_the_class_and_the_node_statement_that_includes_it() {
    let emitted = emit(&example("web")).unwrap();
    let paths: Vec<&str> = emitted.artifact.files.keys().map(String::as_str).collect();
    assert_eq!(paths, [MANIFEST_PATH, SITE_PATH]);
    assert!(emitted.artifact.files[MANIFEST_PATH].contains("class meridian_web {"));
    assert_eq!(
        emitted.artifact.files[SITE_PATH]
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect::<Vec<_>>(),
        ["node 'web01.example.com' {", "  include meridian_web", "}"]
    );
}

#[test]
fn a_refusal_names_openvox_and_keeps_where_the_author_wrote_it() {
    let graph = fixture("web_stack", NAME);
    let err = emit(&graph).unwrap_err();
    assert_eq!(err.target, "openvox");
    assert_eq!(err.resource, "nginx_site");
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.pos.file, "fixtures/web_stack.yaml");
    assert!(err.pos.line >= graph.resource("nginx_site").unwrap().pos.line);
    assert!(
        err.to_string().starts_with("fixtures/web_stack.yaml:"),
        "{err}"
    );
    assert!(
        err.to_string().contains("openvox: resource \"nginx_site\""),
        "{err}"
    );
}

#[test]
fn what_the_language_cannot_say_is_refused_not_dropped() {
    let cases: &[(&str, &str)] = &[
        (
            "unsupported-state",
            "    - id: svc\n      type: service\n      state: restarted\n      params: {name: app}\n",
        ),
        (
            "unsupported-action",
            "    - id: conf\n      type: file\n      params: {path: /a, content: a}\n      notifies:\n        - resource: pkg\n          action: restart\n    - id: pkg\n      type: package\n      params: {name: app}\n",
        ),
        (
            "duplicate-namevar",
            "    - id: a\n      type: package\n      params: {name: nginx}\n    - id: b\n      type: package\n      state: latest\n      params: {name: nginx}\n",
        ),
    ];
    for (rule, resources) in cases {
        let err = compile(resources).expect_err(rule);
        assert_eq!(&err.rule, rule);
        assert_eq!(err.target, "openvox", "{rule}");
    }
}

#[test]
fn a_notify_orders_as_well_as_refreshes_and_a_dependency_becomes_a_require() {
    let text = emit(&example("web")).unwrap().artifact.files[MANIFEST_PATH].clone();
    assert!(text.contains("notify  => Service['nginx_svc'],"), "{text}");
    assert!(text.contains("require => Package['nginx_pkg'],"), "{text}");
    assert!(text.contains("require => User['app_user'],"), "{text}");
    assert!(text.contains("creates => '/srv/app/public',"), "{text}");
}

#[test]
fn a_reference_is_settled_from_the_document_or_refused_as_openvox() {
    let text = compile(
        "    - id: root\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: page\n      type: file\n      params:\n        path: ${root.path}/index.html\n        content: hello\n",
    )
    .unwrap()
    .artifact
    .files[MANIFEST_PATH]
        .clone();
    assert!(text.contains("path    => '/srv/app/index.html',"), "{text}");

    let err = compile(
        "    - id: page\n      type: file\n      params:\n        path: /srv/index.html\n        content: ${nowhere.path}\n",
    )
    .expect_err("an unknown resource is refused");
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.target, "openvox");
    assert_eq!(err.resource, "page");
}

#[test]
fn a_secret_compiles_to_the_same_sensitive_lookup_as_puppet() {
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: openvox\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n    - id: env_file\n      type: file\n      params: {path: /etc/app/env, content: placeholder, mode: \"0600\"}\n";
    let mut graph = graph_from_text("site.yaml", text, NAME);
    secret_in_param(&mut graph, "env_file", "content", "db_password");
    let ours = emit(&graph).unwrap();
    let theirs = Puppet.emit(&graph, &Data::new()).unwrap();
    assert_eq!(ours.artifact.files, theirs.artifact.files);
    assert!(
        ours.artifact.files[MANIFEST_PATH]
            .contains("content => Sensitive(lookup('meridian_secret_db_password')),"),
        "{}",
        ours.artifact.files[MANIFEST_PATH]
    );
}
