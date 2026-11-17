//! The plugin against a real PowerShell, a real DscPipelineRunner and real
//! DSC engines.
//!
//! The resources are `MeridianTestDsc/MarkerFile`, a class-based DSC resource
//! that ships with these tests (`tests/modules`). Its state is a file under a
//! scratch directory, so the tests run on any machine and change nothing
//! else. The same resource runs through both engines: `Invoke-DscResource`
//! for DSC v2, and the `dsc` command line with its PowerShell adapter for
//! DSC v3.
//!
//! ```pwsh
//! Install-Module Dsc.PipelineRunner, Datum.InvokeCommand
//! $env:PSModulePath = "$PWD/plugins/dsc/tests/modules" + [IO.Path]::PathSeparator + $env:PSModulePath
//! $env:MERIDIAN_TEST_REAL_DSC = '1'
//! $env:MERIDIAN_TEST_REAL_DSC_V2 = '1'   # DSC v2; needs Windows
//! $env:MERIDIAN_TEST_REAL_DSC_V3 = '1'   # DSC v3; needs dsc on PATH
//! cargo test -p meridian-target-dsc --test real
//! ```
//!
//! Without `MERIDIAN_TEST_REAL_DSC=1` the tests pass without doing anything.
//! With it, a PowerShell that cannot import the module is a failure.
//!
//! `Invoke-DscResource` only works on Windows: on Linux, PowerShell's DSC
//! module needs a management library (`libmi`) that PowerShell does not ship.
//! So the `dsc` workflow asks for DSC v3 only on Linux, and for both engines
//! on Windows, where it runs these same tests. Nothing in this file is
//! specific to one operating system.

#![allow(clippy::result_large_err)]

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{graph_from_text, scratch, KeptLog};
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    Applied, Artifact, Change, ChangeAction, Data, Emitter, NoLog, Plan, RunContext, Runner,
};
use meridian_target_dsc::{Dsc, DscRunner};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn is_set(variable: &str) -> bool {
    std::env::var(variable).ok().as_deref() == Some("1")
}

/// One PowerShell command of the test's own.
fn pwsh(command: &str) -> Result<String, String> {
    let finished = Tool::new(DscRunner::from_env().program)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            command,
        ])
        .capture_stdout()
        .run(&NoLog)
        .map_err(|e| e.to_string())?;
    if finished.success() {
        Ok(finished.stdout)
    } else {
        Err(format!("{}{}", finished.stdout, finished.stderr))
    }
}

/// Whether to run the tests for an engine. Asked for and not installed is a
/// failure, not a skip.
fn engine(version: &str) -> bool {
    if !is_set("MERIDIAN_TEST_REAL_DSC") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_DSC=1 to run against a real DscPipelineRunner");
        return false;
    }
    let variable = format!("MERIDIAN_TEST_REAL_DSC_V{version}");
    if !is_set(&variable) {
        eprintln!("skipped: set {variable}=1 to run against DSC v{version}");
        return false;
    }
    let module = DscRunner::from_env().module;
    let import = format!(
        "$ErrorActionPreference = 'Stop'; Import-Module '{module}'; \
         if (-not (Get-Module -ListAvailable MeridianTestDsc)) {{ throw 'the test module MeridianTestDsc is not on PSModulePath' }}"
    );
    if let Err(why) = pwsh(&import) {
        panic!("MERIDIAN_TEST_REAL_DSC=1, and PowerShell cannot load what the tests need: {why}");
    }
    let installed = match version {
        "2" => "$ErrorActionPreference = 'Stop'; Import-Module PSDesiredStateConfiguration; $null = Get-Command Invoke-DscResource",
        _ => "dsc --version; if ($LASTEXITCODE -ne 0) { exit 1 }",
    };
    if let Err(why) = pwsh(installed) {
        panic!("{variable}=1, and that engine is not usable here: {why}");
    }
    true
}

/// The first engine these tests were asked to use, for a test that needs
/// one and does not care which.
fn any_engine() -> Option<&'static str> {
    ["3", "2"].into_iter().find(|version| engine(version))
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "site".into(),
        host: "localhost".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn compile(version: &str, resources: &str) -> Artifact {
    let text = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: dsc\n  targetVersion: \"{version}\"\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n{resources}"
    );
    Dsc.emit(&graph_from_text("site.yaml", &text, "dsc"), &Data::new())
        .unwrap_or_else(|e| panic!("{e}"))
        .artifact
}

fn plan(dir: &Path, artifact: &Artifact) -> Plan {
    let log = KeptLog::default();
    DscRunner::from_env()
        .plan(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn apply(dir: &Path, artifact: &Artifact) -> Applied {
    let log = KeptLog::default();
    DscRunner::from_env()
        .apply(&context(dir), artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")))
}

fn changes(list: &[Change]) -> Vec<(&str, ChangeAction)> {
    list.iter()
        .map(|c| (c.resource.as_str(), c.action))
        .collect()
}

/// A path as a YAML document can hold it on any system.
fn shown(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// How many times a marker's `Set` really ran.
fn sets(state: &Path, name: &str) -> usize {
    fs::read_to_string(state.join(format!("{name}.sets")))
        .map(|log| log.lines().count())
        .unwrap_or(0)
}

/// A configuration file, a service that is notified when it changes, and
/// something that depends on the service.
fn site(version: &str, state: &Path, config: &str) -> Artifact {
    let state = shown(state);
    compile(
        version,
        &format!(
            "    - id: config\n      type: MeridianTestDsc/MarkerFile\n      params:\n        Name: config\n        Path: {state}\n        Content: {config}\n        Ensure: Present\n      notifies:\n        - resource: service\n          action: refresh\n    - id: service\n      type: MeridianTestDsc/MarkerFile\n      params:\n        Name: service\n        Path: {state}\n        Content: running\n        Ensure: Present\n    - id: page\n      type: MeridianTestDsc/MarkerFile\n      params:\n        Name: page\n        Path: {state}\n        Content: hello\n        Ensure: Present\n      dependsOn:\n        - resource: service\n"
        ),
    )
}

/// The whole life of a document on one engine.
fn converge(version: &str) {
    let dir = scratch(TMP, &format!("real-v{version}"));
    let state = dir.join("state");
    fs::create_dir_all(&state).unwrap();
    let first = site(version, &state, "v1");

    // A plan tests every resource and sets none.
    let preview = plan(&dir, &first);
    assert!(!preview.unknown);
    assert_eq!(
        changes(&preview.changes),
        [
            ("config", ChangeAction::Update),
            ("service", ChangeAction::Update),
            ("page", ChangeAction::Update),
        ]
    );
    assert!(
        !state.join("config.marker").exists(),
        "a plan changes nothing"
    );

    // An apply sets what had drifted, which is all of it.
    let applied = apply(&dir, &first);
    assert_eq!(changes(&applied.changes), changes(&preview.changes));
    assert!(applied.outputs.is_empty(), "DSC reports no outputs");
    assert_eq!(
        fs::read_to_string(state.join("config.marker")).unwrap(),
        "v1"
    );
    assert_eq!(
        fs::read_to_string(state.join("service.marker")).unwrap(),
        "running"
    );
    assert_eq!(
        fs::read_to_string(state.join("page.marker")).unwrap(),
        "hello"
    );
    assert_eq!(sets(&state, "service"), 1);

    // In its desired state: nothing to change, and an apply sets nothing.
    let again = plan(&dir, &first);
    assert!(again.changes.is_empty(), "{:?}", again.changes);
    let applied = apply(&dir, &first);
    assert!(applied.changes.is_empty(), "{:?}", applied.changes);
    assert_eq!(sets(&state, "config"), 1);
    assert_eq!(sets(&state, "service"), 1);

    // The configuration changes. Only it has drifted, so only it is a
    // change; but it notifies the service, whose Set runs again although the
    // service is already in its desired state. That is what refresh means.
    let second = site(version, &state, "v2");
    let preview = plan(&dir, &second);
    assert_eq!(
        changes(&preview.changes),
        [("config", ChangeAction::Update)]
    );
    let applied = apply(&dir, &second);
    assert_eq!(
        changes(&applied.changes),
        [("config", ChangeAction::Update)]
    );
    assert_eq!(
        fs::read_to_string(state.join("config.marker")).unwrap(),
        "v2"
    );
    assert_eq!(sets(&state, "config"), 2);
    assert_eq!(
        sets(&state, "service"),
        2,
        "the notified resource was set again"
    );
    assert_eq!(sets(&state, "page"), 1, "nothing notified this one");

    // Drift is seen: a file removed behind DSC's back is found by a plan.
    fs::remove_file(state.join("page.marker")).unwrap();
    let preview = plan(&dir, &second);
    assert_eq!(changes(&preview.changes), [("page", ChangeAction::Update)]);

    // Desired state is a property of a DSC resource, so absent is too.
    let shown_state = shown(&state);
    let gone = compile(
        version,
        &format!(
            "    - id: config\n      type: MeridianTestDsc/MarkerFile\n      params:\n        Name: config\n        Path: {shown_state}\n        Ensure: Absent\n"
        ),
    );
    apply(&dir, &gone);
    assert!(!state.join("config.marker").exists());
}

#[test]
fn a_document_converges_through_dsc_v2() {
    if engine("2") {
        converge("2");
    }
}

#[test]
fn a_document_converges_through_dsc_v3() {
    if engine("3") {
        converge("3");
    }
}

#[test]
fn a_runtime_condition_is_evaluated_by_the_runner() {
    let Some(version) = any_engine() else { return };
    let dir = scratch(TMP, "real-condition");
    let state = dir.join("state");
    fs::create_dir_all(&state).unwrap();
    let shown_state = shown(&state);
    // The document's name is the node's name, so one condition holds and
    // the other does not.
    let artifact = compile(
        version,
        &format!(
            "    - id: wanted\n      type: MeridianTestDsc/MarkerFile\n      params:\n        Name: wanted\n        Path: {shown_state}\n      runtimeWhen: \"equals (nodeName) 'site'\"\n    - id: unwanted\n      type: MeridianTestDsc/MarkerFile\n      params:\n        Name: unwanted\n        Path: {shown_state}\n      runtimeWhen: \"equals (nodeName) 'elsewhere'\"\n"
        ),
    );
    let preview = plan(&dir, &artifact);
    assert_eq!(
        changes(&preview.changes),
        [("wanted", ChangeAction::Update)]
    );
    apply(&dir, &artifact);
    assert!(state.join("wanted.marker").exists());
    assert!(
        !state.join("unwanted.marker").exists(),
        "a resource whose condition is false is skipped"
    );
}

#[test]
fn a_resource_dsc_does_not_have_fails_with_the_runners_reason() {
    let Some(version) = any_engine() else { return };
    let dir = scratch(TMP, "real-missing");
    // Whether a module and a resource exist is only known to the engine.
    let artifact = compile(
        version,
        "    - id: ghost\n      type: MeridianNoSuchModule/NoSuchResource\n      params:\n        Name: ghost\n",
    );
    let log = KeptLog::default();
    match DscRunner::from_env().apply(&context(&dir), &artifact, &log) {
        Err(error) => {
            let said = format!("{} {} {}", error.msg, error.output, log.text().join("\n"));
            assert!(
                said.contains("NoSuchResource") || said.contains("MeridianNoSuchModule"),
                "{said}"
            );
        }
        Ok(applied) => panic!("a resource that does not exist was applied: {applied:?}"),
    }
}
