//! `--tool-debug`: each tool is asked for more of its own output, the lines
//! are masked as every tool line is, and the run records the level.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The text of a secret the second stage fetches.
const PASSWORD: &str = "kq7Zp2-not-a-real-password-9fXw";

const SITE: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
    - id: shared
      type: file
      params:
        content: kq7Zp2-not-a-real-password-9fXw
  outputs:
    - name: address
      value: "${vm.content}"
    - name: shared
      value: "${shared.content}"
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
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  resources:
    - id: motd
      type: file
      when: secret.db_password != ''
      params:
        content: "welcome\n"
    - id: banner
      type: file
      params:
        content: "MOTD_BANNER"
    - id: dbpass
      type: file
      params:
        content: "${{ secret.db_password }}"
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("tool-debug-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("SITE_DB_PASSWORD", PASSWORD)
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_OUTPUT")
        .env_remove("MERIDIAN_TOOL_DEBUG")
        .output()
        .expect("meridian runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Meridian's own record and logs, as text. The tools' working directories
/// hold what was applied, which includes the secret by design.
fn kept(dir: &Path) -> String {
    let mut all = String::new();
    let mut pending = vec![dir.join(".meridian/runs")];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                all.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    all
}

#[test]
fn a_debug_level_reaches_the_tool_masked_and_is_recorded() {
    let dir = scratch("level");
    let out = meridian(&dir, &["apply", "site.yaml", "--tool-debug=2"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let log = stderr(&out);
    assert!(
        log.contains("network ! fake-tool debug: FAKE_DEBUG=2"),
        "{log}"
    );
    assert!(log.contains("web ! fake-tool debug: FAKE_DEBUG=2"), "{log}");
    // The tool printed the secret it was handed; nobody sees it.
    assert!(
        log.contains("web ! fake-tool debug: secret db_password is (sensitive)"),
        "{log}"
    );
    assert!(!log.contains(PASSWORD), "{log}");
    let kept = kept(&dir);
    assert!(kept.contains("fake-tool debug: secret db_password is (sensitive)"));
    assert!(!kept.contains(PASSWORD));
    // The record says the run asked for it.
    let events =
        String::from_utf8_lossy(&meridian(&dir, &["events", "--type", "run.started"]).stdout)
            .into_owned();
    assert!(events.contains("\"toolDebug\":2"), "{events}");
}

#[test]
fn level_0_asks_nothing_and_a_bare_flag_is_level_1() {
    let dir = scratch("default");
    let out = meridian(&dir, &["plan", "site.yaml"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("fake-tool debug"),
        "{}",
        stderr(&out)
    );

    let out = meridian(&dir, &["plan", "site.yaml", "--tool-debug"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("network ! fake-tool debug: FAKE_DEBUG=1"));

    let out = meridian(&dir, &["plan", "site.yaml", "--tool-debug=4"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn a_debug_step_limits_the_level_to_that_document() {
    let dir = scratch("step");
    let out = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--tool-debug=3",
            "--tool-debug-step",
            "web",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let log = stderr(&out);
    assert!(log.contains("web ! fake-tool debug: FAKE_DEBUG=3"), "{log}");
    assert!(!log.contains("network ! fake-tool debug"), "{log}");
}
