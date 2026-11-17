//! The plugin against the real tools.
//!
//! These tests do nothing unless `MERIDIAN_TEST_PODMAN` says how far to go:
//!
//! - `quadlet`: the machine has Podman, so every example is read by its own
//!   Quadlet. Nothing is installed or started.
//! - `system`: as above, and units are really installed in
//!   `/etc/containers/systemd`, started by systemd, changed and removed. It
//!   needs systemd, and root or passwordless `sudo`. CI runs it; do not run
//!   it on a machine you care about without reading it first.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_testkit::{graph_from_file, graph_from_text, scratch, KeptLog};
use meridian_sdk::{Artifact, Change, Data, Emitter, RunContext, Runner, Value};
use meridian_target_podman::{Podman, PodmanRunner, Scope};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn level() -> String {
    std::env::var("MERIDIAN_TEST_PODMAN").unwrap_or_default()
}

fn wants(needed: &str) -> bool {
    let level = level();
    let run = level == needed || (needed == "quadlet" && level == "system");
    if !run {
        eprintln!("skipped: set MERIDIAN_TEST_PODMAN={needed} to run this against the real Podman");
    }
    run
}

fn compiled(text: &str) -> Artifact {
    Podman
        .emit(&graph_from_text("real.yaml", text, "podman"), &Data::new())
        .unwrap()
        .artifact
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "real".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// The real tools, with the units kept in a scratch directory so a plan can
/// be made without touching the machine's own.
fn planner(dir: &Path) -> PodmanRunner {
    fs::create_dir_all(dir.join("units")).unwrap();
    PodmanRunner {
        unit_dir: Some(dir.join("units").to_str().unwrap().to_owned()),
        ..PodmanRunner::default()
    }
}

fn podman_major() -> u32 {
    let output = Command::new("podman")
        .arg("--version")
        .output()
        .expect("podman is installed");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .last()
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse().ok())
        .expect("podman --version ends with a version")
}

fn generator() -> PathBuf {
    [
        "/usr/libexec/podman/quadlet",
        "/usr/lib/podman/quadlet",
        "/usr/local/libexec/podman/quadlet",
        "/usr/lib/systemd/system-generators/podman-system-generator",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.exists())
    .expect("Podman's Quadlet is installed")
}

/// What the real Quadlet generates from an artifact's units.
fn generated(dir: &Path, artifact: &Artifact) -> String {
    let units = dir.join("generate");
    fs::create_dir_all(&units).unwrap();
    for (name, content) in &artifact.files {
        fs::write(units.join(name), content).unwrap();
    }
    let output = Command::new(generator())
        .args(["-dryrun", "-user"])
        .env("QUADLET_UNIT_DIRS", &units)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn shown(changes: &[Change]) -> Vec<String> {
    changes.iter().map(ToString::to_string).collect()
}

#[test]
fn the_real_quadlet_accepts_the_examples() {
    if !wants("quadlet") {
        return;
    }
    for (example, creates) in [("web-app", 4), ("edge-host", 3)] {
        let dir = scratch(TMP, &format!("real-{example}"));
        let graph = graph_from_file(&format!("examples/podman/{example}.yaml"), "podman");
        let artifact = Podman.emit(&graph, &Data::new()).unwrap().artifact;
        let log = KeptLog::default();
        let plan = planner(&dir)
            .plan(&context(&dir), &artifact, &log)
            .unwrap_or_else(|e| panic!("{example}: {e}"));
        assert_eq!(plan.changes.len(), creates, "{example}: {:?}", plan.changes);
        assert!(log.text().is_empty(), "{example}: {:?}", log.text());
    }
}

#[test]
fn a_pod_is_accepted_by_podman_5_and_refused_by_name_before_it() {
    if !wants("quadlet") {
        return;
    }
    let dir = scratch(TMP, "real-pod");
    let graph = graph_from_file("examples/podman/pod.yaml", "podman");
    let artifact = Podman.emit(&graph, &Data::new()).unwrap().artifact;
    let result = planner(&dir).plan(&context(&dir), &artifact, &KeptLog::default());
    if podman_major() >= 5 {
        assert_eq!(result.unwrap().changes.len(), 4);
    } else {
        let err = result.unwrap_err();
        assert!(
            err.msg
                .starts_with("metrics.pod would not become metrics-pod.service on this machine")
                && err
                    .msg
                    .ends_with("Quadlet learnt .pod units in Podman 5.0."),
            "{}",
            err.msg
        );
    }
}

#[test]
fn the_real_quadlet_refuses_a_key_it_does_not_know() {
    if !wants("quadlet") {
        return;
    }
    let dir = scratch(TMP, "real-refused");
    let artifact = compiled(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: real\nspec:\n  target: podman\n  resources:\n    - id: web\n      type: container\n      params:\n        Image: quay.io/x/web:1\n        NoSuchKeyEver: 1\n",
    );
    let err = planner(&dir)
        .plan(&context(&dir), &artifact, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "the Quadlet on this machine refused the units");
    assert!(
        err.output.contains("unsupported key 'NoSuchKeyEver'") && !err.output.contains("/tmp"),
        "{}",
        err.output
    );
}

#[test]
fn values_reach_podman_meaning_what_the_document_said() {
    if !wants("quadlet") {
        return;
    }
    let dir = scratch(TMP, "real-values");
    let artifact = compiled(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: real\nspec:\n  target: podman\n  resources:\n    - id: data\n      type: volume\n    - id: web\n      type: container\n      params:\n        Image: quay.io/x/web:1\n        Volume: ${data}:/srv\n        PublishPort: [\"8080:80\", \"8443:443\"]\n        Environment:\n          PASSWORD: \"50%off$now\"\n          NOTE: two words\n          QUOTED: 'say \"hi\"'\n        unit:\n          Description: Web\n",
    );
    let units = generated(&dir, &artifact);
    let start = units
        .lines()
        .find(|line| line.starts_with("ExecStart=") && line.contains("quay.io/x/web:1"))
        .unwrap_or_else(|| panic!("no ExecStart for the container in:\n{units}"));
    // systemd turns %% and $$ back into % and $, so the container sees the
    // password exactly as the document wrote it.
    assert!(start.contains("--env PASSWORD=50%%off$$now"), "{start}");
    assert!(start.contains("--env \"NOTE=two words\""), "{start}");
    assert!(start.contains("--env \"QUOTED=say \\\"hi\\\"\""), "{start}");
    assert!(start.contains("-v systemd-data:/srv"), "{start}");
    assert!(
        start.contains("--publish 8080:80") && start.contains("--publish 8443:443"),
        "{start}"
    );
    // The reference to the volume's unit file is what orders the container.
    assert!(units.contains("Requires=data-volume.service"), "{units}");
    assert!(units.contains("After=data-volume.service"), "{units}");
    assert!(units.contains("Description=Web"), "{units}");
    assert!(units.contains("WantedBy=default.target"), "{units}");
}

// ---- really installing and running units ----

const CONTAINER: &str = "meridian-ci-web";
const UNITS: &str = "/etc/containers/systemd";

fn document(resources: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: real\nspec:\n  target: podman\n  resources:\n    - id: meridian-ci-data\n      type: volume\n{resources}  outputs:\n    - name: data_directory\n      value: ${{meridian-ci-data.inspect.Mountpoint}}\n    - name: volume\n      value: ${{meridian-ci-data.name}}\n"
    )
}

fn web(extra: &str) -> String {
    format!(
        "    - id: {CONTAINER}\n      type: container\n      params:\n        Image: registry.k8s.io/pause:3.10\n        ContainerName: {CONTAINER}\n        Volume: ${{meridian-ci-data}}:/data\n{extra}        service:\n          TimeoutStartSec: 300\n"
    )
}

/// Runs a command as root and returns what it printed.
fn as_root(args: &[&str]) -> String {
    let output = Command::new("sudo").arg("-n").args(args).output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn running() -> Vec<String> {
    as_root(&["podman", "ps", "--format", "{{.Names}}"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Leaves the machine as it was found, whether the test passed or not.
struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        as_root(&["systemctl", "stop", "meridian-ci-web.service"]);
        as_root(&["systemctl", "stop", "meridian-ci-data-volume.service"]);
        as_root(&[
            "rm",
            "-f",
            &format!("{UNITS}/meridian-ci-web.container"),
            &format!("{UNITS}/meridian-ci-data.volume"),
        ]);
        as_root(&["systemctl", "daemon-reload"]);
        as_root(&["podman", "rm", "-f", CONTAINER]);
        as_root(&["podman", "volume", "rm", "-f", "systemd-meridian-ci-data"]);
    }
}

#[test]
fn units_are_really_installed_run_changed_and_removed() {
    if !wants("system") {
        return;
    }
    let dir = scratch(TMP, "real-system");
    let context = context(&dir);
    let runner = PodmanRunner {
        scope: Scope::System,
        ..PodmanRunner::default()
    };
    let _cleanup = Cleanup;
    drop(Cleanup);

    // A first apply installs both units and systemd starts the container.
    let first = compiled(&document(&web("")));
    let log = KeptLog::default();
    let plan = runner
        .plan(&context, &first, &log)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        shown(&plan.changes),
        [
            "create meridian-ci-data: install meridian-ci-data.volume and start meridian-ci-data-volume.service",
            "create meridian-ci-web: install meridian-ci-web.container and start meridian-ci-web.service",
        ]
    );
    assert!(
        !Path::new(UNITS).join("meridian-ci-web.container").exists(),
        "a plan installs nothing"
    );

    let applied = runner
        .apply(&context, &first, &log)
        .unwrap_or_else(|e| panic!("{e}\n{:?}", log.text()));
    assert_eq!(applied.changes.len(), 2);
    assert_eq!(
        fs::read_to_string(Path::new(UNITS).join("meridian-ci-web.container")).unwrap(),
        first.files["meridian-ci-web.container"]
    );
    assert!(running().contains(&CONTAINER.to_owned()), "{:?}", running());
    assert_eq!(
        as_root(&["systemctl", "is-active", "meridian-ci-web.service"]).trim(),
        "active"
    );

    // The outputs are what Podman itself says.
    let output = |name: &str| {
        applied
            .outputs
            .iter()
            .find(|o| o.name == name)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(
        output("volume"),
        Value::String("systemd-meridian-ci-data".into())
    );
    let Value::String(mount) = output("data_directory") else {
        panic!("the mount point is text");
    };
    assert_eq!(
        mount,
        as_root(&[
            "podman",
            "volume",
            "inspect",
            "--format",
            "{{.Mountpoint}}",
            "systemd-meridian-ci-data"
        ])
        .trim()
    );
    assert!(mount.starts_with('/'), "{mount}");
    assert_eq!(runner.outputs(&context).unwrap().len(), 2);

    // Nothing is left to do, and an apply then does nothing.
    let plan = runner.plan(&context, &first, &KeptLog::default()).unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
    let started = as_root(&[
        "podman",
        "inspect",
        "--format",
        "{{.State.StartedAt}}",
        CONTAINER,
    ]);
    assert!(runner
        .apply(&context, &first, &KeptLog::default())
        .unwrap()
        .changes
        .is_empty());
    assert_eq!(
        started,
        as_root(&[
            "podman",
            "inspect",
            "--format",
            "{{.State.StartedAt}}",
            CONTAINER
        ]),
        "the container was not restarted"
    );

    // A changed unit is rewritten and its container replaced.
    let second = compiled(&document(&web("        Label:\n          release: two\n")));
    let plan = runner.plan(&context, &second, &KeptLog::default()).unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["update meridian-ci-web: meridian-ci-web.container changed; restart meridian-ci-web.service"]
    );
    runner
        .apply(&context, &second, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        as_root(&[
            "podman",
            "inspect",
            "--format",
            "{{index .Config.Labels \"release\"}}",
            CONTAINER
        ])
        .trim(),
        "two"
    );
    assert!(runner
        .plan(&context, &second, &KeptLog::default())
        .unwrap()
        .in_sync());

    // A container somebody stopped is started again.
    as_root(&["systemctl", "stop", "meridian-ci-web.service"]);
    assert!(!running().contains(&CONTAINER.to_owned()));
    let plan = runner.plan(&context, &second, &KeptLog::default()).unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["update meridian-ci-web: meridian-ci-web.service is inactive; start it"]
    );
    runner
        .apply(&context, &second, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(running().contains(&CONTAINER.to_owned()));

    // A resource taken out of the document is stopped and its unit removed.
    let third = compiled(&document(""));
    let plan = runner.plan(&context, &third, &KeptLog::default()).unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["delete meridian-ci-web: stop meridian-ci-web.service and remove meridian-ci-web.container"]
    );
    runner
        .apply(&context, &third, &KeptLog::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!running().contains(&CONTAINER.to_owned()));
    assert!(!Path::new(UNITS).join("meridian-ci-web.container").exists());
    assert!(Path::new(UNITS).join("meridian-ci-data.volume").exists());
    assert!(runner
        .plan(&context, &third, &KeptLog::default())
        .unwrap()
        .in_sync());
}

#[test]
fn a_container_that_cannot_start_fails_the_apply_and_says_why() {
    if !wants("system") {
        return;
    }
    struct Remove;
    impl Drop for Remove {
        fn drop(&mut self) {
            as_root(&["systemctl", "stop", "meridian-ci-broken.service"]);
            as_root(&["rm", "-f", &format!("{UNITS}/meridian-ci-broken.container")]);
            as_root(&["systemctl", "daemon-reload"]);
        }
    }
    let _remove = Remove;
    let dir = scratch(TMP, "real-broken");
    let runner = PodmanRunner {
        scope: Scope::System,
        ..PodmanRunner::default()
    };
    // An image that is not there, and must not be looked for on a registry.
    let artifact = compiled(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: real\nspec:\n  target: podman\n  resources:\n    - id: meridian-ci-broken\n      type: container\n      params:\n        Image: localhost/meridian-ci/no-such-image:1\n        Pull: never\n",
    );
    let log = KeptLog::default();
    let err = runner.apply(&context(&dir), &artifact, &log).unwrap_err();
    assert_eq!(err.msg, "a unit did not start", "{err}");
    assert_eq!(err.exit_code, Some(75));
    assert_eq!(
        shown(&err.changes),
        ["create meridian-ci-broken: install meridian-ci-broken.container and start meridian-ci-broken.service"],
        "the unit file was written before the start failed"
    );
    assert!(
        err.output
            .contains("the journal of meridian-ci-broken.service"),
        "{}",
        err.output
    );
}
