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

fn meridian_with(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", plugin_dirs())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .args(args)
        .current_dir(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn refused(output: &Output) -> String {
    assert!(!output.status.success(), "it succeeded:\n{}", text(output));
    text(output)
}

const MANIFEST: &str = "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: estate\nspec:\n  retarget:\n    out: \"moved/{name}.{target}.{ext}\"\n";

#[test]
fn the_project_names_where_a_moved_file_goes_unless_the_command_line_does() {
    let dir = scratch("manifest");
    ok(&meridian(&dir, "import", &["apply", "site.yaml"]));
    fs::write(dir.join("meridian.yaml"), MANIFEST).unwrap();

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
    ok(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["outFrom"], "manifest");
    assert!(dir.join("moved/site.fake-next.yaml").is_file());
    assert!(!dir.join("site.fake-next.yaml").exists());

    let output = meridian(
        &dir,
        "import",
        &[
            "retarget",
            "site.yaml",
            "--to",
            "fake-next",
            "--out",
            "here.yaml",
            "--output",
            "json",
        ],
    );
    ok(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["outFrom"], "cli");
    assert!(dir.join("here.yaml").is_file());

    // Without a manifest the default is used, and says so.
    fs::remove_file(dir.join("meridian.yaml")).unwrap();
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
    ok(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["outFrom"], "default");
    assert!(dir.join("site.fake-next.yaml").is_file());
}

#[test]
fn a_pattern_the_project_gets_wrong_is_refused_before_anything_is_read() {
    let dir = scratch("bad-manifest");
    fs::write(
        dir.join("meridian.yaml"),
        MANIFEST.replace("{name}.{target}.{ext}", "{name}.yaml"),
    )
    .unwrap();
    let said = refused(&meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake-next", "--no-adopt"],
    ));
    assert!(said.contains("spec.retarget.out"), "{said}");
    assert!(said.contains("must hold {target}"), "{said}");
}

#[test]
fn only_what_the_old_tool_applied_is_adopted_and_the_rest_is_created() {
    let dir = scratch("partial");
    // The old tool has applied the motd only.
    let first = SITE.split("    - id: banner").next().unwrap().to_owned();
    fs::write(dir.join("first.yaml"), &first).unwrap();
    ok(&meridian(&dir, "import", &["apply", "first.yaml"]));

    let said = ok(&meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake-next"],
    ));
    assert!(said.contains("1 resource adopted"), "{said}");
    let moved = fs::read_to_string(dir.join("site.fake-next.yaml")).unwrap();
    assert!(
        moved.contains("    - id: motd\n      adopt: \"fake-motd\"\n"),
        "{moved}"
    );
    assert!(
        moved.contains("    - id: banner\n      type: file\n"),
        "{moved}"
    );
    let plan = ok(&meridian(&dir, "import", &["plan", "site.fake-next.yaml"]));
    assert!(plan.contains("= adopt files/motd"), "{plan}");
    assert!(plan.contains("+ create files/banner"), "{plan}");
}

#[test]
fn each_document_of_a_file_adopts_what_its_own_tool_manages() {
    let dir = scratch("documents");
    let db = SITE
        .replace("name: web", "name: db")
        .replace("web01", "db01")
        .replace("id: banner", "id: schema");
    let both = format!("{SITE}---\n{db}");
    fs::write(dir.join("site.yaml"), &both).unwrap();
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
    assert!(
        said.contains("db: fake -> fake-next, 2 resources adopted"),
        "{said}"
    );
    let moved = fs::read_to_string(dir.join("site.fake-next.yaml")).unwrap();
    assert!(moved.contains("adopt: \"fake-banner\""), "{moved}");
    assert!(moved.contains("adopt: \"fake-schema\""), "{moved}");
    let plan = ok(&meridian(&dir, "import", &["plan", "site.fake-next.yaml"]));
    assert_eq!(plan.matches("= adopt").count(), 4, "{plan}");
    assert!(!plan.contains("create"), "{plan}");
}

#[test]
fn an_old_tool_that_cannot_report_what_it_manages_is_refused() {
    let dir = scratch("cannot-report");
    ok(&meridian(&dir, "import", &["apply", "site.yaml"]));
    let said = refused(&meridian_with(
        &dir,
        &[("FAKE_ADOPT", "import"), ("FAKE_IDENTIFIERS", "off")],
        &["retarget", "site.yaml", "--to", "fake-next"],
    ));
    assert!(said.contains("cannot report what it manages"), "{said}");
    assert!(said.contains("--no-adopt"), "{said}");
    assert!(!dir.join("site.fake-next.yaml").exists());
}

#[test]
fn a_moved_document_moves_back_without_anything_to_do() {
    let dir = scratch("back");
    ok(&meridian(&dir, "import", &["apply", "site.yaml"]));
    ok(&meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake-next"],
    ));
    ok(&meridian(&dir, "import", &["apply", "site.fake-next.yaml"]));
    ok(&meridian(
        &dir,
        "import",
        &[
            "retarget",
            "site.fake-next.yaml",
            "--to",
            "fake",
            "--out",
            "back.yaml",
        ],
    ));
    let back = fs::read_to_string(dir.join("back.yaml")).unwrap();
    // Each resource keeps the one adopt line it had.
    assert_eq!(back.matches("adopt:").count(), 2, "{back}");
    let plan = ok(&meridian(&dir, "import", &["plan", "back.yaml"]));
    assert!(!plan.contains("create"), "{plan}");
    assert!(!plan.contains("adopt files"), "{plan}");
}

#[test]
fn a_target_that_cannot_adopt_is_moved_with_a_warning() {
    let dir = scratch("none");
    let said = ok(&meridian(
        &dir,
        "none",
        &["retarget", "site.yaml", "--to", "fake-next"],
    ));
    assert!(said.contains("cannot adopt what exists"), "{said}");
    let moved = fs::read_to_string(dir.join("site.fake-next.yaml")).unwrap();
    assert!(!moved.contains("adopt"), "{moved}");
}

#[test]
fn what_cannot_be_moved_is_refused_and_writes_nothing() {
    let dir = scratch("refusals");
    let said = refused(&meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "nowhere"],
    ));
    assert!(
        said.contains("no plugin is installed for the target \"nowhere\""),
        "{said}"
    );
    let said = refused(&meridian(
        &dir,
        "import",
        &["retarget", "site.yaml", "--to", "fake", "--no-adopt"],
    ));
    assert!(said.contains("already on fake"), "{said}");
    let said = refused(&meridian(
        &dir,
        "import",
        &["retarget", "missing.yaml", "--to", "fake-next"],
    ));
    assert!(said.contains("missing.yaml"), "{said}");

    // A JSON document is refused: its layout could not be kept.
    let json = r#"{"apiVersion": "meridian/v1", "kind": "ResourceSet", "metadata": {"name": "web"},
  "spec": {"target": "fake", "targetHost": {"from": "inline", "host": "h"},
           "resources": [{"id": "motd", "type": "file", "params": {"content": "x"}}]}}"#;
    fs::write(dir.join("site.json"), json).unwrap();
    let said = refused(&meridian(
        &dir,
        "import",
        &["retarget", "site.json", "--to", "fake-next", "--no-adopt"],
    ));
    assert!(said.contains("rewrites YAML only"), "{said}");

    // A document that does not validate is never moved.
    fs::write(
        dir.join("bad.yaml"),
        SITE.replace("type: file", "tpye: file"),
    )
    .unwrap();
    refused(&meridian(
        &dir,
        "import",
        &["retarget", "bad.yaml", "--to", "fake-next", "--no-adopt"],
    ));
    let written: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("fake-next"))
        .collect();
    assert!(written.is_empty(), "{written:?}");
}
