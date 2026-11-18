//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_template::Template;
use meridian_plugin_testkit::{
    assert_golden, assert_golden_refusal, graph_from_file, graph_from_text,
};
use meridian_sdk::{CompileError, Data, Emitted, Emitter, ResourceGraph};

/// The example document, by its path from the repository root.
const EXAMPLE: &str = "crates/meridian-plugin-template/example/site.yaml";

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Template.emit(graph, &Data::new())
}

/// A document holding the given resources, written as YAML list items.
fn document(resources: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: template\n  targetHost:\n    from: inline\n    host: web01\n  resources:\n{resources}"
    )
}

fn compile(resources: &str) -> Result<Emitted, CompileError> {
    emit(&graph_from_text(
        "site.yaml",
        &document(resources),
        "template",
    ))
}

fn refusal(resources: &str) -> CompileError {
    compile(resources).expect_err("the document should be refused")
}

const MOTD: &str = "    - id: motd\n      type: file\n      params:\n        path: /etc/motd\n";

// The golden files are the plugin's output, reviewed like code. Record them
// with `UPDATE_GOLDEN=1 cargo test` after a deliberate change, and read the
// diff before committing it.
#[test]
fn the_example_compiles_to_its_golden_files() {
    let graph = graph_from_file(EXAMPLE, "template");
    assert_golden(&golden(), "site", &emit(&graph).unwrap());
}

// A refusal is output too: its wording is what an operator reads. This one
// is a shared fixture, which every plugin compiles or refuses.
#[test]
fn a_shared_fixture_the_tool_cannot_express_is_refused_in_recorded_words() {
    let graph = graph_from_file("fixtures/web_stack.yaml", "template");
    assert_golden_refusal(&golden(), "web_stack", &emit(&graph).unwrap_err());
}

#[test]
fn compiling_twice_gives_the_same_bytes() {
    let graph = graph_from_file(EXAMPLE, "template");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn a_type_the_tool_does_not_have_is_refused_at_the_authors_line() {
    let err = refusal("    - id: nginx\n      type: package\n      params:\n        name: nginx\n");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(err.resource, "nginx");
    assert_eq!(err.pos.line, 11);
}

#[test]
fn a_param_the_tool_does_not_take_is_refused_not_dropped() {
    let err = refusal(&format!("{MOTD}        owner: root\n"));
    assert_eq!(err.rule, "unsupported-param");
    assert!(err.msg.contains("\"owner\""), "{}", err.msg);
}

#[test]
fn a_file_needs_a_path_and_an_absolute_one() {
    let err = refusal("    - id: motd\n      type: file\n      params:\n        content: hello\n");
    assert_eq!(err.rule, "missing-param");

    let err = refusal("    - id: motd\n      type: file\n      params:\n        path: etc/motd\n");
    assert_eq!(err.rule, "invalid-path");
}

#[test]
fn a_reference_to_another_resource_is_settled_when_compiling() {
    let emitted = compile(&format!(
        "{MOTD}    - id: copy\n      type: file\n      params:\n        path: /etc/motd.copy\n        content: \"see ${{motd.path}}\"\n"
    ))
    .unwrap();
    assert_eq!(emitted.artifact.files["content/copy"], "see /etc/motd");
}

#[test]
fn a_reference_to_nothing_is_refused_not_written_out_as_text() {
    let err = refusal(&format!("{MOTD}        content: \"${{nobody.path}}\"\n"));
    assert_eq!(err.rule, "invalid-reference");
}

#[test]
fn two_resources_cannot_manage_one_path() {
    let err = refusal(&format!(
        "{MOTD}    - id: again\n      type: file\n      params:\n        path: /etc/motd\n"
    ));
    assert_eq!(err.rule, "duplicate-resource");
    assert!(err.msg.contains("\"motd\""), "{}", err.msg);
}

#[test]
fn a_notify_is_refused_because_the_tool_cannot_react_to_a_change() {
    let err = refusal(&format!(
        "{MOTD}      notifies:\n        - resource: other\n          action: restart\n    - id: other\n      type: file\n      params:\n        path: /etc/other\n"
    ));
    assert_eq!(err.rule, "unsupported-notify");
    assert_eq!(err.resource, "motd");
}

#[test]
fn a_state_the_tool_does_not_have_is_refused() {
    let err = refusal(&format!("{MOTD}      state: directory\n"));
    assert_eq!(err.rule, "unsupported-state");
}

#[test]
fn an_absent_file_is_a_line_with_no_content() {
    let emitted = compile(&format!("{MOTD}      state: absent\n")).unwrap();
    assert_eq!(
        emitted.artifact.files["manifest.tsv"],
        "absent\t/etc/motd\n"
    );
    assert_eq!(emitted.artifact.files.len(), 1);
}
