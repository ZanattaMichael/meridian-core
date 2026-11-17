//! The plugin as the host meets it: the real binary, in its own process,
//! over the plugin protocol. Each stage is tested in process elsewhere; this
//! proves nothing is lost or reworded on the way across the boundary.

#![allow(clippy::result_large_err)]

use std::path::Path;

use meridian_plugin_host::{Client, EmitError, RunFailure, StartOptions};
use meridian_plugin_testkit::{
    assert_golden, fixture, plugin_binary, scratch, stand_in, without_positions,
};
use meridian_sdk::{
    Capabilities, Category, Data, Operations, RunContext, PROTOCOL_VERSION, VERSION,
};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn start(env: Option<Vec<(String, String)>>) -> Client {
    let binary = plugin_binary("meridian-target-salt");
    let options = StartOptions {
        env,
        ..StartOptions::default()
    };
    Client::start(binary.to_str().unwrap(), &options)
        .unwrap_or_else(|e| panic!("starting the plugin: {e}"))
}

#[test]
fn the_plugin_describes_itself() {
    let client = start(None);
    let info = client.info();
    assert_eq!(client.name(), "salt");
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
            runtime_condition: true,
        }
    );
    assert_eq!(
        info.resource_types,
        ["exec", "file", "group", "package", "service", "user"]
    );
    assert_eq!(info.protocol_version, PROTOCOL_VERSION);
    assert_eq!(info.sdk_version, VERSION);
    client.close().expect("the plugin exits when asked");
}

#[test]
fn fixtures_compile_to_their_golden_files_across_the_boundary() {
    let client = start(None);
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    // The fixture with a runtime condition is written in another target's
    // terms and is compiled in this one's by the in-process test.
    for name in ["accounts", "restart_chain", "web_stack"] {
        let graph = without_positions(fixture(name, "salt"));
        let emitted = client
            .emit(&graph, &Data::new())
            .unwrap_or_else(|e| panic!("compiling {name}: {e}"));
        assert_golden(&golden, name, &emitted);
    }
}

#[test]
fn a_refusal_crosses_the_boundary_with_its_rule_and_its_line() {
    let client = start(None);
    let mut graph = fixture("web_stack", "salt");
    graph.resources[0].r#type = "firewall_rule".into();
    let Err(EmitError::Refused(err)) = client.emit(&graph, &Data::new()) else {
        panic!("the document should be refused");
    };
    assert_eq!(err.target, "salt");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(err.resource, "nginx_pkg");
    assert_eq!(err.pos.file, "fixtures/web_stack.yaml");
    assert!(err.pos.line > 0);

    // A refusal is an answer, not a crash: the same plugin keeps working.
    client
        .emit(&fixture("accounts", "salt"), &Data::new())
        .expect("the plugin still compiles");
}

#[test]
fn a_plan_and_a_failed_apply_cross_the_boundary() {
    let dir = scratch(TMP, "boundary");
    let program = stand_in(
        &dir,
        "salt-call",
        r#"case "$*" in
*test=True*) cat <<'OUT'
{"local": {"pkg_|-nginx_pkg_|-nginx_|-installed": {"changes": {"nginx": {"new": "installed", "old": ""}},
  "result": null, "comment": "The following packages would be installed/updated: nginx",
  "__run_num__": 0, "__id__": "nginx_pkg"}}}
OUT
;;
*) cat <<'OUT'
{"local": {"pkg_|-nginx_pkg_|-nginx_|-installed": {"changes": {}, "result": false,
  "comment": "Problem encountered installing package(s)", "__run_num__": 0, "__id__": "nginx_pkg"}}}
OUT
exit 2 ;;
esac"#,
    );
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push((
        meridian_target_salt::SALT_CALL_VARIABLE.to_owned(),
        program.to_str().unwrap().to_owned(),
    ));
    env.push((
        meridian_target_salt::TRANSPORT_VARIABLE.to_owned(),
        "local".to_owned(),
    ));
    let client = start(Some(env));

    let graph = without_positions(fixture("web_stack", "salt"));
    let artifact = client.emit(&graph, &Data::new()).unwrap().artifact;
    let context = RunContext {
        document: "web".into(),
        host: "web01.example.com".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };

    let mut lines = Vec::new();
    let plan = client
        .plan(&context, &artifact, &mut |line| lines.push(line.text))
        .unwrap_or_else(|e| panic!("planning: {e}"));
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].resource, "nginx_pkg");
    assert_eq!(
        lines,
        ["nginx_pkg (pkg.installed): would change: The following packages would be installed/updated: nginx"]
    );
    // The compiled files are what the tool was pointed at.
    assert_eq!(
        std::fs::read_to_string(dir.join("work/states/web.sls")).unwrap(),
        artifact.files["states/web.sls"]
    );

    let Err(RunFailure::Tool(err)) = client.apply(&context, &artifact, &mut |_| {}) else {
        panic!("the apply should fail as the tool's answer");
    };
    assert_eq!(err.msg, "salt-call reported a failed state");
    assert_eq!(err.exit_code, Some(2));
    assert_eq!(
        err.output,
        "nginx_pkg: Problem encountered installing package(s)\n"
    );
}
