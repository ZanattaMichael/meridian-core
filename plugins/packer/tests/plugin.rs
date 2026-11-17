//! The real plugin binary, driven by the host across the process boundary,
//! from a document to the outputs of a build.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use meridian_plugin_host::{Client, EmitError, StartOptions};
use meridian_plugin_testkit::{
    assert_golden, graph_from_file, graph_from_text, plugin_binary, scratch, stand_in,
};
use meridian_sdk::{Capabilities, Category, Data, LogLine, Operations, RunContext, Value};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// Starts the plugin, with a stand-in for `packer` named in its environment
/// when one is given.
fn start(packer: Option<&Path>) -> Client {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| name != "MERIDIAN_PACKER")
        .collect();
    if let Some(path) = packer {
        env.push(("MERIDIAN_PACKER".to_owned(), path.display().to_string()));
    }
    let options = StartOptions {
        env: Some(env),
        ..StartOptions::default()
    };
    let binary = plugin_binary("meridian-target-packer");
    Client::start(binary.to_str().unwrap(), &options)
        .unwrap_or_else(|e| panic!("starting the plugin: {e}"))
}

#[test]
fn the_binary_describes_itself_to_the_host() {
    let client = start(None);
    let info = client.info();
    assert_eq!(info.name, "packer");
    assert_eq!(info.category, Category::InfrastructureAsCode);
    assert_eq!(info.operations, Operations::ALL);
    assert_eq!(info.capabilities, Capabilities::default());
    assert_eq!(
        info.resource_types,
        [
            "data.<type>",
            "error-cleanup-provisioner.<type>",
            "local",
            "plugin",
            "post-processor.<type>",
            "provisioner.<type>",
            "source.<builder>",
            "variable",
        ]
    );
    client.close().expect("the plugin exits when asked");
}

#[test]
fn compiling_across_the_boundary_gives_the_same_bytes_and_the_same_refusals() {
    let client = start(None);
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    for case in ["web-image", "app-container", "hello"] {
        let graph = graph_from_file(&format!("examples/packer/{case}.yaml"), "packer");
        let emitted = client.emit(&graph, &Data::new()).unwrap();
        assert_golden(&golden, case, &emitted);
    }

    let graph = graph_from_text(
        "image.yaml",
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: img\nspec:\n  target: packer\n  resources:\n    - id: nginx\n      type: package\n      params: {name: nginx}\n",
        "packer",
    );
    let Err(EmitError::Refused(refusal)) = client.emit(&graph, &Data::new()) else {
        panic!("the document should be refused by the plugin, not fail in transit");
    };
    assert_eq!(refusal.target, "packer");
    assert_eq!(refusal.resource, "nginx");
    assert_eq!(refusal.rule, "unsupported-resource-type");
    assert_eq!(refusal.pos.file, "image.yaml");
    assert_eq!(refusal.pos.line, 8);
}

#[test]
fn a_document_is_compiled_planned_built_and_its_outputs_read_through_the_host() {
    let dir = scratch(TMP, "through-the-host");
    let packer = stand_in(
        &dir,
        "packer",
        r#"case "$1" in
init) ;;
validate) echo 'The configuration is valid.' ;;
build)
  echo "==> hello.null.box: greeting is $PKR_VAR_greeting"
  echo '{"builds":[{"name":"box","builder_type":"null","build_time":1794000000,"files":null,"artifact_id":"Null","packer_run_uuid":"r1","custom_data":null}],"last_run_uuid":"r1"}' > meridian-manifest.json ;;
esac"#,
    );
    let client = start(Some(&packer));
    let graph = graph_from_file("examples/packer/hello.yaml", "packer");
    let artifact = client.emit(&graph, &Data::new()).unwrap().artifact;
    let context = RunContext {
        document: "hello".into(),
        work_dir: dir.join("work"),
        inputs: [("greeting".to_owned(), Value::String("good morning".into()))].into(),
        ..RunContext::default()
    };

    let lines: Mutex<Vec<LogLine>> = Mutex::new(Vec::new());
    let mut keep = |line: LogLine| lines.lock().unwrap().push(line);
    let plan = client.plan(&context, &artifact, &mut keep).unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].to_string(), "create box: null image");

    let applied = client.apply(&context, &artifact, &mut keep).unwrap();
    assert_eq!(applied.changes, plan.changes);
    let text: Vec<String> = lines
        .lock()
        .unwrap()
        .iter()
        .map(|l| l.text.clone())
        .collect();
    assert!(
        text.contains(&"==> hello.null.box: greeting is good morning".to_owned()),
        "{text:?}"
    );

    let outputs = client.outputs(&context).unwrap();
    assert_eq!(outputs, applied.outputs);
    let value = |name: &str| &outputs.iter().find(|o| o.name == name).unwrap().value;
    assert_eq!(value("builder"), &Value::String("null".into()));
    assert_eq!(value("artifact"), &Value::String("Null".into()));
    assert_eq!(
        value("summary"),
        &Value::String("built Null with null".into())
    );
    assert!(fs::read_to_string(dir.join("work/build.pkr.json"))
        .unwrap()
        .contains("source.null.box"));
}
