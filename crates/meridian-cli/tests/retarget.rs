//! `meridian retarget` through the real command: a document applied with one
//! tool moves to another with an `adopt:` line for each resource the first
//! manages, so the new tool's first plan adopts rather than creates. Run
//! against two copies of the fake target, `fake` and `fake-next`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const SITE: &str = r#"# The web host's files.
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake # moving soon
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
    - id: banner
      type: file
      params:
        content: "hello\n"
"#;

/// The fake plugin, and a copy of it installed as a second target.
fn plugin_dirs() -> String {
    static DIRS: OnceLock<String> = OnceLock::new();
    DIRS.get_or_init(|| {
        let fake = meridian_target_fake::plugin_dir();
        let next = Path::new(env!("CARGO_TARGET_TMPDIR")).join("retarget-plugins");
        let _ = fs::remove_dir_all(&next);
        fs::create_dir_all(&next).unwrap();
        fs::copy(
            fake.join("meridian-target-fake"),
            next.join("meridian-target-fake-next"),
        )
        .unwrap();
        format!("{}:{}", fake.display(), next.display())
    })
    .clone()
}

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("retarget")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian(dir: &Path, adopt: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", plugin_dirs())
        .env("FAKE_ADOPT", adopt)
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("meridian runs")
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
fn what_the_old_tool_manages_is_adopted_by_the_new_one() {
    let dir = scratch("import");
    ok(&meridian(&dir, "import", &["apply", "site.yaml"]));

    let said = ok(&meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake-next"],
    ));
    assert!(
        said.contains("web: fake -> fake-next, 2 resources adopted"),
        "{said}"
    );
    // The file read is left alone; the new one keeps its comments.
    assert_eq!(fs::read_to_string(dir.join("site.yaml")).unwrap(), SITE);
    let moved = fs::read_to_string(dir.join("site.fake-next.yaml")).unwrap();
    assert!(moved.starts_with("# The web host's files.\n"), "{moved}");
    assert!(
        moved.contains("  target: fake-next # moving soon\n"),
        "{moved}"
    );
    assert!(
        moved.contains("    - id: motd\n      adopt: \"fake-motd\"\n"),
        "{moved}"
    );
    assert!(
        moved.contains("    - id: banner\n      adopt: \"fake-banner\"\n"),
        "{moved}"
    );

    // The new tool's first plan adopts both; after its apply nothing is
    // left to do.
    let plan = ok(&meridian(&dir, "import", &["plan", "site.fake-next.yaml"]));
    assert!(plan.contains("= adopt files/motd"), "{plan}");
    assert!(plan.contains("= adopt files/banner"), "{plan}");
    assert!(!plan.contains("create"), "{plan}");
    ok(&meridian(&dir, "import", &["apply", "site.fake-next.yaml"]));
    let again = ok(&meridian(&dir, "import", &["plan", "site.fake-next.yaml"]));
    assert!(!again.contains("adopt"), "{again}");

    // An existing file is never overwritten.
    let output = meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake-next"],
    );
    assert!(!output.status.success());
    assert!(
        text(&output).contains("already exists"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_document_never_applied_is_refused_unless_adopt_is_left_out() {
    let dir = scratch("unapplied");
    let output = meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake-next"],
    );
    assert!(!output.status.success());
    let said = text(&output);
    assert!(said.contains("has not been applied with fake"), "{said}");
    assert!(said.contains("--no-adopt"), "{said}");
    assert!(!dir.join("site.fake-next.yaml").exists());

    let said = ok(&meridian(
        &dir,
        "import",
        &[
            "retarget",
            "site.yaml",
            "--to",
            "fake-next",
            "--no-adopt",
            "--out",
            "moved.yaml",
        ],
    ));
    assert!(said.contains("will create every resource again"), "{said}");
    let moved = fs::read_to_string(dir.join("moved.yaml")).unwrap();
    assert_eq!(moved, SITE.replace("target: fake #", "target: fake-next #"));
}

#[test]
fn a_tool_that_converges_needs_no_adopt_lines() {
    let dir = scratch("converge");
    let said = ok(&meridian(
        &dir,
        "converge",
        &["retarget", "site.yaml", "--to", "fake-next"],
    ));
    assert!(said.contains("converges on what exists"), "{said}");
    let moved = fs::read_to_string(dir.join("site.fake-next.yaml")).unwrap();
    assert!(!moved.contains("adopt"), "{moved}");
}

#[test]
fn the_result_is_one_json_object_the_schema_accepts() {
    let dir = scratch("json");
    ok(&meridian(&dir, "import", &["apply", "site.yaml"]));
    let output = meridian(
        &dir,
        "import",
        &[
            "retarget",
            "site.yaml",
            "--to",
            "fake-next",
            "--output",
            "json",
        ],
    );
    let said = ok(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("not one JSON object ({e}): {said}"));
    let schema_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/output/retarget.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(schema_path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(problems.is_empty(), "{problems:#?}\n{said}");
    assert_eq!(value["command"], "retarget");
    assert_eq!(value["adoption"], "import");
    assert_eq!(value["documents"][0]["adopted"][0], "motd");
}
