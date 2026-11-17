//! The plugin as the host meets it: the real binary, launched and driven
//! over the protocol, with a stand-in for `docker` behind it.

use std::fs;
use std::path::Path;

use meridian_plugin_host::{Client, EmitError, RunFailure, StartOptions};
use meridian_plugin_testkit::{graph_from_file, plugin_binary, scratch, stand_in};
use meridian_sdk::{
    Capabilities, Category, Data, Emitter, LogLine, Operations, RunContext, Value, PROTOCOL_VERSION,
};
use meridian_target_docker::Docker;

/// The variable that names the Docker program to the plugin.
const DOCKER_VARIABLE: &str = "MERIDIAN_DOCKER";

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Starts the plugin with `docker` replaced by a script that answers as
/// Docker would for the web example with its proxy not yet started.
fn start(dir: &Path) -> Client {
    let docker = stand_in(
        dir,
        "docker",
        &format!(
            r#"echo "$@" >> "{dir}/calls"
case "$*" in
*" config --format json") echo '{{"services":{{"db":{{"image":"postgres:16"}},"app":{{"image":"app"}},"proxy":{{"image":"nginx:1.27"}}}},"networks":{{"backend":{{"name":"web_backend"}}}},"volumes":{{"db_data":{{"name":"web_db_data"}}}}}}' ;;
*" config --hash "*) echo 'db h-db'; echo 'app h-app'; echo 'proxy h-proxy' ;;
*"ps --all --filter"*) echo 'db|h-db|running|web-db-1|False'; echo 'app|h-old|running|web-app-1|False' ;;
*"network ls "*) echo web_backend ;;
*"volume ls "*) echo web_db_data ;;
*" up "*) echo ' Container web-proxy-1 Started' >&2 ;;
*" port --protocol tcp proxy 80") echo '0.0.0.0:32768' ;;
esac"#,
            dir = dir.display()
        ),
    );
    let options = StartOptions {
        env: Some(vec![
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            (
                DOCKER_VARIABLE.to_owned(),
                docker.to_str().unwrap().to_owned(),
            ),
        ]),
        ..StartOptions::default()
    };
    let binary = plugin_binary("meridian-target-docker");
    Client::start(binary.to_str().unwrap(), &options).expect("the plugin starts")
}

#[test]
fn the_plugin_describes_itself_over_the_protocol() {
    let client = start(&scratch(TMP, "process-describe"));
    let info = client.info();
    assert_eq!(info.name, "docker");
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.category, Category::Containerization);
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(
        info.capabilities,
        Capabilities {
            native_notify: true,
            synthetic_notify: true,
            runtime_condition: false,
        }
    );
    assert_eq!(
        info.resource_types,
        ["config", "network", "project", "secret", "service", "volume"]
    );
    client
        .close()
        .expect("the plugin exits when its stdin closes");
}

#[test]
fn a_compile_through_the_process_is_the_compile_in_process() {
    let client = start(&scratch(TMP, "process-compile"));
    for example in ["web", "monitoring"] {
        let graph = graph_from_file(&format!("examples/docker/{example}.yaml"), "docker");
        let through = client
            .emit(&graph, &Data::new())
            .unwrap_or_else(|e| panic!("{example} compiles: {e}"));
        assert_eq!(through, Docker.emit(&graph, &Data::new()).unwrap());
    }

    // Warnings cross the boundary beside the artifact.
    let graph = graph_from_file(
        "plugins/docker/testdata/documents/relative-paths.yaml",
        "compose",
    );
    let emitted = client.emit(&graph, &Data::new()).unwrap();
    assert_eq!(emitted.warnings.len(), 7);
    assert_eq!(emitted.warnings[0].target, "docker");
    assert_eq!(emitted.warnings[0].resource, "site");
}

#[test]
fn a_refusal_crosses_the_boundary_with_its_position() {
    let client = start(&scratch(TMP, "process-refusal"));
    let graph = graph_from_file(
        "plugins/docker/testdata/documents/unknown-volume.yaml",
        "compose",
    );
    match client.emit(&graph, &Data::new()) {
        Err(EmitError::Refused(e)) => assert_eq!(
            e.to_string(),
            "plugins/docker/testdata/documents/unknown-volume.yaml:11:7: docker: resource \"db\" \
             [invalid-reference]: uses the volume \"dbdata\", which the document does not \
             declare. Add a resource of type \"volume\" with that id"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn plan_apply_and_outputs_run_through_the_process() {
    let dir = scratch(TMP, "process-run");
    let client = start(&dir);
    let graph = graph_from_file("examples/docker/web.yaml", "docker");
    let artifact = client.emit(&graph, &Data::new()).unwrap().artifact;
    let context = RunContext {
        document: "web".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };

    let mut lines: Vec<LogLine> = Vec::new();
    let plan = client
        .plan(&context, &artifact, &mut |line| lines.push(line))
        .expect("the plan runs");
    let shown: Vec<String> = plan.changes.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "replace app: its configuration changed, so its container is recreated",
            "create proxy: container, image nginx:1.27",
        ]
    );
    assert_eq!(
        lines[0].text,
        "project web: comparing the file with what Docker is running"
    );
    // The plugin wrote what it compiled where it told Compose to read it.
    assert_eq!(
        fs::read_to_string(dir.join("work/compose.yaml")).unwrap(),
        artifact.files["compose.yaml"]
    );

    let mut lines: Vec<LogLine> = Vec::new();
    let applied = client
        .apply(&context, &artifact, &mut |line| lines.push(line))
        .expect("the apply runs");
    assert_eq!(applied.changes, plan.changes);
    assert!(lines
        .iter()
        .any(|l| l.text == " Container web-proxy-1 Started"));
    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    assert!(
        calls.contains(
            "--host ssh://deploy@web01.example.com compose --project-name web --file compose.yaml up --detach --remove-orphans --wait\n"
        ),
        "{calls}"
    );

    assert_eq!(applied.outputs.len(), 1);
    assert_eq!(applied.outputs[0].name, "web_port");
    assert_eq!(applied.outputs[0].value, Value::String("32768".into()));
    assert_eq!(client.outputs(&context).unwrap(), applied.outputs);
}

#[test]
fn a_failure_of_the_tool_is_an_answer_and_not_a_crash() {
    let dir = scratch(TMP, "process-failure");
    let client = start(&dir);
    let context = RunContext {
        document: "shop".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };
    match client.outputs(&context) {
        Err(RunFailure::Tool(e)) => assert_eq!(
            e.msg,
            "nothing has been applied for this document yet, so it has no outputs"
        ),
        other => panic!("expected the tool's own failure, got {other:?}"),
    }
    // The plugin is still there to answer the next call.
    let graph = graph_from_file("examples/docker/web.yaml", "docker");
    assert!(client.emit(&graph, &Data::new()).is_ok());
}
