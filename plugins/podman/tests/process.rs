//! The plugin as the host meets it: the real binary, in its own process,
//! configured through its environment.

use std::fs;
use std::path::Path;

use meridian_plugin_host::{Client, RunFailure, StartOptions};
use meridian_plugin_testkit::{assert_golden, graph_from_file, plugin_binary, scratch, stand_in};
use meridian_sdk::{Category, Data, Operations, RunContext, Value};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Starts the plugin with stand-ins for the tools, named the way an operator
/// would name them.
fn start(dir: &Path, extra: &[(&str, &str)]) -> Client {
    fs::create_dir_all(dir.join("units")).unwrap();
    let systemctl = stand_in(
        dir,
        "systemctl",
        &format!(
            "echo \"$@\" >> \"{}/systemctl.calls\"\ncase \"$*\" in *is-active*) echo inactive; exit 3 ;; esac",
            dir.display()
        ),
    );
    let quadlet = stand_in(
        dir,
        "quadlet",
        "for f in \"$QUADLET_UNIT_DIRS\"/*; do\n  name=$(basename \"$f\"); stem=${name%.*}\n  case \"$name\" in\n    *.container) echo \"---$stem.service---\" ;;\n    *) echo \"---$stem-${name##*.}.service---\" ;;\n  esac\ndone",
    );
    let podman = stand_in(
        dir,
        "podman",
        "case \"$1\" in --version) echo 'podman version 5.4.0' ;; volume) echo /volumes/$5 ;; esac",
    );
    let mut env = vec![
        ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        (
            "MERIDIAN_PODMAN".to_owned(),
            podman.to_str().unwrap().to_owned(),
        ),
        (
            "MERIDIAN_PODMAN_SYSTEMCTL".to_owned(),
            systemctl.to_str().unwrap().to_owned(),
        ),
        (
            "MERIDIAN_PODMAN_QUADLET".to_owned(),
            quadlet.to_str().unwrap().to_owned(),
        ),
        (
            "MERIDIAN_PODMAN_UNIT_DIR".to_owned(),
            dir.join("units").to_str().unwrap().to_owned(),
        ),
    ];
    for (key, value) in extra {
        env.push(((*key).to_owned(), (*value).to_owned()));
    }
    let binary = plugin_binary("meridian-target-podman");
    Client::start(
        binary.to_str().unwrap(),
        &StartOptions {
            env: Some(env),
            ..StartOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("starting the plugin: {e}"))
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-app".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

#[test]
fn the_host_compiles_plans_and_applies_through_the_real_binary() {
    let dir = scratch(TMP, "process");
    let client = start(&dir, &[]);

    let info = client.info();
    assert_eq!(info.name, "podman");
    assert_eq!(info.category, Category::Containerization);
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(
        info.resource_types,
        [
            "build",
            "container",
            "image",
            "kube",
            "network",
            "pod",
            "volume"
        ]
    );
    assert!(info.capabilities.native_notify);
    assert!(!info.capabilities.runtime_condition);

    // What crosses the process boundary is the golden output, byte for byte.
    let graph = graph_from_file("examples/podman/web-app.yaml", "podman");
    let emitted = client.emit(&graph, &Data::new()).unwrap();
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    assert_golden(&golden, "web-app", &emitted);

    let mut said = Vec::new();
    let plan = client
        .plan(&context(&dir), &emitted.artifact, &mut |line| {
            said.push(line.text)
        })
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(plan.changes.len(), 4);
    assert!(!plan.unknown);
    assert!(fs::read_dir(dir.join("units")).unwrap().next().is_none());

    let applied = client
        .apply(&context(&dir), &emitted.artifact, &mut |line| {
            said.push(line.text)
        })
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(applied.changes.len(), 4);
    assert_eq!(
        fs::read_to_string(dir.join("units/shop-web.container")).unwrap(),
        emitted.artifact.files["shop-web.container"]
    );
    // The tool's progress reached the host while it ran.
    assert_eq!(
        said,
        ["Starting shop-net-network.service, shop-data-volume.service, shop-db.service, shop-web.service"]
    );
    let directory = applied
        .outputs
        .iter()
        .find(|o| o.name == "data_directory")
        .unwrap();
    assert_eq!(
        directory.value,
        Value::String("/volumes/systemd-shop-data".into())
    );
    assert_eq!(client.outputs(&context(&dir)).unwrap().len(), 3);
    client.close().unwrap();
}

#[test]
fn a_refusal_and_a_bad_setting_both_cross_the_boundary_as_answers() {
    let dir = scratch(TMP, "process-refusals");
    let client = start(&dir, &[("MERIDIAN_PODMAN_SCOPE", "root")]);

    // A document for another kind of tool is refused, with its position.
    let graph = meridian_plugin_testkit::fixture("accounts", "podman");
    let err = match client.emit(&graph, &Data::new()) {
        Err(meridian_plugin_host::EmitError::Refused(err)) => err,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(err.target, "podman");
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(err.pos.file, "fixtures/accounts.yaml");

    let graph = graph_from_file("examples/podman/web-app.yaml", "podman");
    let emitted = client.emit(&graph, &Data::new()).unwrap();
    let failure = client
        .plan(&context(&dir), &emitted.artifact, &mut |_| {})
        .unwrap_err();
    let RunFailure::Tool(err) = failure else {
        panic!("a bad setting is the operator's to fix, not a crash: {failure}");
    };
    assert_eq!(
        err.msg,
        "MERIDIAN_PODMAN_SCOPE is \"root\"; it must be \"user\" or \"system\""
    );
}
