//! The vertical slice: one workflow through three real tools, driven by the
//! real `meridian` binary with the real plugin binaries.
//!
//! `examples/workflow/vertical-slice.yaml` is applied with `kubectl` against
//! the cluster the current kubeconfig context names, `terraform` with the
//! `hashicorp/local` provider, and `ansible-playbook` over the local
//! connection. Then it is planned again, and nothing may want changing. The
//! `vertical slice` workflow starts a kind cluster and installs the other two
//! tools before running this.
//!
//! It runs when `MERIDIAN_TEST_REAL_SLICE=1`:
//!
//! ```sh
//! kind create cluster --wait 180s
//! MERIDIAN_TEST_REAL_SLICE=1 cargo test -p meridian-cli --test vertical_slice -- --ignored --nocapture
//! ```
//!
//! Without the variable it passes without doing anything. With it, a tool
//! that does not run or a cluster that cannot be reached is a failure.
//! Everything on the cluster is created in the namespace `meridian-slice`,
//! which is deleted before and after; everything else goes under a scratch
//! directory.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use meridian_plugin_testkit::{plugin_binary, real_tool_asked, repo_root, scratch};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");
const NAMESPACE: &str = "meridian-slice";

/// Whether to run. Asked for and a tool missing is a failure, not a skip.
fn asked() -> bool {
    if real_tool_asked("SLICE").is_none() {
        return false;
    }
    for (program, args) in [
        ("kubectl", &["version"][..]),
        ("terraform", &["version"][..]),
        ("ansible-playbook", &["--version"][..]),
    ] {
        let runs = Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            runs,
            "MERIDIAN_TEST_REAL_SLICE=1, and {program} does not run; the slice needs kubectl \
             with a cluster, terraform and ansible-playbook"
        );
    }
    true
}

/// One `kubectl` call of the test's own.
fn kubectl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("kubectl")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)))
    }
}

fn delete_namespace() {
    let _ = kubectl(&[
        "delete",
        "namespace",
        NAMESPACE,
        "--ignore-not-found",
        "--wait=true",
        "--timeout=120s",
    ]);
}

/// Deletes the namespace when the test ends, however it ends.
struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        delete_namespace();
    }
}

/// The three plugins, built from this workspace, as one `MERIDIAN_PLUGIN_DIR`.
fn plugin_dirs() -> String {
    ["kubernetes", "terraform", "ansible"]
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

/// `meridian <args>` in `dir`, with the plugins, the example's data and the
/// scratch root, and Ansible told to reach localhost without SSH. The data
/// goes only to the commands that take documents; `validate`, `status` and
/// `report` do not.
fn meridian(dir: &Path, root: &Path, args: &[&str]) -> Output {
    let data = repo_root().join("examples/workflow/vertical-slice.data.yaml");
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command.args(args);
    if matches!(args[0], "compile" | "plan" | "apply" | "drift") {
        command
            .arg("--data")
            .arg(&data)
            .arg("--set")
            .arg(format!("root={}", root.display()));
    }
    command
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", plugin_dirs())
        .env("MERIDIAN_ANSIBLE_ARGS", "--connection local")
        .env("ANSIBLE_LOCALHOST_WARNING", "false")
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND");
    command.output().expect("meridian runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Runs one command and returns what it printed, failing with everything it
/// said when it did not succeed.
fn ok(dir: &Path, root: &Path, args: &[&str]) -> String {
    let out = meridian(dir, root, args);
    assert!(
        out.status.success(),
        "meridian {} failed ({:?})\n--- stdout\n{}--- stderr\n{}",
        args.join(" "),
        out.status.code(),
        text(&out.stdout),
        text(&out.stderr)
    );
    eprintln!("--- meridian {}\n{}", args.join(" "), text(&out.stdout));
    text(&out.stdout)
}

/// The id of the newest run under the state directory.
fn last_run(dir: &Path) -> String {
    let mut ids: Vec<String> = fs::read_dir(dir.join(".meridian/runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    ids.sort();
    ids.pop().expect("a run was recorded")
}

#[test]
#[ignore = "runs real tools; set MERIDIAN_TEST_REAL_SLICE=1 and run with --ignored"]
fn the_slice_is_applied_through_three_real_tools_and_then_in_sync() {
    if !asked() {
        return;
    }
    delete_namespace();
    let _cleanup = Cleanup;

    let dir = scratch(TMP, "vertical-slice");
    let root: PathBuf = dir.join("root");
    fs::create_dir_all(&root).unwrap();
    let workflow = repo_root().join("examples/workflow/vertical-slice.yaml");
    let file = workflow.to_str().unwrap();

    // The language alone, then the plugins without the tools.
    assert_eq!(ok(&dir, &root, &["validate", file]), "3 documents valid\n");
    let compiled = ok(&dir, &root, &["compile", file]);
    assert!(
        compiled.contains("app (Infrastructure, target kubernetes)\n")
            && compiled.contains("edge (Infrastructure, target terraform)\n")
            && compiled.contains(
                "site (ResourceSet, target ansible)\n  deferred: needs the output \"address\" of \
                 Infrastructure \"edge\""
            ),
        "{compiled}"
    );

    // Before anything is applied: the application can be previewed, the
    // edge waits for the application's output, and the site waits for the
    // edge's.
    let plan = ok(&dir, &root, &["plan", file]);
    assert!(
        plan.contains("app (Infrastructure, target kubernetes)\n"),
        "{plan}"
    );
    assert!(
        plan.contains(
            "edge (Infrastructure, target terraform)\n  deferred: needs the output \"workload\" \
             of Infrastructure \"app\", which has not been applied yet\n"
        ),
        "{plan}"
    );
    assert!(
        plan.contains(
            "site (ResourceSet, target ansible)\n  deferred: needs the output \"address\" of \
             Infrastructure \"edge\", which has not been applied yet\n"
        ),
        "{plan}"
    );
    assert!(
        !root.join("edge/upstream.conf").exists() && !root.join("site/index.html").exists(),
        "a plan wrote a file"
    );

    // The apply runs the three stages in order, and each hands the next what
    // it asked for: the workload's name reaches Terraform as a variable, and
    // the machine's address reaches Ansible as the host.
    let applied = ok(&dir, &root, &["apply", file]);
    for line in [
        "app (Infrastructure, target kubernetes)\n",
        "  output workload = \"meridian-slice/web\"\n",
        "  output greeting = \"hello from the slice\"\n",
        "edge (Infrastructure, target terraform)\n",
        "  output address = \"localhost\"\n",
        "site (ResourceSet, target ansible on localhost)\n",
    ] {
        assert!(applied.contains(line), "missing {line:?} in:\n{applied}");
    }
    assert_eq!(
        applied.matches("\n  applied ").count(),
        3,
        "three stages applied:\n{applied}"
    );
    let order = |name: &str| applied.find(name).expect(name);
    assert!(
        order("app (Infrastructure") < order("edge (Infrastructure")
            && order("edge (Infrastructure") < order("site (ResourceSet"),
        "{applied}"
    );

    // What each tool left behind.
    let deployment = kubectl(&["get", "deployment", "web", "--namespace", NAMESPACE]);
    assert!(deployment.is_ok(), "{deployment:?}");
    let greeting = kubectl(&[
        "get",
        "configmap",
        "web-config",
        "--namespace",
        NAMESPACE,
        "-o",
        "jsonpath={.data.greeting}",
    ]);
    assert_eq!(greeting.as_deref(), Ok("hello from the slice"));
    assert_eq!(
        fs::read_to_string(root.join("edge/upstream.conf")).unwrap(),
        "upstream meridian-slice/web\n",
        "Terraform wrote the workload's name it imported"
    );
    assert_eq!(
        fs::read_to_string(root.join("site/index.html")).unwrap(),
        "<h1>Meridian vertical slice</h1>\n"
    );
    assert!(
        root.join("site/.warmed").exists(),
        "the notified exec ran once the index was written"
    );

    // The record says the run succeeded, with its three steps.
    let status = ok(&dir, &root, &["status"]);
    let run = last_run(&dir);
    assert!(
        status.contains(&run) && status.contains("succeeded"),
        "{status}"
    );
    let report = ok(
        &dir,
        &root,
        &["report", "--format", "markdown", "--out", "report.md"],
    );
    let _ = report;
    let written = fs::read_to_string(dir.join("report.md")).unwrap();
    for name in ["app", "edge", "site"] {
        assert!(
            written.contains(name),
            "{name} missing from the report:\n{written}"
        );
    }

    // Planned again, every tool sees the world as the documents say.
    let again = ok(&dir, &root, &["plan", file]);
    assert_eq!(
        again.matches("  in sync, nothing to change\n").count(),
        3,
        "{again}"
    );
    assert!(!again.contains("deferred"), "{again}");

    // A drift check agrees.
    let drift = ok(&dir, &root, &["drift", file]);
    assert_eq!(
        drift.matches("  in sync with what was applied\n").count(),
        3,
        "{drift}"
    );
}
