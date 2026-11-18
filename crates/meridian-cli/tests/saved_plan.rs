//! A saved plan: `plan --out` writes one and `apply --plan` carries it out,
//! or refuses when the documents, the state or the selection have moved on
//! since. Run as a process against the fake target, which saves a plan of
//! its own and records what it was handed back.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::repo_root;
use serde_json::Value;

const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
  labels:
    tier: infra
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
  outputs:
    - name: address
      value: "${vm.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
  labels:
    tier: app
spec:
  target: fake
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: motd
      type: file
      params:
        content: "MOTD\n"
"#;

/// A document that reads a secret, which the fake target refuses to put in
/// a saved plan, as Terraform's plugin does.
const WITH_SECRET: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: inline
    host: web01.example.com
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  resources:
    - id: conf
      type: file
      params:
        content: "password=${{ secret.db_password }}\n"
"#;

fn scratch(test: &str, motd: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("saved-plan")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(&dir, motd);
    dir
}

fn write(dir: &Path, motd: &str) {
    fs::write(dir.join("site.yaml"), WORKFLOW.replace("MOTD", motd)).unwrap();
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env("SITE_DB_PASSWORD", "zzq-not-a-real-password")
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .args(args)
        .current_dir(dir);
    command
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    command(dir, args).output().expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}\n{}{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    );
}

fn plan_file(dir: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join("plan.mplan")).unwrap()).unwrap()
}

fn stage<'a>(plan: &'a Value, name: &str) -> &'a Value {
    plan["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stage| stage["name"] == name)
        .unwrap_or_else(|| panic!("no stage {name} in {plan}"))
}

/// What the fake target recorded about the plan it was handed, if any.
fn handed(dir: &Path, kind: &str, name: &str) -> Option<Value> {
    let path = dir
        .join(".meridian")
        .join(kind)
        .join(name)
        .join("fake")
        .join("applied-from-plan.json");
    fs::read_to_string(path)
        .ok()
        .map(|text| serde_json::from_str(&text).unwrap())
}

#[test]
fn a_plan_is_saved_and_an_apply_from_it_hands_each_tool_the_plan_it_saved() {
    let dir = scratch("round-trip", "welcome");
    let planned = meridian(&dir, &["plan", "site.yaml", "--out", "plan.mplan"]);
    ok(&planned);
    assert!(stderr(&planned).contains("plan: wrote plan.mplan"));

    let plan = plan_file(&dir);
    assert_eq!(plan["meridianPlan"], 1);
    assert_eq!(plan["files"], serde_json::json!(["site.yaml"]));
    assert_eq!(plan["selector"], "");
    assert_eq!(plan["command"], "plan site.yaml --out plan.mplan");
    let network = stage(&plan, "network");
    assert_eq!(network["status"], "planned");
    assert_eq!(network["appliedBefore"], "");
    assert!(network["saved"].is_string(), "{network}");
    assert_eq!(network["changes"].as_array().unwrap().len(), 2);
    assert!(network["artifactHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let web = stage(&plan, "web");
    assert_eq!(web["status"], "planned");
    assert!(web["saved"].is_string());
    // Nothing of the run's values is in the file beyond what the documents
    // say; a file's content is what the fake target plans.
    assert!(!fs::read_to_string(dir.join("plan.mplan"))
        .unwrap()
        .contains("web01.example.com\":"));

    let applied = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    ok(&applied);
    let text = stdout(&applied);
    assert!(text.contains("applied 2 changes"), "{text}");
    assert!(text.contains("applied 1 change"), "{text}");
    let given = handed(&dir, "infrastructure", "network").expect("the tool was handed the plan");
    assert_eq!(given["changes"].as_array().unwrap().len(), 2);
    assert!(handed(&dir, "resourceset", "web").is_some());

    // The record says which plan the apply carried out.
    let events = meridian(&dir, &["events", "--type", "run.started"]);
    ok(&events);
    assert!(
        stdout(&events).contains(&format!("\"plan\":\"{}\"", plan["run"].as_str().unwrap())),
        "{}",
        stdout(&events)
    );

    // Applied once, the plan is spent: the state has moved on.
    let again = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(
        stderr(&again).contains("was applied since the plan was taken (by run "),
        "{}",
        stderr(&again)
    );
}

#[test]
fn a_document_that_changed_since_the_plan_is_refused_and_nothing_is_applied() {
    let dir = scratch("changed", "welcome");
    ok(&meridian(
        &dir,
        &["plan", "site.yaml", "--out", "plan.mplan"],
    ));
    write(&dir, "something else");

    let applied = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    assert_eq!(applied.status.code(), Some(1));
    let text = stdout(&applied);
    assert!(
        text.contains("the document no longer compiles to what was planned"),
        "{text}"
    );
    // The first stage is checked before anything is applied to it, and the
    // stage after it is not run.
    assert!(text.contains("applied 2 changes"), "{text}");
    assert!(!dir.join(".meridian/resourceset/web/fake/current").exists());

    let status = meridian(&dir, &["status"]);
    assert!(
        stdout(&status)
            .contains("web (ResourceSet, target fake on web01.example.com): failed (plan-differs)"),
        "{}",
        stdout(&status)
    );
}

#[test]
fn what_was_typed_must_be_what_the_plan_was_taken_with() {
    let dir = scratch("typed", "welcome");
    ok(&meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "-l",
            "tier=infra",
            "--out",
            "plan.mplan",
        ],
    ));
    let plan = plan_file(&dir);
    assert_eq!(plan["selector"], "tier=infra");
    assert_eq!(plan["stages"].as_array().unwrap().len(), 1);

    let other = meridian(&dir, &["apply", "other.yaml", "--plan", "plan.mplan"]);
    assert_eq!(other.status.code(), Some(1));
    assert!(
        stderr(&other).contains(
            "the plan was taken with the documents `site.yaml`, and this apply names `other.yaml`"
        ),
        "{}",
        stderr(&other)
    );
    let selection = meridian(&dir, &["apply", "-l", "tier=app", "--plan", "plan.mplan"]);
    assert_eq!(selection.status.code(), Some(1));
    assert!(
        stderr(&selection).contains("the plan was taken with the selector `tier=infra`"),
        "{}",
        stderr(&selection)
    );
    // Typed the same, or left out, it goes ahead with the plan's selection.
    let same = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "-l",
            "tier=infra",
            "--plan",
            "plan.mplan",
        ],
    );
    ok(&same);
    assert!(stdout(&same).contains("applied 2 changes"));
    assert!(!stdout(&same).contains("web ("));

    // Only an apply carries a plan out, and never with --resume.
    let together = meridian(&dir, &["apply", "--plan", "plan.mplan", "--resume"]);
    assert_eq!(together.status.code(), Some(2));
    let nothing = meridian(&dir, &["apply"]);
    assert_eq!(nothing.status.code(), Some(2));
    assert!(stderr(&nothing).contains("<FILES>"), "{}", stderr(&nothing));
}

#[test]
fn a_plan_with_another_selection_than_the_apply_is_refused_before_anything_runs() {
    let dir = scratch("selection", "welcome");
    ok(&meridian(
        &dir,
        &["plan", "site.yaml", "--out", "plan.mplan"],
    ));
    // Edit the plan to drop a stage, as a plan of another selection would
    // have, keeping its own selector so the apply reads it as the same.
    let mut plan = plan_file(&dir);
    plan["stages"]
        .as_array_mut()
        .unwrap()
        .retain(|stage| stage["name"] == "network");
    fs::write(dir.join("plan.mplan"), plan.to_string()).unwrap();

    let applied = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    assert_eq!(applied.status.code(), Some(1));
    assert!(
        stderr(&applied).contains("the plan has no stage for ResourceSet \"web\""),
        "{}",
        stderr(&applied)
    );
    assert!(!dir
        .join(".meridian/infrastructure/network/fake/current")
        .exists());
}

#[test]
fn a_plan_with_a_failed_stage_is_not_written() {
    let dir = scratch("failed", "welcome");
    fs::write(
        dir.join("site.yaml"),
        WORKFLOW.replace("MOTD", "welcome").replace(
            "target: fake\n  targetHost",
            "target: nothing\n  targetHost",
        ),
    )
    .unwrap();
    let planned = meridian(&dir, &["plan", "site.yaml", "--out", "plan.mplan"]);
    assert_eq!(planned.status.code(), Some(1));
    assert!(!dir.join("plan.mplan").exists());
    assert!(
        stderr(&planned).contains("the plan was not saved: ResourceSet \"web\" failed"),
        "{}",
        stderr(&planned)
    );
}

#[test]
fn a_tool_that_saves_no_plan_or_would_save_a_secret_is_planned_again_at_apply() {
    let dir = scratch("no-saved-plan", "welcome");
    let planned = command(&dir, &["plan", "site.yaml", "--out", "plan.mplan"])
        .env("FAKE_SAVES_PLANS", "none")
        .output()
        .unwrap();
    ok(&planned);
    let plan = plan_file(&dir);
    assert!(stage(&plan, "network")["saved"].is_null());
    let applied = command(&dir, &["apply", "--plan", "plan.mplan"])
        .env("FAKE_SAVES_PLANS", "none")
        .output()
        .unwrap();
    ok(&applied);
    assert!(stdout(&applied).contains("applied 2 changes"));
    assert!(handed(&dir, "infrastructure", "network").is_none());

    let dir = scratch("secret", "welcome");
    fs::write(dir.join("site.yaml"), WITH_SECRET).unwrap();
    ok(&meridian(
        &dir,
        &["plan", "site.yaml", "--out", "plan.mplan"],
    ));
    let plan = plan_file(&dir);
    assert!(stage(&plan, "web")["saved"].is_null());
    assert!(!fs::read_to_string(dir.join("plan.mplan"))
        .unwrap()
        .contains("zzq-not-a-real-password"));
    let applied = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    ok(&applied);
    assert!(handed(&dir, "resourceset", "web").is_none());
}

#[test]
fn the_json_output_names_the_plan() {
    let dir = scratch("json", "welcome");
    let planned = meridian(
        &dir,
        &[
            "--output",
            "json",
            "plan",
            "site.yaml",
            "--out",
            "plan.mplan",
        ],
    );
    ok(&planned);
    let value: Value = serde_json::from_str(&stdout(&planned)).unwrap();
    assert_eq!(value["plan"]["file"], "plan.mplan");
    assert_eq!(value["plan"]["run"], Value::Null);
    assert_eq!(value["plan"]["hasChanges"], true);

    let applied = meridian(&dir, &["--output", "json", "apply", "--plan", "plan.mplan"]);
    ok(&applied);
    let value: Value = serde_json::from_str(&stdout(&applied)).unwrap();
    assert_eq!(value["plan"]["file"], "plan.mplan");
    assert_eq!(value["plan"]["run"], plan_file(&dir)["run"]);
    assert_eq!(value["ok"], true);

    let spent = meridian(&dir, &["--output", "json", "apply", "--plan", "plan.mplan"]);
    assert_eq!(spent.status.code(), Some(1));
    let value: Value = serde_json::from_str(&stdout(&spent)).unwrap();
    assert_eq!(value["refused"]["kind"], "plan");
    assert_eq!(value["ok"], false);

    let planned = meridian(&dir, &["--output", "json", "plan", "site.yaml"]);
    ok(&planned);
    let value: Value = serde_json::from_str(&stdout(&planned)).unwrap();
    assert_eq!(value["plan"], Value::Null);
}

/// The page's GitHub Actions snippet uses the two actions of this repository,
/// and the smoke-test workflow runs those very actions: each `uses:` of the
/// snippet names an action that exists here and that `saved-plan.yml` runs,
/// and the actions run the two commands the page is about.
#[test]
fn the_github_actions_snippet_on_the_page_uses_the_actions_the_workflow_runs() {
    let root = repo_root();
    let page = fs::read_to_string(root.join("docs/operations/pull-requests.md")).unwrap();
    let workflow = fs::read_to_string(root.join(".github/workflows/saved-plan.yml")).unwrap();
    let snippet = page
        .split("<!-- github-actions-snippet -->")
        .nth(1)
        .expect("the page marks its GitHub Actions snippet")
        .split("```")
        .nth(1)
        .expect("the snippet is a fenced block");
    let actions: Vec<&str> = snippet
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("- ").unwrap_or(line))
        .filter_map(|line| line.strip_prefix("uses: ZanattaMichael/meridian-core/"))
        .map(|uses| uses.split('@').next().unwrap())
        .collect();
    assert_eq!(actions, [".github/actions/plan", ".github/actions/apply"]);
    for action in actions {
        let file = root.join(action).join("action.yml");
        assert!(
            file.is_file(),
            "{} is not an action of this repository",
            file.display()
        );
        assert!(
            workflow
                .lines()
                .map(str::trim)
                .any(|line| line == format!("uses: ./{action}")),
            "saved-plan.yml does not run {action}"
        );
    }
    let plan = fs::read_to_string(root.join(".github/actions/plan/action.yml")).unwrap();
    let apply = fs::read_to_string(root.join(".github/actions/apply/action.yml")).unwrap();
    assert!(
        plan.contains("meridian --output json plan ") && plan.contains("--out \""),
        "{plan}"
    );
    assert!(
        apply.contains("meridian --output json apply --plan "),
        "{apply}"
    );
}

/// A data file is never a reason to refuse on its own, since a key no
/// stage reads can change and a real target compiles to the same artifact.
/// (The fake target echoes all of its data into its artifact, so that half
/// cannot be shown here.) When a stage does compile to something else and a
/// data file has changed, the failure names the file.
#[test]
fn a_changed_data_file_is_named_when_a_stage_differs() {
    let dir = scratch("data", "${{ data.motd }}");
    fs::write(dir.join("prod.yaml"), "motd: welcome\n").unwrap();
    ok(&meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--data",
            "prod.yaml",
            "--out",
            "plan.mplan",
        ],
    ));
    let plan = plan_file(&dir);
    let hashes = plan["dataHashes"]
        .as_array()
        .expect("the plan records the data's hashes");
    assert_eq!(hashes.len(), 1, "{plan}");
    assert!(hashes[0].as_str().unwrap().starts_with("sha256:"), "{plan}");

    fs::write(dir.join("prod.yaml"), "motd: goodbye\n").unwrap();
    let applied = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    assert_eq!(applied.status.code(), Some(1));
    let text = stdout(&applied);
    assert!(
        text.contains("and the data file prod.yaml has changed since the plan; take a new plan"),
        "{text}"
    );
}

/// A document that changed while the data did not names no data file.
#[test]
fn an_unchanged_data_file_is_not_named() {
    let dir = scratch("data-same", "${{ data.motd }} at first");
    fs::write(dir.join("prod.yaml"), "motd: welcome\n").unwrap();
    ok(&meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--data",
            "prod.yaml",
            "--out",
            "plan.mplan",
        ],
    ));
    write(&dir, "${{ data.motd }} later");
    let applied = meridian(&dir, &["apply", "--plan", "plan.mplan"]);
    assert_eq!(applied.status.code(), Some(1));
    let text = stdout(&applied);
    assert!(
        text.contains("plan-differs") || text.contains("no longer compiles"),
        "{text}"
    );
    assert!(!text.contains("data file"), "{text}");
}
