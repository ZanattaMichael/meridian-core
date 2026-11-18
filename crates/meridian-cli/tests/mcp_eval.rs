//! The scorer of the MCP model test set (`tools/mcp_eval/check.py`), run
//! over an example as the answer, so that the scorer cannot rot while the
//! model runs it serves happen elsewhere.

use std::path::Path;
use std::process::Command;

use meridian_plugin_testkit::repo_root;

#[test]
fn the_scorer_compiles_an_example_through_the_server() {
    let root = repo_root();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp_eval");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The Ansible example answers the Ansible task; the Puppet example is
    // the wrong answer to the Chef task, and must be told so.
    std::fs::copy(
        root.join("examples/ansible/web-server.yaml"),
        dir.join("ansible-nginx.yaml"),
    )
    .unwrap();
    std::fs::write(
        dir.join("chef-exec-guarded.yaml"),
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: x\nspec:\n  target: chef\n  targetHost:\n    from: inline\n    host: app01\n  resources:\n    - id: run\n      type: exec\n      state: bogus\n      params:\n        command: /bin/true\n",
    )
    .unwrap();
    // A Windows runner may call it `python`.
    let python = ["python3", "python"]
        .into_iter()
        .find(|p| {
            Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .expect("a python interpreter is on the PATH");
    let out = Command::new(python)
        .arg(root.join("tools/mcp_eval/check.py"))
        .arg("--answers")
        .arg(&dir)
        .arg("--meridian")
        .arg(env!("CARGO_BIN_EXE_meridian"))
        .arg("--json")
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .output()
        .expect("python3 runs the scorer");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let scores: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ansible = scores
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "ansible-nginx")
        .unwrap();
    // The fake plugin directory holds no Ansible plugin, so the example is
    // checked against the language and the target is found in the text.
    assert_eq!(ansible["answered"], true);
    assert_eq!(ansible["expectedTypes"], true);
    assert_eq!(ansible["expectedText"], true);
    assert_eq!(ansible["target"], true);
    let chef = scores
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "chef-exec-guarded")
        .unwrap();
    assert_eq!(chef["expectedText"], false);
    let unanswered = scores
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "bicep-storage-account")
        .unwrap();
    assert_eq!(unanswered["answered"], false);
}
