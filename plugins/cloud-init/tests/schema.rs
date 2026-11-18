//! Every user-data this plugin's tests record, checked by cloud-init itself.
//!
//! The other tests compare bytes with bytes. This one asks the tool that
//! will read them. It needs a real `cloud-init`, which most machines that
//! build Meridian do not have, so without one it says so and passes; the
//! `cloud-init` workflow installs one and sets
//! `MERIDIAN_TEST_REAL_CLOUD_INIT`, which turns a missing tool into a
//! failure.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_testkit::{artifact, scratch, KeptLog};
use meridian_sdk::{RunContext, Runner};
use meridian_target_cloud_init::{CloudInitRunner, PROGRAM_VARIABLE};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

const REQUIRED_VARIABLE: &str = "MERIDIAN_TEST_REAL_CLOUD_INIT";

/// The real cloud-init to test with, if this machine has one.
fn cloud_init() -> Option<String> {
    let program = std::env::var(PROGRAM_VARIABLE).unwrap_or_else(|_| "cloud-init".to_owned());
    let found = Command::new(&program)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if found {
        return Some(program);
    }
    assert!(
        std::env::var_os(REQUIRED_VARIABLE).is_none(),
        "{REQUIRED_VARIABLE} is set, and {program} could not be run"
    );
    eprintln!("skipped: {program} is not installed on this machine");
    None
}

fn recorded_user_data() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "user-data"))
        .collect();
    found.sort();
    assert!(found.len() >= 5, "{found:?}");
    found
}

fn schema(program: &str, file: &Path) -> std::process::Output {
    Command::new(program)
        .args(["schema", "--config-file"])
        .arg(file)
        .output()
        .expect("cloud-init runs")
}

#[ignore = "needs a real cloud-init; see docs/contributing/real-tool-tests.md"]
#[test]
fn cloud_init_accepts_every_recorded_user_data() {
    let Some(program) = cloud_init() else { return };
    for file in recorded_user_data() {
        let out = schema(&program, &file);
        assert!(
            out.status.success(),
            "cloud-init rejected {}:\n{}{}",
            file.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The control: if cloud-init accepted anything at all, the test above would
/// prove nothing.
#[ignore = "needs a real cloud-init; see docs/contributing/real-tool-tests.md"]
#[test]
fn cloud_init_rejects_user_data_that_is_wrong() {
    let Some(program) = cloud_init() else { return };
    let dir = scratch(env!("CARGO_TARGET_TMPDIR"), "schema-control");
    let file = dir.join("user-data");
    fs::write(&file, "#cloud-config\npackages: 4\nwrite_files: nothing\n").unwrap();
    let out = schema(&program, &file);
    assert!(
        !out.status.success(),
        "cloud-init accepted user-data whose packages is a number:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[ignore = "needs a real cloud-init; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_runner_checks_with_the_real_tool() {
    let Some(program) = cloud_init() else { return };
    let dir = scratch(env!("CARGO_TARGET_TMPDIR"), "schema-runner");
    let runner = CloudInitRunner {
        program,
        validate: true,
        wait: None,
        misconfigured: None,
    };
    let context = RunContext {
        document: "everything".into(),
        host: "192.0.2.10".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };
    let recorded = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let user_data = fs::read_to_string(recorded.join("all_features.user-data")).unwrap();
    let meta_data = fs::read_to_string(recorded.join("all_features.meta-data")).unwrap();

    let good = artifact(&[("user-data", &user_data), ("meta-data", &meta_data)]);
    let applied = runner.apply(&context, &good, &KeptLog::default()).unwrap();
    assert_eq!(applied.changes.len(), 1);

    let bad = artifact(&[
        ("user-data", "#cloud-config\npackages: 4\n"),
        ("meta-data", &meta_data),
    ]);
    let err = runner
        .plan(&context, &bad, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "cloud-init rejected the user-data");
    // The seed still holds what was accepted.
    assert_eq!(
        fs::read_to_string(dir.join("work/seed/user-data")).unwrap(),
        user_data
    );
}

/// The schema source against the published cloud-config schema, at the
/// tag of the installed cloud-init. Needs the network; without cloud-init
/// it reads the main branch and says so.
#[ignore = "needs the network; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_cloud_config_schema_is_exported_for_an_editor() {
    use meridian_plugin_testkit::graph_from_text;
    use meridian_sdk::{ParamKind, SchemaRequest, SchemaSource};
    use meridian_target_cloud_init::CloudInitSchemas;
    if std::env::var(REQUIRED_VARIABLE).ok().as_deref() != Some("1") {
        eprintln!("skipped: set {REQUIRED_VARIABLE}=1 to fetch the real cloud-config schema");
        return;
    }
    // The schema is fetched whether or not cloud-init is installed; its
    // version only chooses the tag.
    let program = std::env::var(PROGRAM_VARIABLE).unwrap_or_else(|_| "cloud-init".to_owned());
    let installed = Command::new(&program)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
        .then_some(program);
    let dir = scratch(TMP, "real-schemas");
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: cloud-init\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: clock\n      type: ntp\n      params:\n        servers: [0.pool.ntp.org]\n    - id: zone\n      type: timezone\n      params:\n        value: Europe/Amsterdam\n    - id: nowhere\n      type: no_such_module\n      params:\n        value: 1\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("site.yaml", text, "cloud-init")],
    );
    let log = KeptLog::default();
    let export = CloudInitSchemas::from_env()
        .export(&request, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(export.tool, "cloud-init");
    match &installed {
        Some(_) => assert!(
            export
                .tool_version
                .starts_with(|c: char| c.is_ascii_digit()),
            "the installed version: {}",
            export.tool_version
        ),
        None => assert_eq!(export.tool_version, "main"),
    }
    assert_eq!(export.pins.get("cloud-init"), Some(&export.tool_version));
    assert!(export.pins.contains_key("schema"));
    let ntp = export
        .schemas
        .iter()
        .find(|s| s.r#type == "ntp")
        .unwrap_or_else(|| panic!("ntp is described: {:?}", export.notes));
    assert!(ntp
        .find_param("servers")
        .is_some_and(|p| p.kind == ParamKind::List));
    assert!(ntp
        .find_param("enabled")
        .is_some_and(|p| p.kind == ParamKind::Bool));
    let timezone = export
        .schemas
        .iter()
        .find(|s| s.r#type == "timezone")
        .unwrap();
    assert_eq!(timezone.params.len(), 1);
    assert_eq!(timezone.params[0].name, "value");
    assert_eq!(timezone.params[0].kind, ParamKind::String);
    // The keys the shared types write are not modules.
    for shared in ["packages", "write_files", "users", "groups", "runcmd"] {
        assert!(
            !export.schemas.iter().any(|s| s.r#type == shared),
            "{shared}"
        );
    }
    assert!(
        export.schemas.len() > 40,
        "{} modules",
        export.schemas.len()
    );
    assert!(
        export.notes.iter().any(|n| n.contains("no_such_module")),
        "{:?}",
        export.notes
    );
}
