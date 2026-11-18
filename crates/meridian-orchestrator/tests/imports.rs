//! Handing a value from one document to another's tool, run through real
//! plugin processes.
//!
//! The fake tool takes inputs. It reports an input named `greeting` back as
//! an output of the same name, and it prints every input when it applies,
//! which is how these tests see what a tool was handed.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use meridian_ir::Bundle;
use meridian_orchestrator::record;
use meridian_orchestrator::{
    run, run_selected, Mode, Observer, OnFailure, Options, Outcome, Quiet, Reason, RunReport,
    Selector, StageId, StageKind, MASK,
};
use meridian_plugin_host::{Registry, StartOptions};
use meridian_sdk::{
    Capabilities, Category, CompileError, Data, Emitted, Emitter, LogLine, ResourceGraph, Value,
};

/// The shape of issue 47: the machine is written first, and is created with
/// what the bootstrap document compiles to.
const MACHINE_AND_BOOTSTRAP: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: machine
  labels: {tier: machine}
spec:
  target: fake
  imports:
    - name: greeting
      from: artifact
      ref: bootstrap
      file: files/seed
      SENSITIVE
  resources:
    - id: vm
      type: file
      params:
        content: VM
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: bootstrap
  labels: {tier: bootstrap}
spec:
  target: fake
  targetHost: {from: inline, host: web-1}
  resources:
    - id: seed
      type: file
      params:
        content: "SEED"
"#;

/// An application that needs the address a network produces, written
/// before the network.
const APP_AND_NETWORK: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: app
spec:
  target: fake
  imports:
    - name: greeting
      from: output
      ref: network
      output: OUTPUT
  resources:
    - id: unit
      type: file
      params:
        content: app
---
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
    - id: key
      type: file
      params:
        content: hunter2-very-secret
  outputs:
    - name: address
      value: "${vm.content}"
    - name: secret_key
      value: "${key.content}"
"#;

fn bundle(text: &str) -> Bundle {
    let bundle = Bundle::new(meridian_ir::parse("workflow.yaml", text.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn machine_and_bootstrap(seed: &str, sensitive: bool) -> Bundle {
    bundle(
        &MACHINE_AND_BOOTSTRAP
            .replace("SEED", seed)
            .replace("VM", "vm-one")
            .replace(
                "SENSITIVE",
                if sensitive {
                    "sensitive: true"
                } else {
                    "sensitive: false"
                },
            ),
    )
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let dir = meridian_target_fake::plugin_dir();
    registry
        .discover(dir.to_str().unwrap(), &StartOptions::default())
        .expect("the fake plugin loads");
    registry
}

fn options(test: &str) -> Options {
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("imports")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    Options::new(dir)
}

fn names(report: &RunReport) -> Vec<&str> {
    report.stages.iter().map(|s| s.id.name.as_str()).collect()
}

fn stage<'a>(report: &'a RunReport, name: &str) -> &'a meridian_orchestrator::StageReport {
    report
        .stages
        .iter()
        .find(|s| s.id.name == name)
        .unwrap_or_else(|| panic!("no stage {name}"))
}

/// The value of the output a stage's apply produced.
fn output(report: &RunReport, name: &str, output: &str) -> Value {
    match &stage(report, name).outcome {
        Outcome::Applied { outputs, .. } => outputs
            .iter()
            .find(|o| o.name == output)
            .unwrap_or_else(|| panic!("{name} produced no {output}: {outputs:?}"))
            .value
            .clone(),
        other => panic!("{name} is {other:?}"),
    }
}

#[derive(Default)]
struct Logs(Vec<String>);

impl Observer for Logs {
    fn log(&mut self, id: &StageId, line: &LogLine) {
        self.0.push(format!("{}: {}", id.name, line.text));
    }
}

#[test]
fn a_file_one_document_compiles_to_reaches_the_tool_that_creates_the_machine() {
    let registry = registry();
    let options = options("artifact");
    let workflow = machine_and_bootstrap("#cloud-config: hello", false);

    // A plan can already see it: compiling the bootstrap needs no tool.
    let plan = run(&workflow, &registry, Mode::Plan, &options, &mut Quiet);
    assert!(plan.succeeded(), "{plan:?}");
    assert_eq!(names(&plan), ["bootstrap", "machine"]);
    assert!(matches!(
        stage(&plan, "machine").outcome,
        Outcome::Planned(_)
    ));

    let mut logs = Logs::default();
    let report = run(&workflow, &registry, Mode::Apply, &options, &mut logs);
    assert!(report.succeeded(), "{report:?}");
    // The bootstrap is written second and is a ResourceSet, and still runs
    // first, because the machine needs what it compiles to.
    assert_eq!(names(&report), ["bootstrap", "machine"]);
    assert_eq!(
        output(&report, "machine", "greeting"),
        Value::from("#cloud-config: hello")
    );
    assert!(
        logs.0
            .iter()
            .any(|line| line == "machine: inputs: {\"greeting\":\"#cloud-config: hello\"}"),
        "{:?}",
        logs.0
    );
    // The bootstrap's own tool was handed nothing.
    assert!(!logs
        .0
        .iter()
        .any(|line| line.starts_with("bootstrap: inputs")));

    // Nothing kept holds the value.
    let store = options.store();
    for key in [
        record::events_key(&report.run_id),
        record::run_key(&report.run_id),
        "state.json".to_owned(),
    ] {
        let kept = String::from_utf8(store.get(&key).unwrap().unwrap()).unwrap();
        assert!(!kept.contains("cloud-config"), "{key}: {kept}");
    }

    // A change to the bootstrap reaches the machine on the next apply.
    let changed = machine_and_bootstrap("#cloud-config: changed", false);
    let again = run(&changed, &registry, Mode::Apply, &options, &mut Quiet);
    assert!(again.succeeded(), "{again:?}");
    assert_eq!(
        output(&again, "machine", "greeting"),
        Value::from("#cloud-config: changed")
    );
}

#[test]
fn a_document_the_run_leaves_out_is_still_compiled_for_the_one_that_imports_it() {
    let registry = registry();
    let options = options("unselected");
    let workflow = machine_and_bootstrap("from the bootstrap", false);
    let selector = Selector::parse("tier=machine").unwrap();
    let report = run_selected(
        &workflow,
        &registry,
        Mode::Apply,
        &options,
        &selector,
        &mut Quiet,
    );
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(names(&report), ["machine"]);
    assert_eq!(
        output(&report, "machine", "greeting"),
        Value::from("from the bootstrap")
    );
    // The bootstrap was compiled, not applied.
    assert!(!options
        .state_dir
        .join("resourceset/bootstrap/fake/current")
        .exists());
}

#[test]
fn an_import_marked_sensitive_is_masked_in_what_the_tool_prints() {
    let registry = registry();
    let options = options("sensitive-artifact");
    // Two lines and a quote: a tool prints such a value escaped.
    let workflow = machine_and_bootstrap("a-secret-in-user-data\\nsecond 'line'", true);
    let mut logs = Logs::default();
    let report = run(&workflow, &registry, Mode::Apply, &options, &mut logs);
    assert!(report.succeeded(), "{report:?}");
    let printed = logs.0.join("\n");
    assert!(
        printed.contains(&format!("machine: inputs: {{\"greeting\":\"{MASK}\"}}")),
        "{printed}"
    );
    assert!(!printed.contains("a-secret-in-user-data"), "{printed}");
    assert!(!printed.contains("second"), "{printed}");
    // The tool hands the value back as an output without calling it
    // sensitive. It still is.
    match &stage(&report, "machine").outcome {
        Outcome::Applied { outputs, .. } => {
            let greeting = outputs.iter().find(|o| o.name == "greeting").unwrap();
            assert!(greeting.sensitive, "{greeting:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_output_reaches_any_document_and_orders_the_run() {
    let registry = registry();
    let options = options("output");
    let workflow = bundle(&APP_AND_NETWORK.replace("OUTPUT", "address"));

    // Before the network exists, the app waits on it.
    let plan = run(&workflow, &registry, Mode::Plan, &options, &mut Quiet);
    assert!(plan.succeeded(), "{plan:?}");
    assert_eq!(names(&plan), ["network", "app"]);
    assert_eq!(
        stage(&plan, "app").outcome,
        Outcome::Deferred {
            waiting_on: StageId {
                kind: StageKind::Infrastructure,
                name: "network".to_owned(),
            },
            output: "address".to_owned(),
        }
    );

    let report = run(&workflow, &registry, Mode::Apply, &options, &mut Quiet);
    assert!(report.succeeded(), "{report:?}");
    // Written app first; run network first.
    assert_eq!(names(&report), ["network", "app"]);
    assert_eq!(output(&report, "app", "greeting"), Value::from("10.0.0.5"));

    // Once it exists, a plan reads the output back from the network's tool.
    let plan = run(&workflow, &registry, Mode::Plan, &options, &mut Quiet);
    assert!(matches!(stage(&plan, "app").outcome, Outcome::Planned(_)));
}

#[test]
fn a_sensitive_output_stays_masked_when_it_is_imported() {
    let registry = registry();
    let options = options("sensitive-output");
    let workflow = bundle(&APP_AND_NETWORK.replace("OUTPUT", "secret_key"));
    let mut logs = Logs::default();
    let report = run(&workflow, &registry, Mode::Apply, &options, &mut logs);
    assert!(report.succeeded(), "{report:?}");
    let printed = logs.0.join("\n");
    assert!(
        printed.contains(&format!("app: inputs: {{\"greeting\":\"{MASK}\"}}")),
        "{printed}"
    );
    assert!(!printed.contains("hunter2"), "{printed}");
}

#[test]
fn a_failed_stage_with_imports_can_be_put_back() {
    let registry = registry();
    let options = options("rollback");
    let first = run(
        &machine_and_bootstrap("one", false),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );
    assert!(first.succeeded(), "{first:?}");

    // The machine's own file now fails, after the bootstrap was applied.
    let failing = bundle(
        &MACHINE_AND_BOOTSTRAP
            .replace("SEED", "two")
            .replace("VM", "FAIL vm")
            .replace("SENSITIVE", "sensitive: false"),
    );
    let second = run(
        &failing,
        &registry,
        Mode::Apply,
        &Options {
            on_failure: OnFailure::Rollback,
            ..options.clone()
        },
        &mut Quiet,
    );
    assert!(!second.succeeded());
    assert_eq!(second.rollbacks.len(), 2);
    assert!(
        second.rollbacks.iter().all(|r| r.outcome.is_settled()),
        "{:?}",
        second.rollbacks
    );
    let seed = fs::read_to_string(
        options
            .state_dir
            .join("resourceset/bootstrap/fake/current/files/seed"),
    )
    .unwrap();
    assert_eq!(seed, "one");
}

#[test]
fn a_file_the_document_does_not_compile_to_is_a_failure_that_lists_what_it_does() {
    let registry = registry();
    let options = options("no-file");
    let workflow = bundle(
        &MACHINE_AND_BOOTSTRAP
            .replace("files/seed", "user-data")
            .replace("SEED", "x")
            .replace("VM", "vm")
            .replace("SENSITIVE", "sensitive: false"),
    );
    let report = run(&workflow, &registry, Mode::Apply, &options, &mut Quiet);
    assert!(!report.succeeded());
    let machine = stage(&report, "machine");
    assert_eq!(machine.reason, Some(Reason::CompileRefused));
    match &machine.outcome {
        Outcome::Failed(why) => assert_eq!(
            why,
            "import \"greeting\" is the file \"user-data\" of ResourceSet \"bootstrap\", which \
             compiles to no file of that name (it compiles to: files/seed)"
        ),
        other => panic!("{other:?}"),
    }
}

/// A target that only compiles, and so takes no inputs.
struct CompilesOnly;

impl Emitter for CompilesOnly {
    fn name(&self) -> &str {
        "plain"
    }
    fn supported_resource_types(&self) -> Vec<String> {
        vec!["file".to_owned()]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn category(&self) -> Category {
        Category::Containerization
    }
    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut emitted = Emitted::default();
        emitted
            .artifact
            .files
            .insert("tree".to_owned(), graph.canonical());
        Ok(emitted)
    }
}

#[test]
fn a_target_that_takes_no_inputs_refuses_a_document_that_imports() {
    let mut registry = registry();
    registry.register(Box::new(CompilesOnly)).unwrap();
    let options = options("refused");
    // Only the machine's target changes: it is the document that imports.
    let text = MACHINE_AND_BOOTSTRAP
        .replacen("target: fake", "target: plain", 1)
        .replace("SEED", "x")
        .replace("VM", "vm")
        .replace("SENSITIVE", "sensitive: false");
    let report = run(
        &bundle(&text),
        &registry,
        Mode::Compile,
        &options,
        &mut Quiet,
    );
    assert!(!report.succeeded());
    let machine = stage(&report, "machine");
    assert_eq!(machine.reason, Some(Reason::CompileRefused));
    match &machine.outcome {
        Outcome::Failed(why) => assert_eq!(
            why,
            "the document imports \"greeting\", and target \"plain\" has no way to hand a value \
             from another document to its tool; remove spec.imports, or use a target that takes \
             inputs"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_workflow_without_imports_runs_in_the_order_it_always_did() {
    let registry = registry();
    let options = options("order");
    // A ResourceSet written first still runs after every Infrastructure
    // document, and the Infrastructure documents keep their written order.
    let text = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: web}
spec:
  target: fake
  targetHost: {from: inline, host: web-1}
  resources:
    - {id: a, type: file, params: {content: a}}
---
apiVersion: meridian/v1
kind: Infrastructure
metadata: {name: zeta}
spec:
  target: fake
  resources:
    - {id: a, type: file, params: {content: a}}
---
apiVersion: meridian/v1
kind: Infrastructure
metadata: {name: alpha}
spec:
  target: fake
  resources:
    - {id: a, type: file, params: {content: a}}
"#;
    let report = run(
        &bundle(text),
        &registry,
        Mode::Compile,
        &options,
        &mut Quiet,
    );
    assert_eq!(names(&report), ["zeta", "alpha", "web"]);
}
