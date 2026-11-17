//! The plugin as the host meets it: a separate process, found by its file
//! name and driven over the protocol.

use std::fs;
use std::path::Path;

use meridian_plugin_host::{binary_name, Client, Registry, RunFailure, StartOptions};
use meridian_plugin_testkit::{fixture, plugin_binary, scratch, stand_in, without_positions};
use meridian_sdk::{Capabilities, Category, Data, LogLine, Operations, RunContext};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const PACKAGE: &str = "meridian-target-openvox";

/// Starts the real plugin binary with an environment of its own, so the
/// machine's settings and whatever `puppet` it has are out of the picture.
fn start(dir: &Path, facts: &str, apply: &str, settings: &[(&str, &str)]) -> Client {
    let puppet = stand_in(
        dir,
        "puppet",
        &format!(
            "echo \"$@\" >> \"{}/puppet.args\"\nif [ \"$1\" = facts ]; then\n{facts}\nexit $?\nfi\n{apply}",
            dir.display()
        ),
    );
    let mut env = vec![
        ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        (
            "MERIDIAN_OPENVOX".to_owned(),
            puppet.to_str().unwrap().to_owned(),
        ),
    ];
    env.extend(
        settings
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    );
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    Client::start(plugin_binary(PACKAGE).to_str().unwrap(), &options)
        .unwrap_or_else(|e| panic!("starting the plugin: {e}"))
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web".into(),
        host: "localhost".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

const OPENVOX: &str = r#"echo '{"implementation":"openvox"}'"#;
const CREATED: &str =
    "echo 'Notice: /Stage[main]/Meridian_web/Group[deploy_group]/ensure: created'\nexit 2";

#[test]
fn the_binary_is_discovered_by_name_and_says_what_it_is() {
    let built = plugin_binary(PACKAGE);
    assert_eq!(
        built.file_name().unwrap().to_str().unwrap(),
        binary_name("openvox")
    );

    let mut registry = Registry::new();
    let infos = registry
        .discover(
            built.parent().unwrap().to_str().unwrap(),
            &StartOptions::default(),
        )
        .expect("discovery succeeds");
    assert_eq!(infos.len(), 1);
    let info = &infos[0];
    assert_eq!(info.name, "openvox");
    assert_eq!(info.category, Category::ConfigurationManagement);
    assert_eq!(
        info.operations,
        Operations {
            plan: true,
            apply: true,
            outputs: false
        }
    );
    assert_eq!(
        info.capabilities,
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: false,
        }
    );
    assert_eq!(
        info.resource_types,
        ["exec", "file", "group", "package", "service", "user"]
    );
    assert_eq!(info.protocol_version, meridian_sdk::PROTOCOL_VERSION);
    assert_eq!(registry.names(), ["openvox"]);
    registry.close().expect("the plugin shuts down cleanly");
}

#[test]
fn a_compile_across_the_process_boundary_matches_the_golden_files() {
    let dir = scratch(TMP, "process-compile");
    let client = start(&dir, OPENVOX, "exit 0", &[]);
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");

    let emitted = client
        .emit(
            &without_positions(fixture("accounts", "openvox")),
            &Data::new(),
        )
        .unwrap();
    for (path, text) in &emitted.artifact.files {
        let name = meridian_plugin_testkit::golden_name("accounts", path);
        assert_eq!(
            text,
            &fs::read_to_string(golden.join(name)).unwrap(),
            "{path}"
        );
    }

    // A refusal crosses the boundary as a refusal, naming this target, and
    // not as a failed plugin.
    let err = client
        .emit(&fixture("web_stack", "openvox"), &Data::new())
        .unwrap_err();
    let meridian_plugin_host::EmitError::Refused(refusal) = err else {
        panic!("expected a refusal, got {err}");
    };
    assert_eq!(refusal.target, "openvox");
    assert_eq!(refusal.rule, "unsupported-action");
    assert_eq!(refusal.pos.file, "fixtures/web_stack.yaml");
    client.close().unwrap();
}

#[test]
fn a_plan_and_an_apply_stream_the_tools_output_and_report_changes() {
    let dir = scratch(TMP, "process-run");
    let client = start(&dir, OPENVOX, CREATED, &[]);
    let artifact = client
        .emit(&fixture("accounts", "openvox"), &Data::new())
        .unwrap()
        .artifact;

    let mut lines: Vec<LogLine> = Vec::new();
    let plan = client
        .plan(&context(&dir), &artifact, &mut |line| lines.push(line))
        .unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(
        plan.changes[0].to_string(),
        "create deploy_group: ensure: created"
    );
    assert_eq!(lines.len(), 1);

    let applied = client
        .apply(&context(&dir), &artifact, &mut |_| {})
        .unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert!(applied.outputs.is_empty());

    // The manifest the tool was handed is the compiled class and the line
    // that includes it.
    let handed = fs::read_to_string(dir.join("work/apply.pp")).unwrap();
    assert!(handed.contains("class meridian_web {"), "{handed}");
    assert!(handed.ends_with("\ninclude meridian_web\n"), "{handed}");
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap(),
        "facts show implementation --render-as json\n\
         apply --detailed-exitcodes --color=false --noop apply.pp\n\
         apply --detailed-exitcodes --color=false apply.pp\n"
    );

    // OpenVox reports no outputs, and the host is told so instead of being
    // handed an empty answer.
    assert!(client.outputs(&context(&dir)).is_err());
    client.close().unwrap();
}

#[test]
fn the_environment_configures_the_runner() {
    let dir = scratch(TMP, "process-settings");
    let client = start(
        &dir,
        r#"echo '{"implementation":""}'"#,
        "exit 0",
        &[
            ("MERIDIAN_OPENVOX_VERIFY", "strict"),
            ("MERIDIAN_OPENVOX_ARGS", "--environment staging"),
        ],
    );
    let artifact = client
        .emit(&fixture("accounts", "openvox"), &Data::new())
        .unwrap()
        .artifact;
    let err = client
        .apply(&context(&dir), &artifact, &mut |_| {})
        .unwrap_err();
    let RunFailure::Tool(err) = err else {
        panic!("expected the tool's refusal, got {err}");
    };
    assert!(err.msg.contains("is not OpenVox"), "{err}");
    client.close().unwrap();

    let dir = scratch(TMP, "process-settings-off");
    let client = start(
        &dir,
        "exit 7",
        "exit 0",
        &[
            ("MERIDIAN_OPENVOX_VERIFY", "off"),
            ("MERIDIAN_OPENVOX_ARGS", "--environment staging"),
            ("MERIDIAN_OPENVOX_TRANSPORT", "local"),
        ],
    );
    let mut remote = context(&dir);
    remote.host = "web01.example.com".into();
    client.apply(&remote, &artifact, &mut |_| {}).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap(),
        "apply --detailed-exitcodes --color=false --environment staging apply.pp\n"
    );
    client.close().unwrap();
}
