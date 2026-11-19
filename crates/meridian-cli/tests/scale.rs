//! Tier 1 scale test (issue 240): a generated project of thousands of
//! resources, hundreds of stages and a thousand endpoints is compiled,
//! planned, applied and destroyed through the fake target, and each
//! command's wall time and peak memory is held to `tests/scale/budgets.toml`.
//!
//! Ignored by default; run in release, as `.github/workflows/scale.yml` does:
//!
//! ```text
//! cargo test --release -p meridian-cli --test scale -- --ignored --nocapture
//! ```
//!
//! `MERIDIAN_SCALE_REPORT=FILE` also writes the table as Markdown.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How far over its budget a measurement may go.
const SLACK: f64 = 1.2;

/// `[section]` and `key = number` lines, which is all the budgets file is.
fn budgets(path: &Path) -> BTreeMap<String, f64> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut section = String::new();
    let mut out = BTreeMap::new();
    for line in text.lines().map(|l| l.split('#').next().unwrap().trim()) {
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_owned();
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("{}: expected key = number: {line}", path.display()));
        let value: f64 = value
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{}: {line}: {e}", path.display()));
        out.insert(format!("{section}.{}", key.trim()), value);
    }
    out
}

/// Peak resident memory of a process, in kilobytes, from `/proc`.
fn peak_kb(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
}

/// The ids of a process's children, whichever of its threads started them.
fn children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .filter_map(|t| t.ok())
        .flat_map(|t| {
            fs::read_to_string(t.path().join("children"))
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<u32>>()
        })
        .collect()
}

struct Measured {
    seconds: f64,
    meridian_mb: f64,
    plugin_mb: f64,
}

/// Runs `meridian` to the end, sampling its memory and its plugins'.
fn run(dir: &Path, plugins: &Path, args: &[String]) -> Measured {
    // To a file, not a pipe: a pipe nobody reads while the run goes on fills,
    // and the run stops.
    let log = dir.join("meridian.log");
    let started = Instant::now();
    let mut child: Child = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env("MERIDIAN_PLUGIN_SIGNATURES", "off")
        .env("MERIDIAN_PLUGIN_DIR", plugins)
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_RUN_ID")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log).expect("the log is created"))
        .spawn()
        .expect("meridian starts");
    let pid = child.id();
    let (mut own, mut plugin) = (0u64, 0u64);
    let status = loop {
        own = own.max(peak_kb(pid).unwrap_or(0));
        for c in children(pid) {
            plugin = plugin.max(peak_kb(c).unwrap_or(0));
        }
        if let Some(status) = child.try_wait().expect("meridian is waited on") {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let seconds = started.elapsed().as_secs_f64();
    let stderr = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.success(),
        "meridian {} failed: {status}\n{stderr}",
        args.join(" ")
    );
    Measured {
        seconds,
        meridian_mb: own as f64 / 1024.0,
        plugin_mb: plugin as f64 / 1024.0,
    }
}

fn generate(dir: &Path, project: &BTreeMap<String, f64>) -> Vec<String> {
    let _ = fs::remove_dir_all(dir);
    let arg = |key: &str| format!("{}", project[&format!("project.{key}")] as u64);
    let script = meridian_plugin_testkit::repo_root().join("tools/lab/generate.py");
    let out = Command::new("python3")
        .arg(&script)
        .args(["--resources", &arg("resources")])
        .args(["--endpoints", &arg("endpoints")])
        .args(["--stages", &arg("stages")])
        .args(["--seed", &arg("seed")])
        .arg(dir)
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut files: Vec<String> = ["stages", "hosts"]
        .iter()
        .flat_map(|sub| {
            let mut names: Vec<PathBuf> = fs::read_dir(dir.join(sub))
                .map(|e| e.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            names.sort();
            names
        })
        .map(|p| p.strip_prefix(dir).unwrap().display().to_string())
        .collect();
    files.sort();
    files
}

#[test]
#[ignore = "minutes long; run in release by the scale workflow"]
fn a_large_project_stays_within_its_budgets() {
    let root = meridian_plugin_testkit::repo_root();
    let budget = budgets(&root.join("tests/scale/budgets.toml"));
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("scale");
    let files = generate(&dir, &budget);
    // Built before the clock starts.
    let plugins = meridian_target_fake::plugin_dir();

    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    let mut plugin_mb: f64 = 0.0;
    let mut meridian_mb: f64 = 0.0;
    for (name, command) in [
        ("compile", vec!["compile"]),
        ("plan", vec!["plan"]),
        ("apply", vec!["apply"]),
        ("destroy", vec!["destroy", "--yes"]),
    ] {
        let mut args: Vec<String> = command.iter().map(|s| s.to_string()).collect();
        args.extend(files.iter().cloned());
        let m = run(&dir, &plugins, &args);
        rows.push((
            format!("{name} (s)"),
            m.seconds,
            budget[&format!("seconds.{name}")],
        ));
        meridian_mb = meridian_mb.max(m.meridian_mb);
        plugin_mb = plugin_mb.max(m.plugin_mb);
    }
    rows.push((
        "meridian peak (MB)".to_owned(),
        meridian_mb,
        budget["megabytes.meridian"],
    ));
    rows.push((
        "plugin peak (MB)".to_owned(),
        plugin_mb,
        budget["megabytes.plugin"],
    ));

    let p = |key: &str| budget[&format!("project.{key}")] as u64;
    let mut table = format!(
        "Scale, tier 1: {} resources, {} endpoints, {} stages (seed {})\n\n\
         | Measure | Now | Budget | Limit |\n|---|---|---|---|\n",
        p("resources"),
        p("endpoints"),
        p("stages"),
        p("seed")
    );
    let mut over = Vec::new();
    for (name, now, budget) in &rows {
        let limit = budget * SLACK;
        table.push_str(&format!(
            "| {name} | {now:.1} | {budget:.1} | {limit:.1} |\n"
        ));
        if *now > limit {
            over.push(format!("{name}: {now:.1}, over {limit:.1}"));
        }
    }
    println!("{table}");
    if let Some(path) = std::env::var_os("MERIDIAN_SCALE_REPORT") {
        fs::write(&path, &table).expect("the report is written");
    }
    let _ = fs::remove_dir_all(&dir);
    assert!(
        over.is_empty(),
        "over budget by more than 20%:\n{}",
        over.join("\n")
    );
}
