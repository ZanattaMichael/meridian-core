//! The Runner half of the protocol, and a plugin written in Rust.
//!
//! These tests use `meridian-target-fake`, a
//! plugin written in Rust against `meridian-sdk`. Its tool is a
//! directory, which makes plan and apply real: a plan is a comparison with
//! what the last apply left behind.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use meridian_plugin_host::{Client, EmitError, RunFailure, StartOptions, Stop, StopCause};
use meridian_sdk::{
    Artifact, Capabilities, Category, ChangeAction, Data, LogLine, LogStream, Operations, Position,
    Resource, ResourceGraph, RunContext, SecretName, SecretRef, SecretValue, Secrets, Value,
    PROTOCOL_VERSION, VERSION,
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
        secrets: Default::default(),
        credential: None,
        save_plan: false,
        saved_plan: None,
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
        secrets: Vec::new(),
        credential: None,
        settings: BTreeMap::new(),
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
        RunFailure::Stopped(e) => panic!("the run was stopped instead of the tool failing: {e}"),
    }
}

fn plugin_error(failure: RunFailure) -> meridian_plugin_host::Error {
    match failure {
        RunFailure::Plugin(e) => e,
        RunFailure::Stopped(e) => panic!("the run was stopped instead of the plugin failing: {e}"),
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
            secrets: true,
            credentials: true,
            cloud_credentials: true,
            adopt: meridian_sdk::Adoption::Import,
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
                 the fake target has no mapping for type \"firewall_rule\" (hint: use a type \
                 from the target's resource types; `meridian plugins` lists them)"
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

/// The run that was stopped, or a panic saying what happened instead.
fn stopped(failure: Result<meridian_sdk::Applied, RunFailure>) -> meridian_plugin_host::Stopped {
    match failure {
        Err(RunFailure::Stopped(stopped)) => stopped,
        Err(other) => panic!("the run failed instead of being stopped: {other}"),
        Ok(applied) => panic!("the run finished instead of being stopped: {applied:?}"),
    }
}

#[test]
fn a_run_past_its_time_limit_is_stopped_and_says_what_was_done() {
    let client = start();
    let context = context("timeout");
    // `a` is written before `b` hangs, so the stop reports one change.
    let artifact = artifact(&[("a", "first"), ("b", "HANG here")]);
    let started = Instant::now();
    let stopped = stopped(client.apply_with(
        &context,
        &artifact,
        &mut no_log(),
        &Stop::after(Duration::from_millis(300)),
    ));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the run was not stopped: {:?}",
        started.elapsed()
    );
    assert_eq!(
        stopped.cause,
        StopCause::TimedOut(Duration::from_millis(300))
    );
    assert!(stopped.answered);
    assert_eq!(
        changes(&stopped.changes),
        vec![(meridian_sdk::ChangeAction::Create, "a")]
    );
    assert_eq!(stopped.command, "fake-tool apply --password=***");
    assert_eq!(stopped.output, "stopped while waiting\n");
    let shown = stopped.to_string();
    assert!(
        shown.starts_with("timed out after 300ms (was running `fake-tool apply"),
        "{shown}"
    );

    // The plugin itself is fine: it goes on answering.
    let outputs = client.outputs(&context).unwrap();
    assert!(!outputs.is_empty());
}

#[test]
fn a_run_is_stopped_when_asked_and_a_limit_that_is_not_reached_does_nothing() {
    let client = start();
    let context = context("interrupt");
    let flag = Arc::new(AtomicBool::new(false));
    let asked = Arc::clone(&flag);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        asked.store(true, Ordering::SeqCst);
    });
    let stop = Stop {
        when: Some(flag),
        after: Some(Duration::from_secs(600)),
        ..Stop::default()
    };
    let stopped =
        stopped(client.apply_with(&context, &artifact(&[("a", "HANG")]), &mut no_log(), &stop));
    assert_eq!(stopped.cause, StopCause::Interrupted);
    assert!(stopped.changes.is_empty());
    assert!(
        stopped
            .to_string()
            .starts_with("interrupted: the run was stopped"),
        "{stopped}"
    );

    // A run that finishes within its limit is not touched.
    let applied = client
        .apply_with(
            &context,
            &artifact(&[("a", "fine")]),
            &mut no_log(),
            &Stop::after(Duration::from_secs(600)),
        )
        .unwrap();
    assert_eq!(
        changes(&applied.changes),
        vec![(meridian_sdk::ChangeAction::Create, "a")]
    );
}

/// The fake plugin runs a real child process that is stopped the way the SDK
/// stops every tool: the whole process group, politely and then by force.
#[cfg(unix)]
#[test]
fn a_stopped_run_stops_the_tools_process() {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("FAKE_HANG_TOOL".to_owned(), "1".to_owned()));
    let client = Client::start(
        &fake(),
        &StartOptions {
            env: Some(env),
            ..StartOptions::default()
        },
    )
    .expect("the fake plugin starts");
    let started = Instant::now();
    let stopped = stopped(client.apply_with(
        &context("hang-tool"),
        &artifact(&[("a", "HANG")]),
        &mut no_log(),
        &Stop::after(Duration::from_millis(200)),
    ));
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the tool was not stopped: {:?}",
        started.elapsed()
    );
    assert_eq!(
        stopped.cause,
        StopCause::TimedOut(Duration::from_millis(200))
    );
    assert!(stopped.answered);
    assert_eq!(stopped.command, "sh -c 'sleep 600'");
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

#[test]
fn a_secret_in_a_param_reaches_the_plugin_by_name_and_its_value_when_it_runs() {
    let sink = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("runner")
        .join("secret-sink.json");
    let _ = fs::remove_file(&sink);
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push((
        "FAKE_SECRET_SINK".to_owned(),
        sink.to_string_lossy().into_owned(),
    ));
    let client = Client::start(
        &fake(),
        &StartOptions {
            env: Some(env),
            ..StartOptions::default()
        },
    )
    .unwrap();
    assert!(client.info().capabilities.secrets);

    // Compiling: the param crosses the wire as a secret, and the plugin
    // writes a reference to it by the name the graph gives it.
    let slot = SecretRef::new(7);
    let mut conf = file("conf", "");
    conf.params
        .insert("content".to_owned(), Value::Secret(slot));
    let mut graph = graph(vec![conf]);
    graph.secrets.push(SecretName {
        secret: slot,
        name: "db_password".to_owned(),
    });
    let emitted = client.emit(&graph, &Data::new()).unwrap();
    assert_eq!(
        emitted.artifact.files["files/conf"],
        "<<secret db_password>>"
    );

    // Running: the value goes with the run, and the plugin hands it to its
    // tool in a private file, under the same name.
    let mut context = context("secret");
    context.secrets = Secrets::from_values(vec![(
        slot,
        "db_password".to_owned(),
        SecretValue::from_reader(&b"hunter2"[..], 64).unwrap(),
    )]);
    let mut printed = Vec::new();
    let applied = client
        .apply(&context, &emitted.artifact, &mut |line: LogLine| {
            printed.push(line.text)
        })
        .unwrap();
    assert_eq!(
        changes(&applied.changes),
        [(ChangeAction::Create, "files/conf")]
    );
    assert_eq!(
        fs::read_to_string(&sink).unwrap(),
        "{\"db_password\":\"hunter2\"}\n"
    );
    assert!(printed.contains(&"1 secret(s) handed over in a private file".to_owned()));
    let delivery = fs::read_to_string(context.work_dir.join("secrets-delivery.json")).unwrap();
    assert!(
        delivery.contains("\"names\":[\"db_password\"]"),
        "{delivery}"
    );
    assert!(!delivery.contains("hunter2"));
    assert_eq!(
        fs::read_to_string(context.work_dir.join("current/files/conf")).unwrap(),
        "<<secret db_password>>"
    );

    // Reading outputs needs no secret and is sent none.
    let _ = fs::remove_file(&sink);
    client.outputs(&context).unwrap();
    assert!(!sink.exists());
    client.close().unwrap();
}

#[test]
fn a_credential_reaches_the_plugin_by_name_when_it_compiles_and_by_value_when_it_runs() {
    let sink = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("runner")
        .join("credential-sink.json");
    let _ = fs::remove_file(&sink);
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push((
        "FAKE_CREDENTIAL_SINK".to_owned(),
        sink.to_string_lossy().into_owned(),
    ));
    let client = Client::start(
        &fake(),
        &StartOptions {
            env: Some(env),
            ..StartOptions::default()
        },
    )
    .unwrap();
    assert!(client.info().capabilities.credentials);

    // Compiling: the graph names the credential and its secret fields, and
    // the plugin can put that in its artifact, since there is no value in it.
    let declared = meridian_sdk::Credential {
        name: "deploy".to_owned(),
        kind: "ssh".to_owned(),
        ambient: false,
        text: BTreeMap::from([("user".to_owned(), "deploy".to_owned())]),
        flags: BTreeMap::new(),
        secret_fields: vec!["privateKey".to_owned()],
    };
    let mut graph = graph(vec![file("conf", "x")]);
    graph.credential = Some(declared.clone());
    let emitted = client.emit(&graph, &Data::new()).unwrap();
    assert_eq!(
        emitted.artifact.files["credential.txt"],
        "deploy (ssh, user deploy)\nsecret fields: privateKey\n"
    );

    // Running: the values go with the run, and the plugin hands them to its
    // tool in a private file, recording what it did and never a value.
    let mut context = context("credential");
    context.credential = Some(meridian_sdk::HeldCredential::from_values(
        declared,
        vec![(
            "privateKey".to_owned(),
            SecretValue::from_reader(&b"-----KEY-----"[..], 64).unwrap(),
        )],
    ));
    let mut printed = Vec::new();
    client
        .plan(&context, &emitted.artifact, &mut |line: LogLine| {
            printed.push(line.text)
        })
        .unwrap();
    assert_eq!(
        fs::read_to_string(&sink).unwrap(),
        "{\"privateKey\":\"-----KEY-----\"}\n"
    );
    assert!(printed.contains(
        &"credential deploy (ssh, user deploy) handed over in a private file".to_owned()
    ));
    let delivery = fs::read_to_string(context.work_dir.join("credential-delivery.json")).unwrap();
    for part in [
        "\"name\":\"deploy\"",
        "\"type\":\"ssh\"",
        "\"secretFields\":[\"privateKey\"]",
        "\"text\":{\"user\":\"deploy\"}",
        "\"channel\":\"file\"",
    ] {
        assert!(delivery.contains(part), "{delivery} lacks {part}");
    }
    assert!(!delivery.contains("KEY"), "{delivery}");
    assert!(!printed.iter().any(|l| l.contains("KEY")), "{printed:?}");
    // The private file is gone once the call is over.
    let private = context.work_dir.join("credential.json");
    assert!(!private.exists());

    // Apply gets it too; reading outputs needs none and is sent none.
    let _ = fs::remove_file(&sink);
    client
        .apply(&context, &emitted.artifact, &mut no_log())
        .unwrap();
    assert!(sink.exists());
    let _ = fs::remove_file(&sink);
    context.credential = None;
    client.outputs(&context).unwrap();
    assert!(!sink.exists());
    client.close().unwrap();
}

#[test]
fn a_plugin_that_takes_no_secrets_takes_no_credential_either() {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("FAKE_SECRETS".to_owned(), "none".to_owned()));
    let client = Client::start(
        &fake(),
        &StartOptions {
            env: Some(env),
            ..StartOptions::default()
        },
    )
    .unwrap();
    assert!(!client.info().capabilities.secrets);
    assert!(!client.info().capabilities.credentials);
    client.close().unwrap();
}

/// The project's settings reach the plugin through its environment, under
/// the setting's short name, and an environment variable still wins. A
/// setting the plugin does not declare stops it from starting, with the
/// names it does declare.
#[test]
fn project_settings_reach_the_plugin_and_an_unknown_one_is_refused() {
    let project = |pairs: &[(&str, &str)]| {
        let mut settings = BTreeMap::new();
        settings.insert(
            "fake".to_owned(),
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<String, String>>(),
        );
        settings
    };
    let apply_and_read = |client: &Client, test: &str| {
        let context = context(test);
        client
            .apply(&context, &artifact(&[("a.txt", "x")]), &mut |_| {})
            .expect("the fake applies");
        fs::read_to_string(context.work_dir.join("greeting.txt")).unwrap()
    };

    let client = start();
    assert!(client.info().settings.iter().any(|s| s.name == "greeting"));
    assert_eq!(apply_and_read(&client, "settings-default"), "hello");

    let client = Client::start(
        &fake(),
        &StartOptions {
            settings: project(&[("greeting", "good day")]),
            ..StartOptions::default()
        },
    )
    .expect("a declared setting starts the plugin");
    assert_eq!(apply_and_read(&client, "settings-project"), "good day");

    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("FAKE_GREETING".to_owned(), "from the shell".to_owned()));
    let client = Client::start(
        &fake(),
        &StartOptions {
            env: Some(env),
            settings: project(&[("greeting", "good day")]),
            ..StartOptions::default()
        },
    )
    .expect("the plugin starts");
    assert_eq!(
        apply_and_read(&client, "settings-env-wins"),
        "from the shell"
    );

    let refused = Client::start(
        &fake(),
        &StartOptions {
            settings: project(&[("greeting", "x"), ("colour", "blue")]),
            ..StartOptions::default()
        },
    )
    .expect_err("an undeclared setting is refused");
    let text = refused.to_string();
    assert!(text.contains("the plugin was not started"), "{text}");
    assert!(
        text.contains("meridian.yaml sets colour for target \"fake\" under spec.settings"),
        "{text}"
    );
    assert!(text.contains("it declares greeting"), "{text}");
}
