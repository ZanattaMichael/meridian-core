//! The Runner half of the protocol, and a plugin written in Rust.
//!
//! These tests use `meridian-target-fake`, a
//! plugin written in Rust against `meridian-sdk`. Its tool is a
//! directory, which makes plan and apply real: a plan is a comparison with
//! what the last apply left behind.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use meridian_plugin_host::{Client, EmitError, RunFailure, StartOptions};
use meridian_sdk::{
    Artifact, Capabilities, Category, ChangeAction, Data, LogLine, LogStream, Operations, Position,
    Resource, ResourceGraph, RunContext, Value, PROTOCOL_VERSION, VERSION,
};

fn fake() -> String {
    meridian_target_fake::plugin_path()
        .to_string_lossy()
        .into_owned()
}

fn start() -> Client {
    Client::start(&fake(), &StartOptions::default()).expect("the fake plugin starts")
}

fn start_offering(operations: &str) -> Client {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("FAKE_OPERATIONS".to_owned(), operations.to_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    Client::start(&fake(), &options).expect("the fake plugin starts")
}

/// A fresh work directory per test, so they can run side by side.
fn context(test: &str) -> RunContext {
    let work_dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("runner")
        .join(test);
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir).unwrap();
    RunContext {
        document: "web".to_owned(),
        host: "web01.example.com".to_owned(),
        work_dir,
        inputs: Data::new(),
    }
}

fn artifact(files: &[(&str, &str)]) -> Artifact {
    Artifact {
        files: files
            .iter()
            .map(|(path, content)| ((*path).to_owned(), (*content).to_owned()))
            .collect(),
    }
}

fn file(id: &str, content: &str) -> Resource {
    Resource {
        id: id.to_owned(),
        r#type: "file".to_owned(),
        params: [("content".to_owned(), Value::from(content))]
            .into_iter()
            .collect(),
        ..Resource::default()
    }
}

fn graph(resources: Vec<Resource>) -> ResourceGraph {
    ResourceGraph {
        name: "web".to_owned(),
        host: "web01.example.com".to_owned(),
        target: "fake".to_owned(),
        target_version: String::new(),
        resources,
        edges: Vec::new(),
        outputs: Vec::new(),
    }
}

fn changes(list: &[meridian_sdk::Change]) -> Vec<(ChangeAction, &str)> {
    list.iter()
        .map(|c| (c.action, c.resource.as_str()))
        .collect()
}

fn no_log() -> impl FnMut(LogLine) {
    |_| {}
}

fn tool_error(failure: RunFailure) -> meridian_sdk::RunError {
    match failure {
        RunFailure::Tool(e) => e,
        RunFailure::Plugin(e) => panic!("the plugin failed instead of the tool: {e}"),
    }
}

fn plugin_error(failure: RunFailure) -> meridian_plugin_host::Error {
    match failure {
        RunFailure::Plugin(e) => e,
        RunFailure::Tool(e) => panic!("the tool failed instead of the plugin: {e}"),
    }
}

#[test]
fn a_rust_plugin_describes_its_category_and_operations() {
    let client = start();
    let info = client.info();
    assert_eq!(info.name, "fake");
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.sdk_version, VERSION);
    assert_eq!(info.resource_types, ["file"]);
    assert_eq!(
        info.capabilities,
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
        }
    );
    assert_eq!(info.category, Category::Containerization);
    assert_eq!(info.target_versions, ["1", "2.4"]);
    assert_eq!(info.operations, Operations::ALL);
    client
        .close()
        .expect("a Rust plugin exits when its stdin closes");

    assert_eq!(
        start_offering("plan").info().operations,
        Operations {
            plan: true,
            apply: false,
            outputs: false,
        }
    );
    assert_eq!(
        start_offering("none").info().operations,
        Operations::COMPILE_ONLY
    );
}

#[test]
fn a_rust_plugin_compiles_warns_and_refuses() {
    let client = start();
    let mut gone = file("old", "");
    gone.state = "absent".to_owned();
    let data: Data = [
        ("port".to_owned(), Value::Int(8080)),
        ("ratio".to_owned(), Value::Float(0.5)),
        ("tag".to_owned(), Value::from("<a&b>")),
    ]
    .into_iter()
    .collect();

    let emitted = client
        .emit(&graph(vec![file("motd", "hello\n"), gone]), &data)
        .expect("the document compiles");
    let want: BTreeMap<String, String> = [
        ("files/motd".to_owned(), "hello\n".to_owned()),
        // Integers stay integers across the boundary, in both directions.
        (
            "data.json".to_owned(),
            "{\"port\":8080,\"ratio\":0.5,\"tag\":\"\\u003ca\\u0026b\\u003e\"}".to_owned(),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(emitted.artifact.files, want);
    assert_eq!(emitted.warnings.len(), 1);
    assert_eq!(emitted.warnings[0].resource, "old");

    let mut odd = file("fw", "");
    odd.r#type = "firewall_rule".to_owned();
    odd.pos = Position {
        file: "site.yaml".to_owned(),
        doc_idx: 0,
        line: 7,
        column: 3,
    };
    match client.emit(&graph(vec![odd]), &Data::new()) {
        Err(EmitError::Refused(e)) => {
            assert_eq!(
                e.to_string(),
                "site.yaml:7:3: fake: resource \"fw\" [unsupported-resource-type]: \
                 the fake target has no mapping for type \"firewall_rule\""
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn plan_apply_and_outputs_follow_the_tools_state() {
    let client = start();
    let context = context("lifecycle");
    let first = artifact(&[("files/a", "one"), ("files/b", "two")]);

    // Nothing applied yet: everything is to be created, and reading outputs
    // is the tool's failure, not the plugin's.
    let plan = client.plan(&context, &first, &mut no_log()).unwrap();
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Create, "files/a"),
            (ChangeAction::Create, "files/b")
        ]
    );
    assert!(!plan.in_sync());
    let err = tool_error(client.outputs(&context).unwrap_err());
    assert_eq!(err.to_string(), "nothing has been applied yet");
    assert!(
        !context.work_dir.join("state.json").exists(),
        "a plan must change nothing"
    );

    let mut lines = Vec::new();
    let applied = client
        .apply(&context, &first, &mut |line| lines.push(line))
        .unwrap();
    assert_eq!(changes(&applied.changes), changes(&plan.changes));
    assert_eq!(
        lines,
        [
            LogLine {
                stream: LogStream::Stdout,
                text: "create files/a".to_owned()
            },
            LogLine {
                stream: LogStream::Stdout,
                text: "create files/b".to_owned()
            },
        ]
    );
    assert_eq!(
        fs::read_to_string(context.work_dir.join("current/files/b")).unwrap(),
        "two"
    );

    // Outputs keep their types, and a sensitive one stays marked and does
    // not print its value.
    let outputs = &applied.outputs;
    assert_eq!(outputs[0].name, "file_count");
    assert_eq!(outputs[0].value, Value::Int(2));
    assert!(!outputs[0].sensitive);
    assert_eq!(outputs[1].name, "token");
    assert!(outputs[1].sensitive);
    assert_eq!(outputs[1].value, Value::from("s3cret-for-the-next-stage"));
    assert!(!format!("{outputs:?}").contains("s3cret"));
    assert_eq!(client.outputs(&context).unwrap(), applied.outputs);

    // Applied and unchanged: in sync, and applying again does nothing.
    assert!(client
        .plan(&context, &first, &mut no_log())
        .unwrap()
        .in_sync());
    let again = client.apply(&context, &first, &mut no_log()).unwrap();
    assert!(again.changes.is_empty());

    // One file edited, one dropped, one added.
    let second = artifact(&[("files/a", "ONE"), ("files/c", "three")]);
    let mut lines = Vec::new();
    let plan = client
        .plan(&context, &second, &mut |line| lines.push(line))
        .unwrap();
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Update, "files/a"),
            (ChangeAction::Create, "files/c"),
            (ChangeAction::Delete, "files/b"),
        ]
    );
    assert_eq!(plan.changes[0].to_string(), "update files/a: 3 bytes");
    assert_eq!(
        lines,
        [
            LogLine {
                stream: LogStream::Stdout,
                text: "comparing 2 files for web on web01.example.com".to_owned()
            },
            LogLine {
                stream: LogStream::Stderr,
                text: "3 to change".to_owned()
            },
        ]
    );
    client.apply(&context, &second, &mut no_log()).unwrap();
    assert!(!context.work_dir.join("current/files/b").exists());
}

#[test]
fn inputs_from_an_earlier_stage_reach_the_runner() {
    let client = start();
    let mut context = context("inputs");
    context
        .inputs
        .insert("greeting".to_owned(), Value::from("hello from terraform"));
    context.inputs.insert("port".to_owned(), Value::Int(443));

    let applied = client
        .apply(&context, &artifact(&[("files/a", "x")]), &mut no_log())
        .unwrap();
    let greeting = applied
        .outputs
        .iter()
        .find(|o| o.name == "greeting")
        .unwrap();
    assert_eq!(greeting.value, Value::from("hello from terraform"));
}

#[test]
fn a_plan_that_cannot_say_is_not_reported_as_in_sync() {
    let client = start();
    let context = context("unknown");
    let a = artifact(&[("files/a", "UNKNOWN")]);
    client.apply(&context, &a, &mut no_log()).unwrap();
    let plan = client.plan(&context, &a, &mut no_log()).unwrap();
    assert!(plan.changes.is_empty());
    assert!(plan.unknown);
    assert!(!plan.in_sync());
}

#[test]
fn a_failing_tool_is_an_answer_that_says_what_was_already_done() {
    let client = start();
    let context = context("tool-failure");
    let a = artifact(&[
        ("files/a", "fine"),
        ("files/b", "FAIL here"),
        ("files/c", "never"),
    ]);

    let err = tool_error(client.apply(&context, &a, &mut no_log()).unwrap_err());
    assert_eq!(err.msg, "could not write files/b");
    assert_eq!(err.exit_code, Some(3));
    assert_eq!(err.command, "fake-tool apply --password=***");
    assert_eq!(changes(&err.changes), [(ChangeAction::Create, "files/a")]);
    assert_eq!(
        err.to_string(),
        "could not write files/b (ran `fake-tool apply --password=***`, exit status 3)\ndisk on fire"
    );

    // The plugin is fine and the tool's state is consistent: the next plan
    // shows exactly what is left.
    let plan = client.plan(&context, &a, &mut no_log()).unwrap();
    assert_eq!(
        changes(&plan.changes),
        [
            (ChangeAction::Create, "files/b"),
            (ChangeAction::Create, "files/c")
        ]
    );
}

#[test]
fn a_panicking_runner_fails_the_call_and_not_the_plugin() {
    let client = start();
    let context = context("panic");
    let failure = client
        .apply(&context, &artifact(&[("files/a", "PANIC")]), &mut no_log())
        .unwrap_err();
    let err = plugin_error(failure);
    assert_eq!(err.plugin, "fake");
    assert_eq!(err.msg, "the plugin failed while applying");
    assert!(err.cause.contains("the runner panicked"), "{err}");

    let plan = client
        .plan(&context, &artifact(&[("files/a", "ok")]), &mut no_log())
        .expect("the plugin still answers");
    assert_eq!(changes(&plan.changes), [(ChangeAction::Create, "files/a")]);
}

#[test]
fn a_plugin_that_dies_mid_run_is_a_plugin_failure_with_its_last_words() {
    let client = start();
    let context = context("death");
    let mut lines = Vec::new();
    let failure = client
        .apply(
            &context,
            &artifact(&[("files/a", "fine"), ("files/b", "EXIT")]),
            &mut |line| lines.push(line.text),
        )
        .unwrap_err();
    let err = plugin_error(failure);
    assert_eq!(err.msg, "the plugin failed while applying");
    assert_eq!(err.stderr, "the fake plugin was told to die");
    // Lines the plugin had not sent yet die with it; any that arrived are
    // the tool's own, in order.
    assert!(lines.len() <= 2, "{lines:?}");
    if let Some(first) = lines.first() {
        assert_eq!(first, "create files/a");
    }
}

#[test]
fn a_plugin_is_never_asked_for_an_operation_it_did_not_declare() {
    let plan_only = start_offering("plan");
    let context = context("plan-only");
    let a = artifact(&[("files/a", "x")]);

    plan_only
        .plan(&context, &a, &mut no_log())
        .expect("it plans");
    let err = plugin_error(plan_only.apply(&context, &a, &mut no_log()).unwrap_err());
    assert_eq!(
        err.to_string(),
        "plugin \"fake\": this target does not apply"
    );
    let err = plugin_error(plan_only.outputs(&context).unwrap_err());
    assert_eq!(
        err.to_string(),
        "plugin \"fake\": this target does not report outputs"
    );
    assert!(
        !context.work_dir.join("state.json").exists(),
        "the refused apply never reached the plugin"
    );

    let compile_only = start_offering("none");
    let err = plugin_error(compile_only.plan(&context, &a, &mut no_log()).unwrap_err());
    assert_eq!(
        err.to_string(),
        "plugin \"fake\": this target does not plan"
    );
    compile_only
        .emit(&graph(vec![file("a", "x")]), &Data::new())
        .expect("it still compiles");
}

// Unix only: on Windows a plugin listens on a named pipe, which is not a file
// and is gone when the plugin is, so there is nothing it could leave behind.
#[cfg(unix)]
#[test]
fn a_rust_plugin_leaves_nothing_behind_when_it_exits() {
    // The plugin is given a temporary directory of its own, so what it leaves
    // there can be told apart from every other test's plugins. It is under
    // /tmp and not the system's temporary directory, which on macOS is too
    // deep for a socket: the plugin would then, rightly, not use it.
    let tmp = Path::new("/tmp").join(format!("meridian-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k != "TMPDIR").collect();
    env.push(("TMPDIR".to_owned(), tmp.to_string_lossy().into_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let entries = || fs::read_dir(&tmp).unwrap().count();

    let client = Client::start(&fake(), &options).expect("the fake plugin starts");
    assert_eq!(entries(), 1, "a running plugin has one socket directory");
    client.close().expect("the plugin exits when asked");
    assert_eq!(entries(), 0, "the socket directory was not removed");
    let _ = fs::remove_dir_all(&tmp);
}

/// A temporary directory can be too deep for a unix socket's path, which has
/// a small fixed limit. The plugin has to serve anyway.
#[test]
fn a_rust_plugin_serves_when_the_temporary_directory_is_too_deep_for_a_socket() {
    let deep = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("a".repeat(60))
        .join("b".repeat(60));
    fs::create_dir_all(&deep).unwrap();
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k != "TMPDIR").collect();
    env.push(("TMPDIR".to_owned(), deep.to_string_lossy().into_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let client = Client::start(&fake(), &options).expect("the plugin falls back to a short path");
    assert_eq!(client.name(), "fake");
    client.close().unwrap();
}

#[test]
fn an_import_crosses_the_boundary_whole_and_a_plugin_without_one_is_not_asked() {
    use meridian_plugin_host::ImportFailure;
    use meridian_sdk::{DocumentKind, ImportSource, NoteLevel};

    let client = start();
    assert_eq!(client.info().import_formats.len(), 1);
    assert_eq!(client.info().import_formats[0].patterns, ["*.fake"]);

    let source = ImportSource {
        files: [(
            "files.fake".to_owned(),
            "motd = welcome\n? a link\n".to_owned(),
        )]
        .into_iter()
        .collect(),
        name: "legacy".to_owned(),
        root: "/srv/legacy".to_owned(),
    };
    let imported = client.import(&source).expect("the import succeeds");
    let document = &imported.documents[0];
    assert_eq!(document.kind, DocumentKind::Infrastructure);
    assert_eq!(document.name, "legacy");
    // The plugin left the target to the host, which knows who it asked.
    assert_eq!(document.target, "fake");
    assert_eq!(document.resources[0].id, "motd");
    assert_eq!(document.resources[0].origin, "files.fake:1");
    assert_eq!(
        document.resources[0].params["content"],
        Value::from("welcome")
    );
    assert_eq!(imported.notes.len(), 1);
    assert_eq!(imported.notes[0].level, NoteLevel::Untranslated);
    assert_eq!(imported.notes[0].origin, "files.fake:2");

    // A source the importer cannot read is the source's fault, with its place.
    let broken = ImportSource {
        files: [("files.fake".to_owned(), "BROKEN\n".to_owned())]
            .into_iter()
            .collect(),
        ..ImportSource::default()
    };
    match client.import(&broken) {
        Err(ImportFailure::Source(e)) => {
            assert_eq!((e.file.as_str(), e.line), ("files.fake", 1));
        }
        other => panic!("expected the source to be refused, got {other:?}"),
    }

    // A plugin that offers no import says so in Describe and is never asked.
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("FAKE_IMPORT".to_owned(), "none".to_owned()));
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let plain = Client::start(&fake(), &options).expect("the fake plugin starts");
    assert!(plain.info().import_formats.is_empty());
    match plain.import(&source) {
        Err(ImportFailure::Plugin(e)) => assert!(e.msg.contains("does not import"), "{e}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}
