//! `meridian mcp`, run as a process against a real plugin binary, the way a
//! client starts it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const DOC: &str = "\
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: net
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("site.yaml"), DOC).unwrap();
    dir
}

/// Starts the server in `dir`, sends `messages` one per line, and returns
/// the replies once the input is closed.
fn talk(dir: &Path, args: &[&str], messages: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .arg("mcp")
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_MCP_ALLOW_PLAN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in messages {
            writeln!(stdin, "{}", serde_json::to_string(message).unwrap()).unwrap();
        }
    }
    let stdout = child.stdout.take().unwrap();
    let replies: Vec<Value> = BufReader::new(stdout)
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "the server exits cleanly when its input ends"
    );
    replies
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: i64, tool: &str, args: Value) -> Value {
    request(id, "tools/call", json!({ "name": tool, "arguments": args }))
}

fn initialize() -> Value {
    request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
    )
}

#[test]
fn the_server_loads_the_plugins_beside_the_binary_and_compiles() {
    let dir = scratch("mcp_compiles");
    let replies = talk(
        &dir,
        &[],
        &[
            initialize(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            call(2, "list_targets", json!({})),
            call(3, "compile", json!({ "file": "site.yaml" })),
            call(4, "plan", json!({ "file": "site.yaml" })),
        ],
    );
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "meridian");
    let targets = &replies[1]["result"]["structuredContent"]["targets"];
    assert_eq!(targets[0]["name"], "fake");
    let compiled = &replies[2]["result"]["structuredContent"];
    assert_eq!(compiled["ok"], true, "{compiled}");
    assert_eq!(compiled["documents"][0]["files"][0]["name"], "files/vm");
    assert_eq!(compiled["documents"][0]["files"][0]["text"], "10.0.0.5");
    // Nothing was written: compile is in memory.
    assert!(!dir.join("out").exists());
    assert!(!dir.join(".meridian").exists());
    // And plan is off.
    let plan = &replies[3]["result"];
    assert_eq!(plan["isError"], true);
    assert!(plan["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--allow-plan"));
}

#[test]
fn plan_runs_the_command_when_allowed() {
    let dir = scratch("mcp_plans");
    let replies = talk(
        &dir,
        &["--allow-plan"],
        &[
            initialize(),
            call(2, "plan", json!({ "file": "site.yaml" })),
            call(3, "plan", json!({ "file": "../somewhere-else.yaml" })),
        ],
    );
    let plan = &replies[1]["result"];
    let body = &plan["structuredContent"];
    assert_eq!(plan["isError"], false, "{body}");
    // The object is the one `meridian plan --output json` prints.
    assert_eq!(body["command"], "plan");
    assert_eq!(body["ok"], true);
    assert_eq!(body["summary"]["stages"], 1);
    assert!(body["report"].as_str().unwrap().contains("net"));
    assert_eq!(body["exitStatus"], 0);
    let outside = &replies[2]["result"];
    assert_eq!(outside["isError"], true);
}
