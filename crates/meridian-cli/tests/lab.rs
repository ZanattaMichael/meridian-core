//! `meridian lab`, run as a process against the fake target: the matrix
//! expands, exclusions and filters leave cells out, cells run side by side
//! and one failing does not stop the others, `list` shows the last run, and
//! `sweep` destroys what a kept or expired run left.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value as Json;

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
    - id: tag
      type: file
      params:
        content: "${{ meridian.run.id }} ${{ meridian.lab.size }} ${{ meridian.lab.zone }}"
  outputs:
    - name: address
      value: "${vm.content}"
"#;

const TEST: &str = r#"apiVersion: meridian/v1
kind: Test
metadata:
  name: site
spec:
  documents: [site.yaml]
  cases:
    - name: network
      run: lifecycle
      assert:
        - stages.network.outputs.address == '10.0.0.5'
"#;

fn lab_file(expiry: &str) -> String {
    format!(
        r#"apiVersion: meridian/v1
kind: Lab
metadata:
  name: nightly
spec:
  environments: [site.test.yaml]
  matrix:
    size: [small, large, FAIL]
    zone: [a, b]
  exclude:
    - {{size: large, zone: b}}
  concurrency: 3
  expiry: {expiry}
"#
    )
}

fn scratch(test: &str, expiry: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("lab")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    fs::write(dir.join("site.test.yaml"), TEST).unwrap();
    fs::write(dir.join("nightly.lab.yaml"), lab_file(expiry)).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .env(
            "MERIDIAN_PLUGIN_DIR",
            meridian_target_fake::plugin_dir().display().to_string(),
        )
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_RUN_ID")
        .stdin(Stdio::null())
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

fn json(output: &Output) -> Json {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(output)))
}

fn valid(output: &Output, schema: &str) -> Json {
    let value = json(output);
    let path = meridian_plugin_testkit::repo_root()
        .join("schemas/output")
        .join(schema);
    let schema: Json = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let errors: Vec<String> = jsonschema::draft7::new(&schema)
        .unwrap()
        .iter_errors(&value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}\n{value:#}");
    value
}

/// Every cell's state directory left under the lab's run.
fn left(dir: &Path, run: &str) -> Vec<String> {
    let base = dir.join(".meridian/lab/nightly").join(run);
    let mut names: Vec<String> = fs::read_dir(&base)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn every_cell_runs_and_one_failing_does_not_stop_the_others() {
    let dir = scratch("matrix", "1h");
    let out = meridian(
        &dir,
        &["lab", "run", "nightly.lab.yaml", "--output", "json"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let result = valid(&out, "lab-run.schema.json");
    let run = result["run"].as_str().unwrap().to_owned();
    let cells: Vec<(String, String, String)> = result["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                c["status"].as_str().unwrap().to_owned(),
                c["runId"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected = [
        ("site: network [size=small, zone=a]", "passed"),
        ("site: network [size=small, zone=b]", "passed"),
        ("site: network [size=large, zone=a]", "passed"),
        ("site: network [size=FAIL, zone=a]", "failed"),
        ("site: network [size=FAIL, zone=b]", "failed"),
    ];
    assert_eq!(cells.len(), expected.len(), "{cells:?}");
    for (i, ((name, status, id), (want, want_status))) in cells.iter().zip(expected).enumerate() {
        assert_eq!((name.as_str(), status.as_str()), (want, want_status));
        assert_eq!(id, &format!("{run}-{}", i + 1));
    }
    assert_eq!(result["summary"]["failed"], 2);
    // A failed provision is still destroyed: nothing is left.
    assert_eq!(left(&dir, &run), Vec::<String>::new());
}

#[test]
fn a_filter_picks_cells_and_junit_has_a_suite_per_cell() {
    let dir = scratch("filter", "1h");
    let out = meridian(
        &dir,
        &[
            "lab",
            "run",
            "nightly.lab.yaml",
            "--filter",
            "size=small",
            "--junit",
            "lab.xml",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("  ok    site: network [size=small, zone=a] (lifecycle, 1 document)\n"),
        "{said}"
    );
    assert!(said.ends_with("2 cells: 2 passed\n"), "{said}");
    let xml = fs::read_to_string(dir.join("lab.xml")).unwrap();
    assert!(
        xml.contains("<testsuite name=\"site: network [size=small, zone=b]\""),
        "{xml}"
    );
    assert!(xml.contains("name=\"network: destroy\""), "{xml}");

    let refused = meridian(
        &dir,
        &["lab", "run", "nightly.lab.yaml", "--filter", "region=x"],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused).contains("--filter region: the lab has no such axis (it has: size, zone)")
    );
}

#[test]
fn list_shows_each_cell_and_how_it_last_ended() {
    let dir = scratch("list", "1h");
    let before = meridian(&dir, &["lab", "list", "nightly.lab.yaml"]);
    assert!(before.status.success(), "{}", text(&before));
    let said = String::from_utf8_lossy(&before.stdout);
    assert!(said.contains("(nightly, never run)"), "{said}");
    assert!(
        said.contains("  not-run   site: network [size=FAIL, zone=b]\n"),
        "{said}"
    );

    let _ = meridian(
        &dir,
        &["lab", "run", "nightly.lab.yaml", "--filter", "zone=b"],
    );
    let after = meridian(
        &dir,
        &["lab", "list", "nightly.lab.yaml", "--output", "json"],
    );
    let result = valid(&after, "lab-list.schema.json");
    let last: Vec<&str> = result["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["last"].as_str().unwrap())
        .collect();
    assert_eq!(last, ["not-run", "passed", "not-run", "not-run", "failed"]);
}

#[test]
fn sweep_destroys_kept_environments_once_they_are_due() {
    let dir = scratch("sweep", "1h");
    let out = meridian(
        &dir,
        &[
            "lab",
            "run",
            "nightly.lab.yaml",
            "--filter",
            "size=small",
            "--keep",
            "--output",
            "json",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let run = json(&out)["run"].as_str().unwrap().to_owned();
    assert_eq!(left(&dir, &run), ["1-network", "2-network"]);
    // Each cell named what it made with its own run id and its values.
    let tag = dir
        .join(".meridian/lab/nightly")
        .join(&run)
        .join("2-network/infrastructure/network/fake/current/files/tag");
    assert_eq!(fs::read_to_string(tag).unwrap(), format!("{run}-2 small b"));

    // Not expired yet.
    let early = meridian(&dir, &["lab", "sweep"]);
    assert!(early.status.success(), "{}", text(&early));
    assert!(text(&early).contains("2 environments: 0 destroyed, 0 failed, 2 not due"));
    assert_eq!(left(&dir, &run).len(), 2);

    let swept = meridian(
        &dir,
        &["lab", "sweep", "--older-than", "0s", "--output", "json"],
    );
    let result = valid(&swept, "lab-sweep.schema.json");
    assert_eq!(result["summary"]["destroyed"], 2, "{result:#}");
    assert_eq!(left(&dir, &run), Vec::<String>::new());
}

#[test]
fn sweep_destroys_what_an_expired_run_left_without_being_told_an_age() {
    let dir = scratch("expired", "1s");
    let out = meridian(
        &dir,
        &[
            "lab",
            "run",
            "nightly.lab.yaml",
            "--filter",
            "size=large",
            "--keep",
            "--output",
            "json",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    let run = json(&out)["run"].as_str().unwrap().to_owned();
    assert_eq!(left(&dir, &run), ["1-network"]);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let swept = meridian(&dir, &["lab", "sweep"]);
    assert!(swept.status.success(), "{}", text(&swept));
    assert!(
        text(&swept).contains("1 environment: 1 destroyed, 0 failed, 0 not due"),
        "{}",
        text(&swept)
    );
    assert_eq!(left(&dir, &run), Vec::<String>::new());
}

#[test]
fn a_lab_with_a_mistake_is_refused_before_anything_runs() {
    let dir = scratch("mistakes", "1h");
    for (body, said) in [
        (
            lab_file("1h").replace("zone: b}", "region: b}"),
            "spec.exclude[0].region: no such axis in spec.matrix",
        ),
        (
            lab_file("1h").replace("zone: [a, b]", "run: [a, b]"),
            "spec.matrix.run: an axis is named with letters, digits and _, and not run or cell",
        ),
        (
            lab_file("1h").replace("name: nightly", "name: Nightly Run"),
            "metadata.name: lowercase letters, digits and -, as it names a directory",
        ),
    ] {
        fs::write(dir.join("bad.lab.yaml"), body).unwrap();
        let out = meridian(&dir, &["lab", "run", "bad.lab.yaml"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(text(&out).contains(said), "{}", text(&out));
        assert!(!dir.join(".meridian/lab").exists());
    }
}
