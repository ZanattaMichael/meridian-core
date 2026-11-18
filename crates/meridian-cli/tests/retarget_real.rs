//! `meridian retarget` against the real tools: a document applied with
//! Terraform moves to OpenTofu, and OpenTofu's first plan adopts what
//! Terraform made instead of creating it again. Needs `terraform` and `tofu`
//! on the `PATH`, and the network for the `hashicorp/random` provider; set
//! `MERIDIAN_TEST_REAL_RETARGET=1`. The opentofu workflow runs it.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use meridian_plugin_testkit::{plugin_binary, real_tool_asked, scratch};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

const SITE: &str = r#"# Two random ids, made by Terraform first.
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: ids
spec:
  target: terraform
  resources:
    - id: random
      type: provider
      params:
        source: hashicorp/random
        version: ">= 3.6.0"
    - id: first
      type: random_id
      params:
        byte_length: 8
    - id: second
      type: random_id
      params:
        byte_length: 8
"#;

fn asked() -> bool {
    if real_tool_asked("RETARGET").is_none() {
        return false;
    }
    for program in ["terraform", "tofu"] {
        let runs = Command::new(program)
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            runs,
            "MERIDIAN_TEST_REAL_RETARGET=1, and {program} does not run; the test needs \
             terraform and tofu"
        );
    }
    true
}

fn plugin_dirs() -> String {
    ["terraform", "opentofu"]
        .iter()
        .map(|target| {
            plugin_binary(&format!("meridian-target-{target}"))
                .parent()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn meridian(dir: &Path, args: &[&str]) -> String {
    let output: Output = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", plugin_dirs())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("meridian runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "meridian {} failed:\n{said}",
        args.join(" ")
    );
    said
}

/// The changes Meridian's plan lists, as `<action> <resource>`, from its own
/// lines (`= adopt first: random_id`), not the tool's output beneath them,
/// which is prefixed with the document's name.
fn actions(plan: &str) -> Vec<String> {
    const ACTIONS: &[&str] = &["create", "update", "replace", "delete", "adopt"];
    plan.lines()
        .filter(|line| line.starts_with("  ") && !line.contains(" | "))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let (_sign, action, resource) = (words.next()?, words.next()?, words.next()?);
            ACTIONS
                .contains(&action)
                .then(|| format!("{action} {}", resource.trim_end_matches(':')))
        })
        .collect()
}

/// The identifier the moved file gives a resource.
fn adopted(moved: &str, id: &str) -> String {
    let after = moved
        .split(&format!("- id: {id}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("{id} is not in\n{moved}"));
    let line = after.lines().next().unwrap().trim();
    line.strip_prefix("adopt: \"")
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{id} has no adopt line:\n{moved}"))
        .to_owned()
}

#[ignore = "needs terraform and tofu; see docs/contributing/real-tool-tests.md"]
#[test]
fn what_terraform_made_is_adopted_by_opentofu_and_nothing_is_made_again() {
    if !asked() {
        return;
    }
    let dir = scratch(TMP, "retarget-real");
    fs::write(dir.join("site.yaml"), SITE).unwrap();

    let applied = meridian(&dir, &["apply", "site.yaml"]);
    assert!(applied.contains("first"), "{applied}");

    let said = meridian(&dir, &["retarget", "site.yaml", "--to", "opentofu"]);
    assert!(
        said.contains("ids: terraform -> opentofu, 2 resources adopted"),
        "{said}"
    );
    let moved = fs::read_to_string(dir.join("site.opentofu.yaml")).unwrap();
    assert!(moved.contains("  target: opentofu\n"), "{moved}");
    // The provider is not a thing Terraform manages, so it adopts nothing.
    assert!(
        moved.contains("    - id: random\n      type: provider\n"),
        "{moved}"
    );
    let first = adopted(&moved, "first");
    let second = adopted(&moved, "second");
    assert_ne!(first, second);

    // OpenTofu's first plan adopts both, and creates and replaces nothing.
    let plan = meridian(&dir, &["plan", "site.opentofu.yaml"]);
    assert!(plan.contains("= adopt first"), "{plan}");
    assert!(plan.contains("= adopt second"), "{plan}");
    assert_eq!(actions(&plan), ["adopt first", "adopt second"], "{plan}");

    // The apply takes them into OpenTofu's state as they are.
    meridian(&dir, &["apply", "site.opentofu.yaml"]);
    let state =
        fs::read_to_string(dir.join(".meridian/infrastructure/ids/opentofu/terraform.tfstate"))
            .expect("opentofu wrote its state");
    assert!(state.contains(&first) && state.contains(&second), "{state}");

    // And after it there is nothing left to do.
    let again = meridian(&dir, &["plan", "site.opentofu.yaml"]);
    assert!(actions(&again).is_empty(), "{again}");
}

#[test]
fn a_plans_own_lines_are_read_and_the_tools_output_is_not() {
    let plan = "ids (Infrastructure, target opentofu)\n  = adopt first: random_id\n  = adopt second: random_id\n  2 changes to make\n  ids | \n  ids | Terraform has created a lock file\n  ids |   + create\n";
    assert_eq!(actions(plan), ["adopt first", "adopt second"]);
    assert!(actions("ids (Infrastructure, target opentofu)\n  no changes\n").is_empty());
}
