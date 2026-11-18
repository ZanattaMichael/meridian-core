//! `meridian schema sync`, `status` and `clean`, run as a process against
//! the fake plugin, whose "tool" describes its `file` type.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        content: "10.0.0.4"
    - id: other
      type: file
      params:
        content: x
        owner:
          name: root
          group: staff
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("schema_sync")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command.env("MERIDIAN_ANNOTATIONS", "off");
    command
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_sync_caches_what_the_tool_says_and_records_it_in_the_lock() {
    let dir = scratch("sync");

    // Before: nothing cached, and the plugin says it can export.
    let status = meridian(&dir, &["schema", "status"], &[]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(stdout(&status), "fake: not synced\n");

    // Lint knows `file` by name only: any param goes.
    let loose = SITE.replace(
        "        content: x\n",
        "        content: x\n        colour: blue\n",
    );
    fs::write(dir.join("loose.yaml"), &loose).unwrap();
    let lint = meridian(&dir, &["lint", "loose.yaml"], &[]);
    assert!(lint.status.success(), "{}{}", stdout(&lint), stderr(&lint));

    // The sync.
    let sync = meridian(&dir, &["schema", "sync", "site.yaml"], &[]);
    assert!(sync.status.success(), "{}{}", stdout(&sync), stderr(&sync));
    let out = stdout(&sync);
    assert!(
        out.contains("fake: asking the tool about 2 resources in 1 document"),
        "{out}"
    );
    assert!(
        out.contains("fake: cached 1 resource type from faketool 1.0; fakelib 2.3 in"),
        "{out}"
    );
    assert!(out.contains("recorded in meridian.lock"), "{out}");
    let cache: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join(".meridian/schemas/fake.json")).unwrap())
            .unwrap();
    assert_eq!(cache["target"], "fake");
    assert_eq!(cache["toolVersion"], "1.0");
    assert_eq!(cache["pins"]["fakelib"], "2.3");
    assert_eq!(cache["schemas"][0]["type"], "file");
    assert_eq!(
        cache["schemas"][0]["params"][1]["params"][0]["name"],
        "name"
    );
    assert_eq!(cache["schemas"][0]["params"][2]["computed"], true);
    let lock = fs::read_to_string(dir.join("meridian.lock")).unwrap();
    assert!(lock.contains("schemas:\n  - target: fake\n    tool: faketool\n    toolVersion: \"1.0\"\n    pins:\n      fakelib: \"2.3\"\n    integrity: h1:"), "{lock}");
    // The tool was asked in its own work directory, about the types used.
    let log = fs::read_to_string(dir.join(".meridian/schemas/work/fake/schemas.log")).unwrap();
    assert_eq!(log, "types: file\n");

    // After: status says current, and the check passes.
    let status = meridian(&dir, &["schema", "status"], &[]);
    let shown = stdout(&status);
    assert!(
        shown.starts_with("fake: 1 resource type from faketool 1.0; fakelib 2.3, synced 20"),
        "{shown}"
    );
    assert!(shown.trim_end().ends_with(": current"), "{shown}");
    let json = meridian(&dir, &["schema", "status", "--json"], &[]);
    let line: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert_eq!(line["state"], "current");
    assert_eq!(line["cached"]["types"], 1);
    assert_eq!(line["locked"]["pins"]["fakelib"], "2.3");
    let check = meridian(&dir, &["schema", "sync", "--check", "site.yaml"], &[]);
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );

    // Lint now checks `file` against the tool's schema, where the plugin's
    // own description left it open.
    let lint = meridian(&dir, &["lint", "loose.yaml"], &[]);
    assert!(!lint.status.success());
    let out = stdout(&lint);
    assert!(out.contains("unsupported-param"), "{out}");
    assert!(out.contains("does not take param \"colour\""), "{out}");
    let nested = SITE.replace("          group: staff\n", "          group: nobody\n");
    fs::write(dir.join("nested.yaml"), &nested).unwrap();
    let lint = meridian(&dir, &["lint", "nested.yaml"], &[]);
    let out = stdout(&lint);
    assert!(
        out.contains("param \"owner.group\" does not take \"nobody\""),
        "{out}"
    );
    assert!(stdout(&meridian(&dir, &["lint", "site.yaml"], &[])).contains("nothing to report"));

    // The JSON Schema export carries the nested params too.
    let schema = meridian(&dir, &["schema", "--target", "fake"], &[]);
    let schema: serde_json::Value = serde_json::from_str(&stdout(&schema)).unwrap();
    let file = &schema["definitions"]["Resource.fake"]["allOf"][2]["then"]["properties"]["params"];
    assert_eq!(
        file["properties"]["owner"]["properties"]["name"]["type"],
        json_text()
    );
    assert_eq!(
        file["properties"]["owner"]["required"],
        serde_json::json!(["name"])
    );
    assert!(
        file["properties"].get("size").is_none(),
        "what the tool sets is not written: {file}"
    );

    // A changed cache file is reported as differing, and fails the check.
    let path = dir.join(".meridian/schemas/fake.json");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("\"1.0\"", "\"1.1\""),
    )
    .unwrap();
    let status = meridian(&dir, &["schema", "status"], &[]);
    assert!(
        stdout(&status).contains("differs from what meridian.lock records"),
        "{}",
        stdout(&status)
    );
    let check = meridian(&dir, &["schema", "sync", "--check", "site.yaml"], &[]);
    assert!(!check.status.success());
    assert!(
        stderr(&check).contains("1 cache not current"),
        "{}",
        stderr(&check)
    );

    // Clean forgets the cache and the record.
    let clean = meridian(&dir, &["schema", "clean"], &[]);
    assert!(clean.status.success(), "{}", stderr(&clean));
    assert_eq!(stdout(&clean), "fake: forgotten\n");
    assert!(!path.exists());
    assert!(
        !dir.join("meridian.lock").exists(),
        "a lockfile with nothing in it is removed"
    );
    assert_eq!(
        stdout(&meridian(&dir, &["schema", "status"], &[])),
        "fake: not synced\n"
    );
}

fn json_text() -> serde_json::Value {
    // A param may always hold a `${...}` reference, so a string stays a
    // string.
    serde_json::json!(["string", "number", "boolean"])
}

#[test]
fn a_tool_that_fails_and_a_plugin_that_does_not_export_are_reported() {
    let dir = scratch("failures");
    let sync = meridian(
        &dir,
        &["schema", "sync", "site.yaml"],
        &[("FAKE_SCHEMAS", "fail")],
    );
    assert!(!sync.status.success());
    let err = stderr(&sync);
    assert!(
        err.contains("faketool could not describe its types"),
        "{err}"
    );
    assert!(err.contains("no such library"), "{err}");
    assert!(!dir.join(".meridian/schemas/fake.json").exists());
    assert!(!dir.join("meridian.lock").exists());

    let sync = meridian(
        &dir,
        &["schema", "sync", "site.yaml"],
        &[("FAKE_SCHEMAS", "none")],
    );
    assert!(sync.status.success(), "{}", stderr(&sync));
    assert_eq!(
        stdout(&sync),
        "fake: the plugin does not export its tool's schemas\n"
    );
    let status = meridian(&dir, &["schema", "status"], &[("FAKE_SCHEMAS", "none")]);
    assert_eq!(
        stdout(&status),
        "fake: the plugin does not export its tool's schemas\n"
    );

    // A target no document uses cannot be synced by name.
    let sync = meridian(
        &dir,
        &["schema", "sync", "--target", "nope", "site.yaml"],
        &[],
    );
    assert!(!sync.status.success());
    assert!(
        stderr(&sync)
            .contains("no document in the paths given uses target \"nope\" (they use: fake)"),
        "{}",
        stderr(&sync)
    );

    // Nothing to sync in an empty directory.
    fs::remove_file(dir.join("site.yaml")).unwrap();
    let sync = meridian(&dir, &["schema", "sync", "."], &[]);
    assert!(!sync.status.success());
    assert!(
        stderr(&sync).contains("no Meridian documents were found"),
        "{}",
        stderr(&sync)
    );
}

#[test]
fn a_sync_keeps_the_modules_the_lock_pins_and_module_install_keeps_the_schemas() {
    let dir = scratch("lock");
    fs::write(
        dir.join("meridian.lock"),
        "# Written by `meridian module`. Commit it; do not edit it.\nlockVersion: 1\nmodules:\n  - name: acme/base\n    version: 1.0.0\n    source: path+../base\n",
    )
    .unwrap();
    let sync = meridian(&dir, &["schema", "sync", "site.yaml"], &[]);
    assert!(sync.status.success(), "{}", stderr(&sync));
    let lock = fs::read_to_string(dir.join("meridian.lock")).unwrap();
    assert!(lock.contains("  - name: acme/base\n"), "{lock}");
    assert!(lock.contains("schemas:\n  - target: fake\n"), "{lock}");

    // Without the lock: cached, unrecorded.
    let dir = scratch("no-lock");
    let sync = meridian(&dir, &["schema", "sync", "--no-lock", "site.yaml"], &[]);
    assert!(sync.status.success(), "{}", stderr(&sync));
    assert!(!dir.join("meridian.lock").exists());
    assert!(stdout(&meridian(&dir, &["schema", "status"], &[]))
        .contains("not recorded in meridian.lock"));
}
