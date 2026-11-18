//! A resource's `adopt` through the real command: the first plan reports
//! an adoption and an apply records it, a target that cannot adopt is
//! refused before its plugin is asked, and one that keeps no state of its
//! own compiles with a warning. Run against the fake target, which adopts
//! as a stateful tool does, or as the other two kinds under `FAKE_ADOPT`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SITE: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: motd
      type: file
      adopt: "${{ data.motd_id }}"
      params:
        content: "welcome\n"
    - id: banner
      type: file
      params:
        content: "hello\n"
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("adopt")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian(dir: &Path, adopt: &str, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env("FAKE_ADOPT", adopt)
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .args(args)
        .current_dir(dir);
    if args[0] != "validate" {
        command.arg("--set").arg("motd_id=motd-0042");
    }
    command.output().expect("meridian runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\n{}",
        output.status.code(),
        text(output)
    );
    text(output)
}

#[test]
fn the_first_plan_adopts_and_after_an_apply_nothing_is_left_to_do() {
    let dir = scratch("import");
    let plan = ok(&meridian(&dir, "import", &["plan", "site.yaml"]));
    assert!(plan.contains("= adopt files/motd"), "{plan}");
    assert!(plan.contains("+ create files/banner"), "{plan}");

    // The identifier is placed from data, and the tool is handed it.
    ok(&meridian(
        &dir,
        "import",
        &["compile", "site.yaml", "--out", "build"],
    ));
    assert_eq!(
        fs::read_to_string(dir.join("build/web/adopt/motd")).unwrap(),
        "motd-0042"
    );

    ok(&meridian(&dir, "import", &["apply", "site.yaml"]));
    let again = ok(&meridian(&dir, "import", &["plan", "site.yaml"]));
    assert!(!again.contains("adopt files/motd"), "{again}");
    assert!(!again.contains("create"), "{again}");
}

#[test]
fn an_adoption_is_its_own_action_in_json() {
    let dir = scratch("json");
    let output = meridian(&dir, "import", &["plan", "site.yaml", "--output", "json"]);
    let out = ok(&output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let changes = report["stages"][0]["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        changes
            .iter()
            .any(|c| c["action"] == "adopt" && c["resource"] == "files/motd"),
        "{out}"
    );
}

#[test]
fn a_target_that_cannot_adopt_is_refused_before_its_plugin_is_asked() {
    let dir = scratch("none");
    let output = meridian(&dir, "none", &["compile", "site.yaml", "--out", "build"]);
    assert!(!output.status.success(), "{}", text(&output));
    let said = text(&output);
    assert!(
        said.contains(
            "resource \"motd\" adopts \"motd-0042\", and target \"fake\" cannot take a thing"
        ),
        "{said}"
    );
    assert!(!dir.join("build/web").exists());
}

#[test]
fn a_target_that_keeps_no_state_compiles_and_warns_that_adopt_changes_nothing() {
    let dir = scratch("converge");
    let said = ok(&meridian(
        &dir,
        "converge",
        &["compile", "site.yaml", "--out", "build"],
    ));
    assert!(
        said.contains("adopt \"motd-0042\" changes nothing"),
        "{said}"
    );
    assert!(!dir.join("build/web/adopt").exists());
}

#[test]
fn adopting_a_resource_and_asking_for_it_to_be_absent_is_refused() {
    let dir = scratch("absent");
    fs::write(
        dir.join("site.yaml"),
        SITE.replace(
            "      adopt: \"${{ data.motd_id }}\"\n",
            "      adopt: x\n      state: absent\n",
        ),
    )
    .unwrap();
    let output = meridian(&dir, "import", &["validate", "site.yaml"]);
    assert!(!output.status.success());
    assert!(
        text(&output).contains("adopts a thing and asks for it to be absent"),
        "{}",
        text(&output)
    );
}
