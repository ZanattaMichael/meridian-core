//! Analyzers over a stage's compiled files, run through a real plugin
//! process and a real analyzer program.
//!
//! The analyzer here is a shell script that speaks the protocol and stands
//! in for a scanner: each line of its rules file (named by the setting
//! `rules`) is `needle|rule|severity|resource`, and a rule whose needle is
//! in one of the compiled files answers with a finding in that file. It
//! also keeps every input it was given beside the rules file, which a real
//! analyzer has no reason to do, so that the tests can read what an
//! analyzer is told and where its files were.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use meridian_ir::Bundle;
use meridian_orchestrator::{
    event, run, Analyzer, Baseline, Event, FailOn, Mode, Observer, Options, Outcome, Quiet, Reason,
    RunReport, RunState, Severity, StageId, StageKind,
};
use meridian_plugin_host::{Registry, StartOptions};
use meridian_sdk::{decode_value, Value};

const ANALYZER: &str = r#"#!/bin/sh
input=$(cat)
rules="$MERIDIAN_ANALYZE_RULES"
printf '%s\n' "$input" >> "$(dirname "$rules")/seen.jsonl"
printf '%s\n' "$PWD" >> "$(dirname "$rules")/dirs.txt"
findings=""
while IFS='|' read -r needle rule severity resource; do
  case "$needle" in
    '#exit') echo "the scanner broke" >&2; exit "$rule" ;;
    '#garbage') echo "certainly"; exit 0 ;;
    '#measure') findings="$findings"; measure="{\"name\":\"$rule\",\"value\":$severity,\"unit\":\"$resource\"}"; continue ;;
  esac
  for file in $(find . -type f ! -name .meridian-files | sort); do
    line=$(grep -nF -- "$needle" "$file" | head -1 | cut -d: -f1)
    if [ -n "$line" ]; then
      path=${file#./}
      findings="$findings${findings:+,}{\"rule\":\"$rule\",\"severity\":\"$severity\",\"message\":\"$rule says no to $needle\",\"file\":\"$path\",\"line\":$line,\"resource\":\"$resource\"}"
    fi
  done
done < "$rules"
printf '{"findings":[%s],"measures":[%s]}\n' "$findings" "$measure"
"#;

const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
  labels: {env: prod}
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: "vm on 10.0.0.5"
    - id: logs
      type: file
      params:
        content: "bucket logs is public"
  outputs:
    - name: address
      value: "10.0.0.5"
---
apiVersion: meridian/v1
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
        content: "welcome"
"#;

fn workflow() -> Bundle {
    let bundle = Bundle::new(meridian_ir::parse("workflow.yaml", WORKFLOW.as_bytes()).unwrap());
    bundle.validate().unwrap();
    bundle
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let dir = meridian_target_fake::plugin_dir();
    registry
        .discover(dir.to_str().unwrap(), &StartOptions::default())
        .expect("the fake plugin loads");
    registry
}

struct Case {
    dir: PathBuf,
    options: Options,
}

/// A scratch directory holding the analyzer, its rules, and the state.
fn setup(test: &str, rules: &str) -> Case {
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("analyze")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::create_dir_all(dir.join("rules")).unwrap();
    let program = dir.join("bin/meridian-analyze-lines");
    fs::write(&program, ANALYZER).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let rules_file = dir.join("rules/rules");
    fs::write(&rules_file, rules).unwrap();
    let mut analyzer = Analyzer::find("lines", &[dir.join("bin")]).unwrap();
    analyzer
        .settings
        .insert("rules".to_owned(), rules_file.display().to_string());
    let options = Options {
        analyzers: vec![analyzer],
        ..Options::new(dir.join("state"))
    };
    Case { dir, options }
}

impl Case {
    fn run(&self, mode: Mode) -> RunReport {
        run(&workflow(), &registry(), mode, &self.options, &mut Quiet)
    }

    fn run_recorded(&self, mode: Mode) -> (RunReport, Recorder) {
        let mut recorder = Recorder { events: Vec::new() };
        let report = run(&workflow(), &registry(), mode, &self.options, &mut recorder);
        (report, recorder)
    }

    /// Every input the analyzer was given, in order.
    fn seen(&self) -> Vec<Value> {
        fs::read_to_string(self.dir.join("rules/seen.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| decode_value(line.as_bytes()).unwrap())
            .collect()
    }

    /// Every directory the analyzer was started in.
    fn dirs(&self) -> Vec<PathBuf> {
        fs::read_to_string(self.dir.join("rules/dirs.txt"))
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .collect()
    }
}

struct Recorder {
    events: Vec<Event>,
}

impl Observer for Recorder {
    fn event(&mut self, event: &Event) {
        self.events.push(event.clone());
    }
}

fn network() -> StageId {
    StageId {
        kind: StageKind::Infrastructure,
        name: "network".to_owned(),
    }
}

fn failed_with(report: &RunReport, index: usize) -> &str {
    match &report.stages[index].outcome {
        Outcome::Failed(why) => why,
        other => panic!("stage {index} did not fail: {other:?}"),
    }
}

#[test]
fn a_compile_is_analyzed_and_a_finding_is_traced_to_the_document() {
    let case = setup(
        "compile",
        "public|PUBLIC-BUCKET|medium|\nwelcome|GREETING|low|motd\n",
    );
    let (report, recorder) = case.run_recorded(Mode::Compile);
    assert!(report.succeeded(), "{report:?}");

    // The first stage: one finding, traced to the resource whose id is on
    // the line, and to the line of the document that resource starts on.
    let network = &report.stages[0];
    assert_eq!(network.analyses.len(), 1);
    let analysis = &network.analyses[0];
    assert!(analysis.asked && analysis.error.is_empty());
    assert_eq!(analysis.findings.len(), 1);
    let finding = &analysis.findings[0];
    assert_eq!(finding.rule, "PUBLIC-BUCKET");
    assert_eq!(finding.severity, Severity::Medium);
    assert_eq!(finding.file, "files/logs");
    assert_eq!(finding.line, 1);
    assert_eq!(finding.resource, "logs", "traced from the compiled line");
    assert_eq!(finding.document, "workflow.yaml");
    assert_eq!(finding.document_line, 13);
    assert_eq!(finding.place(), "workflow.yaml:13 (files/logs:1)");
    assert!(!finding.accepted);
    assert!(analysis.passed(Some(Severity::High)));

    // The second waits on the first's address, so a compile cannot look
    // at it, and nothing was analyzed.
    let web = &report.stages[1];
    assert!(
        matches!(web.outcome, Outcome::Deferred { .. }),
        "{:?}",
        web.outcome
    );
    assert!(web.analyses.is_empty());

    // What the analyzer was told, and where it ran.
    let seen = case.seen();
    assert_eq!(seen.len(), 1);
    let first = seen[0].as_map().unwrap();
    assert_eq!(
        first["run"].as_map().unwrap()["mode"],
        Value::from("compile")
    );
    let stage = first["stage"].as_map().unwrap();
    assert_eq!(stage["name"], Value::from("network"));
    assert_eq!(stage["kind"], Value::from("Infrastructure"));
    assert_eq!(stage["target"], Value::from("fake"));
    assert_eq!(
        stage["labels"].as_map().unwrap()["env"],
        Value::from("prod")
    );
    let artifact = first["artifact"].as_map().unwrap();
    let files: Vec<&str> = artifact["files"]
        .as_list()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(files, ["files/logs", "files/vm", "outputs/address"]);
    let dir = PathBuf::from(artifact["dir"].as_str().unwrap());
    assert_eq!(case.dirs()[0], dir);
    assert!(
        !dir.exists(),
        "the compiled files are removed once the analyzer has looked"
    );
    assert_eq!(
        first["settings"].as_map().unwrap()["rules"],
        Value::from(case.dir.join("rules/rules").display().to_string().as_str())
    );

    // A compile keeps no record, but the observer hears every finding.
    let kinds: Vec<&str> = recorder.events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            event::RUN_STARTED,
            event::STEP_STARTED,
            event::FINDING_REPORTED,
            event::ANALYSIS_PASSED,
            event::STEP_SUCCEEDED,
            event::STEP_STARTED,
            event::STEP_DEFERRED,
            event::RUN_SUCCEEDED,
        ]
    );
    let found = &recorder.events[2];
    assert_eq!(found.step, "network");
    assert_eq!(found.detail["analyzer"], Value::from("lines"));
    assert_eq!(found.detail["rule"], Value::from("PUBLIC-BUCKET"));
    assert_eq!(found.detail["severity"], Value::from("medium"));
    assert_eq!(found.detail["resource"], Value::from("logs"));
    assert_eq!(found.detail["document"], Value::from("workflow.yaml"));
    assert_eq!(found.detail["documentLine"], Value::Uint(13));
    assert_eq!(found.detail["accepted"], Value::Bool(false));
    let passed = &recorder.events[3];
    assert_eq!(passed.detail["findings"], Value::Uint(1));
    assert_eq!(passed.detail["medium"], Value::Uint(1));
    assert_eq!(passed.detail["failing"], Value::Uint(0));
    assert_eq!(passed.detail["asked"], Value::Bool(true));
}

#[test]
fn a_finding_at_the_threshold_fails_the_stage_and_a_plan_goes_on() {
    let case = setup("threshold", "public|PUBLIC-BUCKET|high|\n");
    let (report, recorder) = case.run_recorded(Mode::Plan);
    assert!(!report.succeeded());
    assert_eq!(report.stages[0].reason, Some(Reason::AnalysisFailed));
    let why = failed_with(&report, 0);
    assert!(
        why.contains("severity high or above") && why.contains("--fail-on high"),
        "{why}"
    );
    assert!(
        why.contains("high PUBLIC-BUCKET (lines) at workflow.yaml:13 (files/logs:1): PUBLIC-BUCKET says no to public"),
        "{why}"
    );
    // The plan went on to the next stage, which waits on the first.
    assert!(
        matches!(report.stages[1].outcome, Outcome::Deferred { .. }),
        "{:?}",
        report.stages[1].outcome
    );
    let failed = recorder
        .events
        .iter()
        .find(|e| e.kind == event::ANALYSIS_FAILED)
        .unwrap();
    assert_eq!(failed.detail["failing"], Value::Uint(1));
    assert_eq!(failed.detail["high"], Value::Uint(1));
    assert_eq!(
        recorder
            .events
            .iter()
            .find(|e| e.kind == event::STEP_FAILED)
            .unwrap()
            .detail["reason"],
        Value::from("analysis-failed")
    );

    // Lower the bar, and the same finding is only reported.
    let mut lenient = Options {
        fail_on: FailOn::AtLeast(Severity::Critical),
        ..case.options.clone()
    };
    let report = run(&workflow(), &registry(), Mode::Plan, &lenient, &mut Quiet);
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(report.stages[0].analyses[0].findings.len(), 1);
    lenient.fail_on = FailOn::Never;
    let report = run(&workflow(), &registry(), Mode::Plan, &lenient, &mut Quiet);
    assert!(report.succeeded(), "{report:?}");
}

#[test]
fn an_apply_analyzes_every_stage_first_and_applies_nothing_on_a_failure() {
    let case = setup("apply", "welcome|GREETING|critical|motd\n");
    let (report, recorder) = case.run_recorded(Mode::Apply);
    assert!(!report.succeeded());
    // The second stage waits on the first's output, so the first pass
    // could not look at it, and the first stage was applied before the
    // second was analyzed at its turn.
    assert!(matches!(report.stages[0].outcome, Outcome::Applied { .. }));
    assert_eq!(report.stages[1].reason, Some(Reason::AnalysisFailed));
    // The analyzer named the resource itself, and the finding points at the
    // line of the document it was written on.
    let finding = &report.stages[1].analyses[0].findings[0];
    assert_eq!(finding.rule, "GREETING");
    assert_eq!(finding.resource, "motd");
    assert_eq!(finding.document, "workflow.yaml");
    assert_eq!(finding.document_line, 32);
    assert!(recorder
        .events
        .iter()
        .any(|e| e.kind == event::ANALYSIS_FAILED && e.step == "web"));

    // A finding in a stage the first pass can look at stops the run before
    // anything is applied, and it is looked at once.
    let case = setup("apply-first", "public|PUBLIC-BUCKET|critical|\n");
    let report = case.run(Mode::Apply);
    assert!(!report.succeeded());
    assert_eq!(report.stages[0].reason, Some(Reason::AnalysisFailed));
    assert!(matches!(report.stages[1].outcome, Outcome::NotRun { .. }));
    let state = RunState::read(case.options.store().as_ref()).unwrap();
    assert!(state.get(&network()).is_none(), "nothing was applied");
    assert_eq!(case.seen().len(), 1, "the first pass looked, and once");

    // With nothing to find, the first pass's analysis is reused at the
    // stage's turn rather than run again.
    let case = setup("apply-clean", "nothing-here|NOTHING|high|\n");
    let report = case.run(Mode::Apply);
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(
        case.seen().len(),
        2,
        "network once in the first pass, web once at its turn"
    );
    assert!(report
        .stages
        .iter()
        .all(|s| s.analyses[0].findings.is_empty()));
}

#[test]
fn a_baseline_accepts_a_finding_without_hiding_it() {
    let case = setup("baseline", "public|PUBLIC-BUCKET|high|\n");
    let baseline = Baseline::from_value(
        &decode_value(
            br#"{"accepted": [{"rule": "PUBLIC-BUCKET", "resource": "logs", "reason": "a public bucket on purpose"}]}"#,
        )
        .unwrap(),
    )
    .unwrap();
    let options = Options {
        baseline,
        ..case.options.clone()
    };
    let mut recorder = Recorder { events: Vec::new() };
    let report = run(
        &workflow(),
        &registry(),
        Mode::Plan,
        &options,
        &mut recorder,
    );
    assert!(report.succeeded(), "{report:?}");
    let finding = &report.stages[0].analyses[0].findings[0];
    assert!(finding.accepted);
    let found = recorder
        .events
        .iter()
        .find(|e| e.kind == event::FINDING_REPORTED)
        .unwrap();
    assert_eq!(found.detail["accepted"], Value::Bool(true));
    let passed = recorder
        .events
        .iter()
        .find(|e| e.kind == event::ANALYSIS_PASSED)
        .unwrap();
    assert_eq!(passed.detail["accepted"], Value::Uint(1));
    assert_eq!(passed.detail["findings"], Value::Uint(0));
}

#[test]
fn an_analyzer_that_cannot_look_fails_the_stage() {
    let case = setup("broken", "#exit|3\n");
    let report = case.run(Mode::Plan);
    assert!(!report.succeeded());
    assert_eq!(report.stages[0].reason, Some(Reason::AnalyzerFailed));
    let why = failed_with(&report, 0);
    assert!(
        why.contains("could not look at this stage")
            && why.contains("exited with status 3: the scanner broke"),
        "{why}"
    );
    assert!(!report.stages[0].analyses[0].asked);

    let case = setup("garbage", "#garbage|\n");
    let report = case.run(Mode::Compile);
    assert_eq!(report.stages[0].reason, Some(Reason::AnalyzerFailed));
    assert!(failed_with(&report, 0).contains("is not JSON"));

    let gone = Options {
        analyzers: vec![Analyzer {
            name: "gone".to_owned(),
            program: PathBuf::from("/nonexistent/meridian-analyze-gone"),
            settings: Default::default(),
        }],
        ..Options::new(case.dir.join("state-gone"))
    };
    let report = run(&workflow(), &registry(), Mode::Compile, &gone, &mut Quiet);
    assert_eq!(report.stages[0].reason, Some(Reason::AnalyzerFailed));
    assert!(failed_with(&report, 0).contains("starting"));
}

#[test]
fn a_measure_is_carried_with_the_analysis() {
    let case = setup("measure", "#measure|monthly-cost|12.5|USD\n");
    let (report, recorder) = case.run_recorded(Mode::Compile);
    assert!(report.succeeded(), "{report:?}");
    let measures = &report.stages[0].analyses[0].measures;
    assert_eq!(measures.len(), 1);
    assert_eq!(measures[0].name, "monthly-cost");
    assert_eq!(measures[0].value, 12.5);
    assert_eq!(measures[0].unit, "USD");
    let passed = recorder
        .events
        .iter()
        .find(|e| e.kind == event::ANALYSIS_PASSED)
        .unwrap();
    let carried = passed.detail["measures"].as_list().unwrap()[0]
        .as_map()
        .unwrap()
        .clone();
    assert_eq!(carried["value"], Value::Float(12.5));
}

#[test]
fn an_analyzer_is_found_by_name_and_a_bad_name_is_refused() {
    let case = setup("find", "");
    let found = Analyzer::find("lines", &[case.dir.join("bin")]).unwrap();
    assert_eq!(found.program, case.dir.join("bin/meridian-analyze-lines"));
    let missing = Analyzer::find("none", &[case.dir.join("bin")]).unwrap_err();
    assert!(
        missing.contains("no program named meridian-analyze-none"),
        "{missing}"
    );
    let unsafe_name = Analyzer::find("../x", &[case.dir.join("bin")]).unwrap_err();
    assert!(
        unsafe_name.contains("is not an analyzer name"),
        "{unsafe_name}"
    );
}
