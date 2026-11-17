//! A whole workflow, run through real plugin processes.

use std::fs;
use std::path::{Path, PathBuf};

use meridian_ir::Bundle;
use meridian_orchestrator::{
    artifact_hash, kind_fits, run, run_selected, selected, version_fits, work_dir, Mode, Observer,
    Options, Outcome, Quiet, RunState, Selector, StageId, StageKind, StageReport, MASK,
};
use meridian_plugin_host::{Registry, StartOptions};
use meridian_sdk::{
    Capabilities, Category, ChangeAction, CompileError, Data, Emitted, Emitter, LogLine,
    ResourceGraph, Value,
};

/// An Infrastructure document that "provisions" a machine by writing its
/// address into a file, and a ResourceSet configured on whatever address that
/// turns out to be.
const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: infrastructure
    ref: network
    output: address
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
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

const INLINE: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: TARGET
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: motd
      type: file
      params:
        content: "CONTENT"
"#;

fn bundle(text: &str) -> Bundle {
    let bundle = Bundle::new(meridian_ir::parse("workflow.yaml", text.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let dir = meridian_target_fake::plugin_dir();
    let found = registry
        .discover(dir.to_str().unwrap(), &StartOptions::default())
        .expect("the fake plugin loads");
    assert_eq!(found.len(), 1);
    registry
}

fn options(test: &str) -> Options {
    let state_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&state_dir);
    Options::new(state_dir)
}

fn id(kind: StageKind, name: &str) -> StageId {
    StageId {
        kind,
        name: name.to_owned(),
    }
}

/// Records what an observer is told, in order.
#[derive(Default)]
struct Recorder {
    events: Vec<String>,
}

impl Observer for Recorder {
    fn stage_started(&mut self, id: &StageId, target: &str) {
        self.events.push(format!("start {id} with {target}"));
    }
    fn log(&mut self, id: &StageId, line: &LogLine) {
        self.events.push(format!("log {}: {}", id.name, line.text));
    }
    fn stage_finished(&mut self, report: &StageReport) {
        self.events.push(format!("finish {}", report.id));
    }
}

fn file(dir: &Path, relative: &str) -> PathBuf {
    dir.join("current").join(relative)
}

#[test]
fn compile_touches_nothing_and_defers_what_depends_on_infrastructure() {
    let registry = registry();
    let options = options("compile");
    let report = run(
        &bundle(WORKFLOW),
        &registry,
        Mode::Compile,
        &options,
        &mut Quiet,
    );

    // Infrastructure runs first, whatever order the documents were written in.
    assert_eq!(
        report.stages[0].id,
        id(StageKind::Infrastructure, "network")
    );
    assert_eq!(report.stages[0].outcome, Outcome::Compiled);
    let artifact = report.stages[0].artifact.as_ref().unwrap();
    assert_eq!(artifact.files["files/vm"], "10.0.0.5");
    assert_eq!(artifact.files["outputs/address"], "${vm.content}");

    // The address does not exist until the infrastructure does.
    assert_eq!(
        report.stages[1].outcome,
        Outcome::Deferred {
            waiting_on: id(StageKind::Infrastructure, "network"),
            output: "address".to_owned(),
        }
    );
    assert!(report.succeeded(), "a deferred stage is not a failure");
    assert!(
        !file(
            &work_dir(&options.state_dir, &report.stages[0].id, "fake"),
            "files/vm"
        )
        .exists(),
        "compiling applied something"
    );
}

#[test]
fn apply_hands_an_output_to_the_next_stage_and_masks_the_secret_one() {
    let registry = registry();
    let options = options("apply");
    let bundle = bundle(WORKFLOW);
    let mut recorder = Recorder::default();
    let report = run(&bundle, &registry, Mode::Apply, &options, &mut recorder);
    assert!(report.succeeded(), "{report:?}");

    let network = &report.stages[0];
    let Outcome::Applied { changes, outputs } = &network.outcome else {
        panic!("{:?}", network.outcome);
    };
    assert_eq!(changes.len(), 4);
    let address = outputs.iter().find(|o| o.name == "address").unwrap();
    assert_eq!(address.value, Value::from("10.0.0.5"));
    assert!(
        outputs
            .iter()
            .find(|o| o.name == "secret_key")
            .unwrap()
            .sensitive
    );

    // The ResourceSet was compiled for, and applied to, the address the
    // infrastructure stage produced.
    let web = &report.stages[1];
    assert_eq!(web.id, id(StageKind::ResourceSet, "web"));
    assert_eq!(web.host, "10.0.0.5");
    assert!(matches!(&web.outcome, Outcome::Applied { changes, .. } if changes.len() == 1));
    let web_dir = work_dir(&options.state_dir, &web.id, "fake");
    assert_eq!(
        fs::read_to_string(file(&web_dir, "files/motd")).unwrap(),
        "welcome\n"
    );

    // Each stage has its own directory, so one tool never sees another's state.
    let network_dir = work_dir(&options.state_dir, &network.id, "fake");
    assert_ne!(web_dir, network_dir);
    assert!(file(&network_dir, "files/vm").exists());

    // The second stage's tool echoed its inputs, the secret among them. What
    // reached the observer has it masked, and the plain address intact.
    let echoed = recorder
        .events
        .iter()
        .find(|e| e.starts_with("log web: inputs:"))
        .expect("the tool echoed its inputs");
    assert!(echoed.contains("\"address\":\"10.0.0.5\""), "{echoed}");
    assert!(echoed.contains(MASK), "{echoed}");
    assert!(
        !recorder.events.iter().any(|e| e.contains("hunter2")),
        "a secret reached the observer: {:?}",
        recorder.events
    );
    assert_eq!(
        recorder.events.first().map(String::as_str),
        Some("start Infrastructure \"network\" with fake")
    );
    assert_eq!(
        recorder.events.last().map(String::as_str),
        Some("finish ResourceSet \"web\"")
    );

    // The run-state records what was applied, and holds no outputs at all.
    let state_file = options.state_dir.join("state.json");
    let state = RunState::load(&state_file);
    let record = state.get(&web.id).expect("the stage was recorded");
    assert!(record.applied);
    assert_eq!(record.target, "fake");
    assert_eq!(
        record.artifact_hash,
        artifact_hash(web.artifact.as_ref().unwrap())
    );
    assert!(!fs::read_to_string(&state_file).unwrap().contains("hunter2"));
}

#[test]
fn a_plan_after_an_apply_sees_the_existing_infrastructure() {
    let registry = registry();
    let options = options("plan-after-apply");
    let bundle = bundle(WORKFLOW);

    // Before anything exists: the infrastructure is all to be created, and
    // the stage that needs its address cannot be planned yet.
    let before = run(&bundle, &registry, Mode::Plan, &options, &mut Quiet);
    let Outcome::Planned(plan) = &before.stages[0].outcome else {
        panic!("{:?}", before.stages[0].outcome);
    };
    assert_eq!(plan.changes.len(), 4);
    assert!(plan
        .changes
        .iter()
        .all(|c| c.action == ChangeAction::Create));
    assert!(matches!(before.stages[1].outcome, Outcome::Deferred { .. }));

    assert!(run(&bundle, &registry, Mode::Apply, &options, &mut Quiet).succeeded());

    // A fresh run has no outputs in memory. It reads them back from the tool
    // that owns them, so the second stage can now be planned, and both are
    // in sync.
    let after = run(&bundle, &registry, Mode::Plan, &options, &mut Quiet);
    for stage in &after.stages {
        let Outcome::Planned(plan) = &stage.outcome else {
            panic!("{}: {:?}", stage.id, stage.outcome);
        };
        assert!(plan.in_sync(), "{}: {:?}", stage.id, plan.changes);
    }
    assert_eq!(after.stages[1].host, "10.0.0.5");
}

#[test]
fn a_failed_stage_stops_the_run_and_later_stages_are_not_attempted() {
    let registry = registry();
    let options = options("failure");
    let broken = WORKFLOW.replace("content: 10.0.0.5", "content: FAIL 10.0.0.5");
    let report = run(
        &bundle(&broken),
        &registry,
        Mode::Apply,
        &options,
        &mut Quiet,
    );

    assert!(!report.succeeded());
    let Outcome::Failed(why) = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert!(why.starts_with("could not write files/vm"), "{why}");
    assert_eq!(
        report.stages[1].outcome,
        Outcome::NotRun {
            because_of: id(StageKind::Infrastructure, "network"),
        }
    );
    assert!(report.stages[1].artifact.is_none());

    let state = RunState::load(&options.state_dir.join("state.json"));
    assert!(!state.get(&report.stages[0].id).unwrap().applied);
    assert!(state.get(&report.stages[1].id).is_none());
}

#[test]
fn a_document_for_a_target_with_no_plugin_fails_and_names_what_is_loaded() {
    let registry = registry();
    let text = INLINE
        .replace("TARGET", "terraform")
        .replace("CONTENT", "x");
    let report = run(
        &bundle(&text),
        &registry,
        Mode::Plan,
        &options("no-plugin"),
        &mut Quiet,
    );
    assert_eq!(
        report.stages[0].outcome,
        Outcome::Failed(
            "no plugin provides target \"terraform\" (loaded targets: fake)".to_owned()
        )
    );
}

#[test]
fn a_refusal_to_compile_fails_the_stage_at_the_authors_line() {
    let registry = registry();
    let text = INLINE
        .replace("TARGET", "fake")
        .replace("CONTENT", "x")
        .replace("type: file", "type: firewall_rule");
    let report = run(
        &bundle(&text),
        &registry,
        Mode::Apply,
        &options("refusal"),
        &mut Quiet,
    );
    let Outcome::Failed(why) = &report.stages[0].outcome else {
        panic!("{:?}", report.stages[0].outcome);
    };
    assert_eq!(
        why,
        "workflow.yaml:11:7: fake: resource \"motd\" [unsupported-resource-type]: \
         the fake target has no mapping for type \"firewall_rule\""
    );
}

#[test]
fn a_target_that_only_compiles_can_be_planned_but_not_applied() {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.push(("FAKE_OPERATIONS".to_owned(), "none".to_owned()));
    let mut registry = Registry::new();
    registry
        .load(
            meridian_target_fake::plugin_path().to_str().unwrap(),
            &StartOptions {
                env: Some(env),
                ..StartOptions::default()
            },
        )
        .unwrap();
    let bundle = bundle(&INLINE.replace("TARGET", "fake").replace("CONTENT", "x"));
    let options = options("compile-only");

    let planned = run(&bundle, &registry, Mode::Plan, &options, &mut Quiet);
    assert_eq!(planned.stages[0].outcome, Outcome::Compiled);
    assert!(planned.stages[0].artifact.is_some());

    let applied = run(&bundle, &registry, Mode::Apply, &options, &mut Quiet);
    let Outcome::Failed(why) = &applied.stages[0].outcome else {
        panic!("{:?}", applied.stages[0].outcome);
    };
    assert!(why.starts_with("target \"fake\" only compiles"), "{why}");
}

#[test]
fn an_artifact_hash_depends_on_content_and_not_on_how_files_are_split() {
    let artifact = |files: &[(&str, &str)]| meridian_sdk::Artifact {
        files: files
            .iter()
            .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
            .collect(),
    };
    let a = artifact_hash(&artifact(&[("a", "bc"), ("d", "")]));
    assert_eq!(a, artifact_hash(&artifact(&[("d", ""), ("a", "bc")])));
    assert_ne!(a, artifact_hash(&artifact(&[("ab", "c"), ("d", "")])));
    assert_ne!(a, artifact_hash(&artifact(&[("a", "bcd")])));
    assert!(a.starts_with("sha256:") && a.len() == 71);
}

/// A target that lives in this process, so a test can give it any category
/// and any list of versions. It compiles a document into one file listing
/// what it was handed.
struct InProcess {
    name: &'static str,
    category: Category,
    versions: &'static [&'static str],
}

impl Emitter for InProcess {
    fn name(&self) -> &str {
        self.name
    }
    fn supported_resource_types(&self) -> Vec<String> {
        vec!["file".to_owned()]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn category(&self) -> Category {
        self.category
    }
    fn target_versions(&self) -> Vec<String> {
        self.versions.iter().map(|v| (*v).to_owned()).collect()
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

const CATEGORIES: [(&str, Category); 5] = [
    ("iac", Category::InfrastructureAsCode),
    ("cm", Category::ConfigurationManagement),
    ("containers", Category::Containerization),
    ("cluster", Category::ContainerOrchestration),
    ("old", Category::Unspecified),
];

fn in_process(versions: &'static [&'static str]) -> Registry {
    let mut registry = Registry::new();
    for (name, category) in CATEGORIES {
        registry
            .register(Box::new(InProcess {
                name,
                category,
                versions,
            }))
            .unwrap();
    }
    registry
}

const PROVISION: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: net
spec:
  target: TARGET
  resources:
    - id: vm
      type: file
"#;

fn outcome_of(text: &str, registry: &Registry, test: &str) -> Outcome {
    let report = run(
        &bundle(text),
        registry,
        Mode::Compile,
        &options(test),
        &mut Quiet,
    );
    report.stages[0].outcome.clone()
}

fn failure(outcome: Outcome) -> String {
    match outcome {
        Outcome::Failed(why) => why,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_document_is_only_handed_to_a_category_that_takes_its_kind() {
    let registry = in_process(&[]);
    let resource_set = |target: &str| INLINE.replace("TARGET", target).replace("CONTENT", "x");
    let infrastructure = |target: &str| PROVISION.replace("TARGET", target);

    // (target, takes a ResourceSet, takes an Infrastructure document)
    for (target, takes_resource_set, takes_infrastructure) in [
        ("iac", false, true),
        ("cm", true, false),
        ("containers", true, true),
        ("cluster", false, true),
        ("old", true, true),
    ] {
        let test = format!("category-{target}");
        let got = outcome_of(&resource_set(target), &registry, &test);
        assert_eq!(
            got == Outcome::Compiled,
            takes_resource_set,
            "{target}: {got:?}"
        );
        let got = outcome_of(&infrastructure(target), &registry, &test);
        assert_eq!(
            got == Outcome::Compiled,
            takes_infrastructure,
            "{target}: {got:?}"
        );
    }

    assert_eq!(
        failure(outcome_of(&resource_set("iac"), &registry, "category-why")),
        "this is a ResourceSet document, and its target is an infrastructure-as-code tool: it \
         creates what should exist and is not pointed at one machine, so it takes kind: \
         Infrastructure, without a targetHost"
    );
    assert_eq!(
        failure(outcome_of(&infrastructure("cm"), &registry, "category-why")),
        "this is an Infrastructure document, and its target is a configuration-management \
         tool: it configures a machine that already exists, so it takes kind: ResourceSet, \
         with a targetHost"
    );
    assert!(failure(outcome_of(
        &resource_set("cluster"),
        &registry,
        "category-why"
    ))
    .contains("a container-orchestration tool"),);
}

#[test]
fn the_pairing_of_kind_and_category_is_one_table() {
    use Category::*;
    use StageKind::*;
    for (kind, category, fits) in [
        (ResourceSet, ConfigurationManagement, true),
        (ResourceSet, Containerization, true),
        (ResourceSet, Unspecified, true),
        (ResourceSet, InfrastructureAsCode, false),
        (ResourceSet, ContainerOrchestration, false),
        (Infrastructure, InfrastructureAsCode, true),
        (Infrastructure, Containerization, true),
        (Infrastructure, ContainerOrchestration, true),
        (Infrastructure, Unspecified, true),
        (Infrastructure, ConfigurationManagement, false),
    ] {
        assert_eq!(
            kind_fits(kind, category).is_ok(),
            fits,
            "{kind:?} {category}"
        );
    }
}

#[test]
fn a_wrong_pairing_is_refused_before_the_plugin_or_the_host_is_asked_for() {
    // The ResourceSet names an Infrastructure output that does not exist yet.
    // Without the check it would be reported as deferred, and the mistake
    // would only surface after the infrastructure had been applied.
    let registry = in_process(&[]);
    let text = WORKFLOW
        .replace(
            "target: fake\n  targetHost",
            "target: cluster\n  targetHost",
        )
        .replace("target: fake", "target: iac");
    let report = run(
        &bundle(&text),
        &registry,
        Mode::Compile,
        &options("category-first"),
        &mut Quiet,
    );
    assert_eq!(report.stages[0].outcome, Outcome::Compiled);
    assert!(
        failure(report.stages[1].outcome.clone()).starts_with("this is a ResourceSet document"),
    );
}

#[test]
fn a_target_version_must_be_one_the_target_declares() {
    for (asked, declared, fits) in [
        ("3", &["3"][..], true),
        ("3.0", &["3"], true),
        ("3.1.4", &["3"], true),
        ("3.1.4", &["3.1"], true),
        ("2", &["2", "3"], true),
        ("4", &["2", "3"], false),
        // A version is compared part by part, never as a prefix of text.
        ("30", &["3"], false),
        ("1.10", &["1.1"], false),
        // The document may be more specific than the target, not less.
        ("3", &["3.1"], false),
        ("3", &[], false),
    ] {
        let declared: Vec<String> = declared.iter().map(|v| (*v).to_owned()).collect();
        assert_eq!(
            version_fits(asked, &declared),
            fits,
            "{asked} in {declared:?}"
        );
    }
}

#[test]
fn a_target_version_reaches_the_plugin_or_the_document_is_refused() {
    let versioned = |target: &str, version: &str| {
        PROVISION.replace(
            "target: TARGET\n",
            &format!("target: {target}\n  targetVersion: \"{version}\"\n"),
        )
    };

    // Through a real plugin process: the fake declares 1 and 2.4.
    let registry = registry();
    for version in ["1", "1.9", "2.4", "2.4.1"] {
        let report = run(
            &bundle(&versioned("fake", version)),
            &registry,
            Mode::Compile,
            &options("version-ok"),
            &mut Quiet,
        );
        assert_eq!(report.stages[0].outcome, Outcome::Compiled, "{version}");
    }
    for version in ["2", "2.5", "3", "10"] {
        assert_eq!(
            failure(outcome_of(
                &versioned("fake", version),
                &registry,
                "version-no"
            )),
            format!(
                "the document asks for targetVersion {version:?}, and target \"fake\" compiles \
                 for: 1, 2.4"
            )
        );
    }

    // The version is in the tree the plugin compiles.
    let registry = in_process(&["3"]);
    let report = run(
        &bundle(&versioned("iac", "3.1")),
        &registry,
        Mode::Compile,
        &options("version-tree"),
        &mut Quiet,
    );
    let tree = &report.stages[0].artifact.as_ref().unwrap().files["tree"];
    assert!(tree.contains("\ntargetVersion=3.1\n"), "{tree}");

    // A target that declares nothing cannot honour a version, and says so.
    let registry = in_process(&[]);
    assert_eq!(
        failure(outcome_of(
            &versioned("iac", "3"),
            &registry,
            "version-undeclared"
        )),
        "the document asks for targetVersion \"3\", but target \"iac\" does not say which \
         versions of its tool it compiles for, so the request cannot be honoured; remove \
         targetVersion"
    );
    // And a document that asks for none is unaffected.
    assert_eq!(
        outcome_of(
            &PROVISION.replace("TARGET", "iac"),
            &registry,
            "version-none"
        ),
        Outcome::Compiled
    );
}

const CONDITIONAL: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: cm
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: old_conf
      type: file
      when: data.legacy
    - id: conf
      type: file
      dependsOn:
        - resource: old_conf
          ifMissing: POLICY
    - id: banner
      type: file
      when: "data.env == 'prod'"
"#;

fn with_data(text: &str, data: &[(&str, Value)], test: &str) -> StageReport {
    let mut options = options(test);
    options.data = data
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    let report = run(
        &bundle(text),
        &in_process(&[]),
        Mode::Compile,
        &options,
        &mut Quiet,
    );
    report.stages[0].clone()
}

fn resources_in(report: &StageReport) -> Vec<String> {
    report.artifact.as_ref().unwrap().files["tree"]
        .lines()
        .filter_map(|line| line.strip_prefix("resource "))
        .map(|line| line.split(' ').next().unwrap().to_owned())
        .collect()
}

#[test]
fn a_when_decides_what_is_compiled_and_if_missing_decides_the_edges() {
    let prod = Value::String("prod".to_owned());
    let dev = Value::String("dev".to_owned());

    // Every condition holds: everything is compiled, in dependency order.
    let all = with_data(
        &CONDITIONAL.replace("POLICY", "error"),
        &[("legacy", Value::Bool(true)), ("env", prod.clone())],
        "when-all",
    );
    assert_eq!(resources_in(&all), ["old_conf", "conf", "banner"]);
    let tree = &all.artifact.as_ref().unwrap().files["tree"];
    assert!(
        tree.contains("edge old_conf -> conf kind=dependsOn"),
        "{tree}"
    );

    // A removed resource that nothing depends on simply is not there.
    let no_banner = with_data(
        &CONDITIONAL.replace("POLICY", "error"),
        &[("legacy", Value::Bool(true)), ("env", dev.clone())],
        "when-some",
    );
    assert_eq!(resources_in(&no_banner), ["old_conf", "conf"]);

    // A removed resource something depends on: refused by default...
    let refused = with_data(
        &CONDITIONAL.replace("POLICY", "error"),
        &[("legacy", Value::Bool(false)), ("env", dev.clone())],
        "when-error",
    );
    assert_eq!(
        failure(refused.outcome),
        "workflow.yaml:17:11: spec.resources[1].dependsOn[0].resource: resource \"conf\" \
         depends on \"old_conf\", which its `when` removed from this run; set ifMissing: \
         skip on this entry if the resource should carry on without it"
    );

    // ...and with skip, the edge goes and the resource stays.
    let skipped = with_data(
        &CONDITIONAL.replace("POLICY", "skip"),
        &[("legacy", Value::Bool(false)), ("env", dev)],
        "when-skip",
    );
    assert_eq!(resources_in(&skipped), ["conf"]);
    let tree = &skipped.artifact.as_ref().unwrap().files["tree"];
    assert!(!tree.contains("edge "), "{tree}");

    // Data the condition needs and was not given is an error, not a "no".
    let missing = with_data(&CONDITIONAL.replace("POLICY", "skip"), &[], "when-missing");
    assert!(failure(missing.outcome).contains("resource \"old_conf\": data.legacy is not set"),);
}

const LABELLED: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
  labels: {tier: net, env: prod}
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params: {content: 10.0.0.5}
  outputs:
    - name: address
      value: "${vm.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
  labels: {tier: web, env: prod}
spec:
  target: fake
  targetHost: {from: infrastructure, ref: network, output: address}
  resources:
    - id: motd
      type: file
      params: {content: web}
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: batch
spec:
  target: fake
  targetHost: {from: inline, host: batch01}
  resources:
    - id: motd
      type: file
      params: {content: batch}
"#;

fn names(report: &meridian_orchestrator::RunReport) -> Vec<&str> {
    report.stages.iter().map(|s| s.id.name.as_str()).collect()
}

#[test]
fn a_selector_chooses_which_documents_run() {
    let registry = registry();
    let bundle = bundle(LABELLED);
    let run_with = |selector: &str, mode: Mode, state: &Options| {
        let selector = Selector::parse(selector).unwrap();
        run_selected(&bundle, &registry, mode, state, &selector, &mut Quiet)
    };
    let state = options("selector");

    for (selector, want) in [
        ("tier=net", vec!["network"]),
        ("env=prod", vec!["network", "web"]),
        ("env=prod,tier!=net", vec!["web"]),
        ("!env", vec!["batch"]),
        ("tier", vec!["network", "web"]),
        ("tier=db", vec![]),
    ] {
        let report = run_with(selector, Mode::Compile, &state);
        assert_eq!(names(&report), want, "{selector}");
        let selector = Selector::parse(selector).unwrap();
        assert_eq!(selected(&bundle, &selector, &Data::new()), want.len());
    }
    assert_eq!(selected(&bundle, &Selector::all(), &Data::new()), 3);

    // A document left out is still there to be referred to. Selecting only
    // the web tier before the network exists defers it, as ever...
    let report = run_with("tier=web", Mode::Apply, &state);
    assert_eq!(names(&report), ["web"]);
    assert!(
        matches!(report.stages[0].outcome, Outcome::Deferred { .. }),
        "{:?}",
        report.stages[0].outcome
    );
    // ...and once the network has been applied on its own, the web tier
    // alone finds its host from what that apply left behind.
    let report = run_with("tier=net", Mode::Apply, &state);
    assert!(report.succeeded());
    let report = run_with("tier=web", Mode::Apply, &state);
    assert_eq!(names(&report), ["web"]);
    assert_eq!(report.stages[0].host, "10.0.0.5");
    assert!(
        matches!(report.stages[0].outcome, Outcome::Applied { .. }),
        "{:?}",
        report.stages[0].outcome
    );
}

const RESOLVED: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
  labels:
    env: ${{ data.env }}
spec:
  target: ${{ data.tool }}
  targetVersion: ${{ data.version ?? '3' }}
  variables:
    name: shop-${{ data.env }}
  conditions:
    is_prod: data.env == 'prod'
  resources:
    - id: vpc
      type: file
      params:
        name: ${{ var.name }}
        size: "${{ cond.is_prod ? 3 : 1 }}"
    - id: audit
      type: file
      when: cond.is_prod
    - id: gateway
      type: file
      dependsOn:
        - resource: vpc
          when: cond.is_prod
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: monitoring
  labels:
    env: ${{ data.env }}
    part: extra
spec:
  target: cm
  when: data.monitoring ?? false
  targetHost:
    from: inline
    host: ${{ var.host }}
  variables:
    host: mon.${{ data.env }}.example.com
  resources:
    - id: agent
      type: file
"#;

fn run_resolved(
    data: &[(&str, Value)],
    selector: &str,
    test: &str,
) -> meridian_orchestrator::RunReport {
    let mut options = options(test);
    options.data = data
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    run_selected(
        &bundle(RESOLVED),
        &in_process(&["3"]),
        Mode::Compile,
        &options,
        &if selector.is_empty() {
            Selector::all()
        } else {
            Selector::parse(selector).unwrap()
        },
        &mut Quiet,
    )
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

#[test]
fn variables_conditions_and_interpolation_are_settled_before_the_plugin_compiles() {
    let report = run_resolved(
        &[
            ("env", text("prod")),
            ("tool", text("iac")),
            ("monitoring", Value::Bool(true)),
        ],
        "",
        "resolved-prod",
    );
    assert!(report.succeeded(), "{report:#?}");
    let network = &report.stages[0];
    // The target itself came from data, and so did the version.
    assert_eq!(network.target, "iac");
    assert_eq!(resources_in(network), ["vpc", "audit", "gateway"]);
    let tree = &network.artifact.as_ref().unwrap().files["tree"];
    assert!(tree.contains("\ntargetVersion=3\n"), "{tree}");
    assert!(tree.contains("shop-prod"), "{tree}");
    assert!(
        tree.contains("edge vpc -> gateway kind=dependsOn"),
        "{tree}"
    );
    assert!(
        !tree.contains("${{"),
        "nothing is left for a plugin to decide: {tree}"
    );
    assert!(network.removed.is_empty());

    // The second document's host came from a variable.
    let monitoring = &report.stages[1];
    assert_eq!(monitoring.outcome, Outcome::Compiled);
    assert_eq!(monitoring.host, "mon.prod.example.com");
}

#[test]
fn what_a_condition_removed_is_reported_with_the_condition() {
    let report = run_resolved(
        &[("env", text("dev")), ("tool", text("iac"))],
        "",
        "resolved-dev",
    );
    assert!(report.succeeded(), "{report:#?}");
    let network = &report.stages[0];
    assert_eq!(resources_in(network), ["vpc", "gateway"]);
    let tree = &network.artifact.as_ref().unwrap().files["tree"];
    assert!(tree.contains("shop-dev"), "{tree}");
    assert!(!tree.contains("edge "), "{tree}");
    let removed: Vec<(&str, &str)> = network
        .removed
        .iter()
        .map(|r| (r.subject.as_str(), r.condition.as_str()))
        .collect();
    assert_eq!(
        removed,
        [
            ("resource \"audit\"", "cond.is_prod"),
            ("\"gateway\" depends on \"vpc\"", "cond.is_prod"),
        ]
    );
}

#[test]
fn a_document_its_own_when_leaves_out_is_skipped_and_the_run_still_succeeds() {
    let report = run_resolved(
        &[("env", text("dev")), ("tool", text("iac"))],
        "",
        "resolved-skipped",
    );
    let monitoring = &report.stages[1];
    assert_eq!(
        monitoring.outcome,
        Outcome::LeftOut {
            condition: "data.monitoring ?? false".to_owned()
        }
    );
    assert!(monitoring.artifact.is_none());
    assert!(report.succeeded());
}

#[test]
fn a_selector_matches_labels_as_they_are_once_interpolated() {
    let prod = [
        ("env", text("prod")),
        ("tool", text("iac")),
        ("monitoring", Value::Bool(true)),
    ];
    assert_eq!(
        names(&run_resolved(&prod, "env=prod", "sel-a")),
        ["network", "monitoring"]
    );
    assert_eq!(
        names(&run_resolved(&prod, "env=prod,part=extra", "sel-b")),
        ["monitoring"]
    );
    assert!(names(&run_resolved(&prod, "env=dev", "sel-c")).is_empty());
    let data: Data = prod
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    assert_eq!(
        selected(
            &bundle(RESOLVED),
            &Selector::parse("env=prod").unwrap(),
            &data
        ),
        2
    );
    assert_eq!(
        selected(
            &bundle(RESOLVED),
            &Selector::parse("env=dev").unwrap(),
            &data
        ),
        0
    );
}

#[test]
fn a_document_that_cannot_be_resolved_fails_its_stage_and_says_where() {
    // No data.env: the label cannot be decided, so the document stays a
    // stage and its error is reported, whatever the selector says.
    let report = run_resolved(&[("tool", text("iac"))], "env=prod", "resolved-unset");
    assert_eq!(names(&report), ["network", "monitoring"]);
    let why = failure(report.stages[0].outcome.clone());
    assert!(why.contains("data.env is not set"), "{why}");
    assert!(
        matches!(report.stages[1].outcome, Outcome::NotRun { .. }),
        "{:?}",
        report.stages[1].outcome
    );
    assert!(!report.succeeded());

    // A target that data names, and no plugin provides.
    let report = run_resolved(
        &[("env", text("prod")), ("tool", text("nosuch"))],
        "",
        "resolved-target",
    );
    let why = failure(report.stages[0].outcome.clone());
    assert!(
        why.contains("no plugin provides target \"nosuch\""),
        "{why}"
    );
}

#[test]
fn checking_what_a_document_declares_reads_no_value_and_runs_no_command() {
    use meridian_orchestrator::check_declared;
    use meridian_secret::Probe;
    let dir = std::env::temp_dir().join(format!("meridian-check-declared-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("ran");
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: site}}\nspec:\n  target: fake\n  \
         targetHost: {{from: inline, host: web01}}\n  secrets:\n    \
         - {{name: a, from: env, key: MERIDIAN_TEST_CHECK_DECLARED_NOT_SET}}\n    \
         - {{name: b, from: env, key: MERIDIAN_TEST_CHECK_DECLARED_NOT_SET, optional: true}}\n    \
         - {{name: c, from: exec, command: [touch, {}]}}\n    \
         - {{name: d, from: file, path: {}}}\n  \
         resources:\n    - {{id: motd, type: file, params: {{content: hi}}}}\n",
        marker.display(),
        dir.display()
    );
    let docs = meridian_ir::parse("site.yaml", text.as_bytes()).unwrap();
    let rows = check_declared(&docs[0]);
    let seen: Vec<(&str, &str, bool)> = rows
        .iter()
        .map(|row| (row.name.as_str(), row.from.as_str(), row.unanswered()))
        .collect();
    assert_eq!(
        seen,
        [
            ("a", "env", true),
            ("b", "env", false),
            ("c", "exec", false),
            ("d", "file", true)
        ]
    );
    assert!(matches!(rows[0].probe, Probe::Absent(_)));
    assert!(matches!(rows[2].probe, Probe::Unknown(_)));
    assert!(matches!(rows[3].probe, Probe::Refused(_)));
    assert!(!marker.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
