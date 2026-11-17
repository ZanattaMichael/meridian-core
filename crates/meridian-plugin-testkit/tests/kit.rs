//! The test kit's own tests: what every plugin's tests are built on had
//! none until now.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    artifact, assert_golden, assert_golden_refusal, golden_name, graph_from_text, is_golden_of,
    real_tool_asked, real_tool_program, scratch, stand_in, KeptLog,
};
use meridian_sdk::tool::Tool;
use meridian_sdk::{CompileError, Emitted, Position};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

#[test]
fn a_golden_name_turns_the_artifact_path_into_one_word() {
    assert_eq!(golden_name("web", "playbook.yml"), "web.playbook_yml");
    assert_eq!(
        golden_name("web", "manifests/init.pp"),
        "web.manifests_init_pp"
    );
    assert_eq!(golden_name("web", "Dockerfile"), "web.Dockerfile");
}

#[test]
fn only_the_kits_own_files_count_as_golden_files_of_a_case() {
    for name in [
        "web.playbook_yml",
        "web.warnings",
        "web.refused",
        "web.Dockerfile",
    ] {
        assert!(is_golden_of("web", name), "{name}");
    }
    // A test's input beside its golden files, another case, and a case
    // whose name is a prefix of this one.
    for name in [
        "web.yaml",
        "web.json",
        "web",
        "web2.playbook_yml",
        "website.yaml",
        "notes.md",
    ] {
        assert!(!is_golden_of("web", name), "{name}");
    }
}

fn emitted(files: &[(&str, &str)]) -> Emitted {
    Emitted {
        artifact: artifact(files),
        warnings: Vec::new(),
    }
}

/// Environment variables are process-wide; the tests that set one run under
/// this lock so they do not see each other's settings.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `f` with `UPDATE_GOLDEN` set, and takes it away again.
fn recording(f: impl FnOnce()) {
    let _held = ENV.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("UPDATE_GOLDEN", "1");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::env::remove_var("UPDATE_GOLDEN");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Runs `f` with `UPDATE_GOLDEN` unset, holding the same lock, so a
/// comparison cannot run while another test is recording and be turned
/// into a recording itself.
fn comparing(f: impl FnOnce()) {
    let _held = ENV.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        std::env::var_os("UPDATE_GOLDEN").is_none(),
        "UPDATE_GOLDEN is set outside a recording; the kit's own tests need it unset"
    );
    f();
}

#[test]
fn recording_a_case_again_removes_its_stale_golden_files_and_nothing_else() {
    let dir = scratch(TMP, "golden-update");
    recording(|| {
        assert_golden(
            &dir,
            "web",
            &emitted(&[("playbook.yml", "one\n"), ("inventory.ini", "hosts\n")]),
        );
    });
    assert_eq!(
        fs::read_to_string(dir.join("web.playbook_yml")).unwrap(),
        "one\n"
    );
    assert!(dir.join("web.inventory_ini").is_file());

    // The test's input, and another case's golden file, live beside them.
    fs::write(dir.join("web.yaml"), "kind: ResourceSet\n").unwrap();
    fs::write(dir.join("web2.playbook_yml"), "other\n").unwrap();

    recording(|| {
        assert_golden(&dir, "web", &emitted(&[("playbook.yml", "two\n")]));
    });
    assert_eq!(
        fs::read_to_string(dir.join("web.playbook_yml")).unwrap(),
        "two\n"
    );
    assert!(
        !dir.join("web.inventory_ini").exists(),
        "the stale golden file is removed"
    );
    assert!(
        dir.join("web.yaml").is_file(),
        "the input is not the kit's to delete"
    );
    assert!(
        dir.join("web2.playbook_yml").is_file(),
        "another case is left alone"
    );

    // Comparing reads only the golden files, so the input beside them is
    // not an "extra file".
    comparing(|| assert_golden(&dir, "web", &emitted(&[("playbook.yml", "two\n")])));
}

#[test]
#[should_panic(expected = "the set of golden files differs")]
fn a_missing_golden_file_is_reported_by_name() {
    let dir = scratch(TMP, "golden-missing");
    recording(|| assert_golden(&dir, "web", &emitted(&[("playbook.yml", "one\n")])));
    comparing(|| {
        assert_golden(
            &dir,
            "web",
            &emitted(&[("playbook.yml", "one\n"), ("inventory.ini", "hosts\n")]),
        )
    });
}

#[test]
fn a_refusal_is_recorded_as_one_file() {
    let dir = scratch(TMP, "golden-refusal");
    let refusal = CompileError {
        target: "fake".into(),
        resource: "web".into(),
        rule: "unsupported-param".into(),
        msg: "no".into(),
        pos: Position::default(),
    };
    recording(|| assert_golden_refusal(&dir, "web", &refusal));
    assert_eq!(
        fs::read_to_string(dir.join("web.refused")).unwrap(),
        format!("{refusal}\n")
    );
    comparing(|| assert_golden_refusal(&dir, "web", &refusal));
}

#[test]
fn a_stand_in_sees_the_tools_arguments_and_directory() {
    let dir = scratch(TMP, "stand-in");
    let program = stand_in(
        &dir,
        "tool",
        "echo \"args: $*\"\necho \"dir: $(pwd)\"\nexit 3",
    );
    let finished = Tool::new(program.to_str().unwrap())
        .args(["plan", "--json"])
        .dir(&dir)
        .capture_stdout()
        .run(&KeptLog::default())
        .unwrap();
    assert_eq!(finished.code, Some(3));
    assert!(
        finished.stdout.contains("args: plan --json"),
        "{}",
        finished.stdout
    );
    let shown = Path::new(
        finished
            .stdout
            .lines()
            .nth(1)
            .unwrap()
            .trim_start_matches("dir: "),
    );
    assert_eq!(
        shown.canonicalize().unwrap(),
        dir.canonicalize().unwrap(),
        "{}",
        finished.stdout
    );
}

#[test]
fn stand_ins_written_by_many_threads_at_once_all_run() {
    // The script is written under another name and renamed into place, so
    // a thread never runs a file another thread still has open.
    let dir = scratch(TMP, "stand-in-race");
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let program = stand_in(&dir, "racer", &format!("echo {i}"));
                let finished = Tool::new(program.to_str().unwrap())
                    .capture_stdout()
                    .run(&KeptLog::default())
                    .unwrap();
                assert!(finished.success(), "{finished:?}");
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn a_real_tool_test_is_asked_for_by_one_variable() {
    let _held = ENV.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("MERIDIAN_TEST_REAL_KITTEST");
    std::env::remove_var("MERIDIAN_TEST_REAL_REQUIRED");
    assert_eq!(real_tool_asked("KITTEST"), None);
    assert_eq!(real_tool_program("KITTEST", "kittest"), None);

    std::env::set_var("MERIDIAN_TEST_REAL_KITTEST", "0");
    assert_eq!(real_tool_asked("KITTEST"), None, "0 means no");

    std::env::set_var("MERIDIAN_TEST_REAL_KITTEST", "1");
    assert_eq!(real_tool_asked("KITTEST").as_deref(), Some("1"));
    assert_eq!(
        real_tool_program("KITTEST", "kittest").as_deref(),
        Some("kittest")
    );
    std::env::set_var("MERIDIAN_KITTEST", "/opt/kittest");
    assert_eq!(
        real_tool_program("KITTEST", "kittest").as_deref(),
        Some("/opt/kittest")
    );

    std::env::remove_var("MERIDIAN_TEST_REAL_KITTEST");
    std::env::set_var("MERIDIAN_TEST_REAL_REQUIRED", "1");
    let required = std::panic::catch_unwind(|| real_tool_asked("KITTEST"));
    std::env::remove_var("MERIDIAN_TEST_REAL_REQUIRED");
    std::env::remove_var("MERIDIAN_KITTEST");
    assert!(required.is_err(), "required and missing is a failure");
}

#[test]
fn a_graph_is_built_from_a_document_for_one_target() {
    let graph = graph_from_text(
        "site.yaml",
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\nspec:\n  target: fake\n  targetHost:\n    from: inline\n    host: localhost\n  resources:\n    - id: motd\n      type: file\n      params:\n        path: /etc/motd\n",
        "fake",
    );
    assert_eq!(graph.order(), ["motd"]);
    let map: BTreeMap<_, _> = graph.resources.iter().map(|r| (r.id.as_str(), r)).collect();
    assert_eq!(map["motd"].r#type, "file");
}
