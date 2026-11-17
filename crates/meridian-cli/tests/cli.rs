//! The `meridian` command, run as a process against a real plugin.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const WORKFLOW: &str = r#"apiVersion: meridian/v1
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
    - id: key
      type: file
      params:
        content: hunter2-very-secret
  outputs:
    - name: address
      value: "${vm.content}"
    - name: secret_key
      value: "${key.content}"
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
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
"#;

/// A scratch directory holding the workflow file, per test.
fn scratch(test: &str, workflow: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), workflow).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn validate_accepts_good_documents_and_points_at_the_problem_in_bad_ones() {
    let dir = scratch("validate", WORKFLOW);
    let ok = meridian(&dir, &["validate", "site.yaml"]);
    assert!(ok.status.success(), "{}", stderr(&ok));
    assert_eq!(stdout(&ok), "2 documents valid\n");

    // A field that does not exist, reported at its own line.
    let typo = WORKFLOW.replace("    - id: motd\n", "    - id: motd\n      colour: blue\n");
    assert_ne!(typo, WORKFLOW);
    fs::write(dir.join("site.yaml"), typo).unwrap();
    let bad = meridian(&dir, &["validate", "site.yaml"]);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(stdout(&bad), "");
    assert_eq!(
        stderr(&bad),
        "error: site.yaml[doc 1]:34:7: spec.resources[0]: unknown field \"colour\"\n"
    );

    // A document that parses but points at one that does not exist.
    let dangling = WORKFLOW.replace("ref: network", "ref: nowhere");
    fs::write(dir.join("site.yaml"), dangling).unwrap();
    let bad = meridian(&dir, &["validate", "site.yaml"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(stderr(&bad).contains("nowhere"), "{}", stderr(&bad));
}

#[test]
fn plugins_lists_what_each_plugin_is_and_can_do() {
    let dir = scratch("plugins", WORKFLOW);
    let out = meridian(&dir, &["plugins"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let binary = meridian_target_fake::plugin_path();
    assert_eq!(
        text,
        format!(
            "fake\n  category:  containerization\n  can:       compile, plan, apply, outputs, inputs, import\n  \
             resources: file\n  versions:  1, 2.4\n  imports:   Fake files (*.fake)\n  sdk:       0.1.0 (protocol 2)\n  binary:    {}\n",
            binary.display()
        )
    );
}

#[test]
fn compile_writes_each_documents_files_and_applies_nothing() {
    let dir = scratch("compile", WORKFLOW);
    let out = meridian(&dir, &["compile", "site.yaml", "--out", "build"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "network (Infrastructure, target fake)\n  compiled 4 files into build/network\n\
         web (ResourceSet, target fake)\n  deferred: needs the output \"address\" of Infrastructure \"network\", \
         which has not been applied yet\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("build/network/files/vm")).unwrap(),
        "10.0.0.5"
    );
    assert_eq!(
        fs::read_to_string(dir.join("build/network/outputs/address")).unwrap(),
        "${vm.content}"
    );
    assert!(!dir.join(".meridian/state.json").exists());
}

#[test]
fn plan_then_apply_then_plan_tells_the_whole_story() {
    let dir = scratch("lifecycle", WORKFLOW);

    let plan = meridian(&dir, &["plan", "site.yaml"]);
    assert!(plan.status.success(), "{}", stderr(&plan));
    assert_eq!(
        stdout(&plan),
        "network (Infrastructure, target fake)\n\
         \x20 + create files/key: 19 bytes\n\
         \x20 + create files/vm: 8 bytes\n\
         \x20 + create outputs/address: 13 bytes\n\
         \x20 + create outputs/secret_key: 14 bytes\n\
         \x20 4 changes to make\n\
         web (ResourceSet, target fake)\n\
         \x20 deferred: needs the output \"address\" of Infrastructure \"network\", which has not been applied yet\n"
    );

    let apply = meridian(&dir, &["apply", "site.yaml"]);
    assert!(apply.status.success(), "{}", stderr(&apply));
    let text = stdout(&apply);
    assert!(text.contains("  applied 4 changes\n"), "{text}");
    assert!(text.contains("  output address = \"10.0.0.5\"\n"), "{text}");
    assert!(
        text.contains("  output secret_key = (sensitive)\n"),
        "{text}"
    );
    assert!(text.contains("web (ResourceSet, target fake on 10.0.0.5)\n  + create files/motd: 8 bytes\n  applied 1 change\n"), "{text}");
    // The tool's own output is on stderr, and the secret is in neither.
    let log = stderr(&apply);
    assert!(log.contains("  network | create files/vm\n"), "{log}");
    assert!(log.contains("  web ! inputs: "), "{log}");
    assert!(
        !text.contains("hunter2") && !log.contains("hunter2"),
        "{text}{log}"
    );
    assert_eq!(
        fs::read_to_string(dir.join(".meridian/resourceset/web/fake/current/files/motd")).unwrap(),
        "welcome\n"
    );

    let again = meridian(&dir, &["plan", "site.yaml"]);
    assert!(again.status.success());
    assert_eq!(
        stdout(&again),
        "network (Infrastructure, target fake)\n  in sync, nothing to change\n\
         web (ResourceSet, target fake on 10.0.0.5)\n  in sync, nothing to change\n"
    );
}

#[test]
fn a_failed_apply_exits_non_zero_and_says_which_stage_and_why() {
    let dir = scratch(
        "failure",
        &WORKFLOW.replace("content: 10.0.0.5", "content: FAIL"),
    );
    let out = meridian(&dir, &["apply", "site.yaml", "--state-dir", "state"]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(
        text.contains(
            "  FAILED: could not write files/vm (ran `fake-tool apply --password=***`, exit status 3)\n    disk on fire\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("web (ResourceSet, target fake)\n  not run, because Infrastructure \"network\" failed\n"),
        "{text}"
    );
    assert!(
        dir.join("state/state.json").exists(),
        "--state-dir was not used"
    );
}

#[test]
fn a_missing_plugin_is_a_clear_failure_and_usage_errors_exit_2() {
    let dir = scratch(
        "no-plugin",
        &WORKFLOW.replace("target: fake", "target: terraform"),
    );
    let out = meridian(&dir, &["plan", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out)
            .contains("  FAILED: no plugin provides target \"terraform\" (loaded targets: fake)\n"),
        "{}",
        stdout(&out)
    );

    assert_eq!(meridian(&dir, &["plan"]).status.code(), Some(2));
    assert_eq!(meridian(&dir, &["frobnicate"]).status.code(), Some(2));
    let missing = meridian(&dir, &["validate", "nope.yaml"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("nope.yaml"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn validate_finds_a_dependency_cycle_without_loading_a_plugin() {
    let cycle = WORKFLOW.replace(
        "    - id: key\n      type: file\n",
        "    - id: key\n      type: file\n      dependsOn:\n        - resource: lock\n    \
         - id: lock\n      type: file\n      dependsOn:\n        - resource: key\n",
    );
    assert_ne!(cycle, WORKFLOW);
    let dir = scratch("validate-cycle", &cycle);
    // No plugin directory at all: the check is the core's own.
    let out = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(["validate", "site.yaml"])
        .current_dir(&dir)
        .env("MERIDIAN_PLUGIN_DIR", dir.join("no-plugins-here"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    assert_eq!(
        stderr(&out),
        "error: site.yaml:12:7: spec.resources[1].dependsOn: dependency cycle among 2 \
         resources: key -> lock -> key\n"
    );

    // The same document is refused the same way by every other command.
    let out = meridian(&dir, &["compile", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("dependency cycle among 2 resources"));
}

#[test]
fn validate_refuses_a_notify_event_no_target_can_fire_on() {
    let odd = WORKFLOW.replace(
        "    - id: key\n      type: file\n",
        "    - id: key\n      type: file\n      notifies:\n        - resource: vm\n          \
         action: restart\n          on: always\n",
    );
    let dir = scratch("validate-on", &odd);
    let out = meridian(&dir, &["validate", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("spec.resources[1].notifies[0].on: unknown event \"always\""),
        "{}",
        stderr(&out)
    );
    fs::write(
        dir.join("site.yaml"),
        odd.replace("on: always", "on: changed"),
    )
    .unwrap();
    let out = meridian(&dir, &["validate", "site.yaml"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

const LABELLED: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
  labels:
    tier: web
spec:
  target: fake
  targetHost: {from: inline, host: web01}
  resources:
    - id: motd
      type: file
      params: {content: web}
    - id: debug
      type: file
      when: "data.env != 'prod'"
      params: {content: on}
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: db
  labels:
    tier: db
spec:
  target: fake
  targetHost: {from: inline, host: db01}
  resources:
    - id: motd
      type: file
      params: {content: db}
"#;

#[test]
fn a_selector_runs_only_the_documents_whose_labels_match() {
    let dir = scratch("selector", LABELLED);
    fs::write(dir.join("prod.yaml"), "env: prod\n").unwrap();
    let compile = |args: &[&str]| {
        let mut all = vec!["compile", "site.yaml", "--data", "prod.yaml"];
        all.extend_from_slice(args);
        meridian(&dir, &all)
    };

    let both = compile(&[]);
    assert!(both.status.success(), "{}", stderr(&both));
    let text = stdout(&both);
    assert!(
        text.contains("web (ResourceSet") && text.contains("db (ResourceSet"),
        "{text}"
    );

    for flags in [
        &["-l", "tier=db"][..],
        &["--selector", "tier!=web"],
        &["-l", "tier", "-l", "tier=db"],
    ] {
        let only = compile(flags);
        assert!(only.status.success(), "{}", stderr(&only));
        let text = stdout(&only);
        assert!(
            text.starts_with("db (ResourceSet, target fake on db01)\n"),
            "{text}"
        );
        assert!(!text.contains("web ("), "{text}");
    }

    // Selecting nothing is said out loud, not reported as a success.
    let none = compile(&["-l", "tier=cache"]);
    assert_eq!(none.status.code(), Some(1));
    assert_eq!(
        stderr(&none),
        "error: no document's labels match the selector tier=cache\n"
    );
    let bad = compile(&["-l", "=web"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(
        stderr(&bad).contains("has no usable label name"),
        "{}",
        stderr(&bad)
    );
}

#[test]
fn a_when_is_decided_from_the_data_files_before_anything_is_compiled() {
    let dir = scratch("when", LABELLED);
    fs::write(dir.join("base.yaml"), "env: dev\n").unwrap();
    fs::write(dir.join("prod.json"), "{\"env\": \"prod\"}").unwrap();
    let files = |args: &[&str]| {
        let mut all = vec!["compile", "site.yaml", "-l", "tier=web", "--out", "build"];
        all.extend_from_slice(args);
        let out = meridian(&dir, &all);
        let _ = fs::remove_dir_all(dir.join("build-seen"));
        if dir.join("build").exists() {
            fs::rename(dir.join("build"), dir.join("build-seen")).unwrap();
        }
        out
    };
    let compiled = |name: &str| dir.join("build-seen/web/files").join(name).exists();

    // In dev the debug file is part of the output.
    let dev = files(&["--data", "base.yaml"]);
    assert!(dev.status.success(), "{}", stderr(&dev));
    assert!(compiled("motd") && compiled("debug"), "{}", stdout(&dev));

    // A later data file replaces an earlier one's key, and in prod the
    // resource is simply not there.
    let prod = files(&["--data", "base.yaml", "--data", "prod.json"]);
    assert!(prod.status.success(), "{}", stderr(&prod));
    assert!(compiled("motd") && !compiled("debug"), "{}", stdout(&prod));

    // With no data the condition cannot be decided, and nothing is guessed.
    let none = files(&[]);
    assert_eq!(none.status.code(), Some(1));
    assert!(
        stdout(&none).contains(
            "FAILED: site.yaml:14:7: spec.resources[1].when: resource \"debug\": data.env is \
             not set"
        ),
        "{}",
        stdout(&none)
    );

    // A data file that is not a mapping is refused by name.
    fs::write(dir.join("list.yaml"), "- a\n- b\n").unwrap();
    let bad = files(&["--data", "list.yaml"]);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(
        stderr(&bad),
        "error: the data file list.yaml must hold a mapping of names to values, and it holds \
         a list\n"
    );
    let gone = files(&["--data", "absent.yaml"]);
    assert_eq!(gone.status.code(), Some(1));
    assert!(stderr(&gone).starts_with("error: reading the data file absent.yaml: "));
}

#[test]
fn a_target_version_the_target_does_not_compile_for_is_refused() {
    let versioned = |version: &str| {
        LABELLED.replacen(
            "  target: fake\n",
            &format!("  target: fake\n  targetVersion: \"{version}\"\n"),
            1,
        )
    };
    let dir = scratch("target-version", &versioned("2.4.1"));
    fs::write(dir.join("data.yaml"), "env: prod\n").unwrap();
    let args = ["compile", "site.yaml", "--data", "data.yaml"];
    let ok = meridian(&dir, &args);
    assert!(ok.status.success(), "{}{}", stdout(&ok), stderr(&ok));

    fs::write(dir.join("site.yaml"), versioned("9")).unwrap();
    let bad = meridian(&dir, &args);
    assert_eq!(bad.status.code(), Some(1));
    assert!(
        stdout(&bad).contains(
            "FAILED: the document asks for targetVersion \"9\", and target \"fake\" compiles \
             for: 1, 2.4"
        ),
        "{}",
        stdout(&bad)
    );
}

const FAKE_SOURCE: &str =
    "# What the fake tool already has.\nmotd = welcome\nissue = authorised use only\n";

#[test]
fn import_writes_a_document_that_compiles_and_reports_what_it_read() {
    let dir = scratch("import", WORKFLOW);
    fs::create_dir_all(dir.join("legacy")).unwrap();
    fs::write(dir.join("legacy/files.fake"), FAKE_SOURCE).unwrap();

    // Printed when no directory is given: the document on stdout, the report
    // on stderr. The plugin is found from the file's name.
    let out = meridian(&dir, &["import", "legacy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let document = stdout(&out);
    assert!(
        document.starts_with("# Imported from legacy by `meridian import`."),
        "{document}"
    );
    assert!(
        document
            .contains("kind: Infrastructure\nmetadata:\n  name: legacy\nspec:\n  target: fake\n"),
        "{document}"
    );
    assert!(document.contains("    # From files.fake:2\n    - id: motd\n      type: file\n      params:\n        content: welcome\n"), "{document}");
    assert_eq!(
        stderr(&out),
        "imported legacy with the fake plugin: 1 document, 2 resources\n  legacy (Infrastructure, 2 resources)\n\
         everything in the source was carried over\n"
    );

    // Written when one is, and what was written compiles.
    let out = meridian(
        &dir,
        &["import", "legacy", "--name", "old", "-o", "imported"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("  old (Infrastructure, 2 resources) -> imported/old.yaml\n"),
        "{}",
        stderr(&out)
    );
    let compiled = meridian(&dir, &["compile", "imported/old.yaml", "--out", "build"]);
    assert!(compiled.status.success(), "{}", stderr(&compiled));
    assert_eq!(
        fs::read_to_string(dir.join("build/old/files/motd")).unwrap(),
        "welcome"
    );

    // A document that is already there is not replaced unless asked.
    let again = meridian(
        &dir,
        &["import", "legacy", "--name", "old", "-o", "imported"],
    );
    assert_eq!(again.status.code(), Some(1));
    assert!(
        stderr(&again).contains("already exists; pass --force"),
        "{}",
        stderr(&again)
    );
    let forced = meridian(
        &dir,
        &[
            "import", "legacy", "--name", "old", "-o", "imported", "--force",
        ],
    );
    assert!(forced.status.success(), "{}", stderr(&forced));
}

#[test]
fn import_marks_what_was_not_carried_over_and_strict_makes_it_an_error() {
    let dir = scratch("import-lost", WORKFLOW);
    fs::write(
        dir.join("files.fake"),
        format!("{FAKE_SOURCE}? a symbolic link\n"),
    )
    .unwrap();

    let out = meridian(&dir, &["import", "files.fake", "-o", "imported"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = stderr(&out);
    assert!(
        report.contains("files.fake:4: a symbolic link: the fake tool has nothing like this, so it was left out"),
        "{report}"
    );
    assert!(
        report.contains("1 thing to look at before applying"),
        "{report}"
    );
    let document = fs::read_to_string(dir.join("imported/files.yaml")).unwrap();
    assert!(
        document.contains("# TODO(meridian-import): not translated: files.fake:4: a symbolic link"),
        "{document}"
    );

    // The same import as a check: the document is still written.
    let strict = meridian(&dir, &["import", "files.fake", "-o", "strict", "--strict"]);
    assert_eq!(strict.status.code(), Some(1));
    assert!(
        stderr(&strict).contains("error: --strict: 1"),
        "{}",
        stderr(&strict)
    );
    assert!(dir.join("strict/files.yaml").exists());
}

#[test]
fn import_says_where_a_source_is_wrong_and_when_no_plugin_reads_it() {
    let dir = scratch("import-errors", WORKFLOW);
    fs::write(dir.join("files.fake"), "motd = welcome\nBROKEN\n").unwrap();
    let broken = meridian(&dir, &["import", "files.fake"]);
    assert_eq!(broken.status.code(), Some(1));
    assert_eq!(stdout(&broken), "");
    assert!(
        stderr(&broken).contains("files.fake:2: this line is broken"),
        "{}",
        stderr(&broken)
    );

    fs::write(dir.join("notes.txt"), "nothing\n").unwrap();
    let unknown = meridian(&dir, &["import", "notes.txt"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stderr(&unknown).contains("--from"), "{}", stderr(&unknown));

    let missing = meridian(&dir, &["import", "files.fake", "--from", "nonesuch"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("nonesuch"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn version_shows_every_version_stream_of_the_build() {
    let dir = scratch("version", WORKFLOW);
    let out = meridian(&dir, &["version"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "meridian {}\n  language         meridian/v1  supported\n  plugin sdk       {}\n  plugin protocol  2\n  lockfile format  1\n",
            env!("CARGO_PKG_VERSION"),
            meridian_sdk::VERSION
        )
    );
}

#[test]
fn an_api_version_this_build_does_not_read_is_refused_with_the_reason() {
    for (version, why) in [
        (
            "meridian/v2",
            "it is newer than this build, which understands meridian/v1; upgrade meridian",
        ),
        (
            "meridian/v1.0",
            "versions are whole numbers, as in \"meridian/v1\"",
        ),
        ("apps/v1", "expected \"meridian/v1\""),
    ] {
        let dir = scratch(
            "api-version",
            &WORKFLOW.replacen(
                "apiVersion: meridian/v1",
                &format!("apiVersion: {version}"),
                1,
            ),
        );
        let out = meridian(&dir, &["validate", "site.yaml"]);
        assert!(!out.status.success());
        assert_eq!(
            stderr(&out),
            format!(
                "error: site.yaml:1:1: apiVersion: unsupported apiVersion {version:?}: {why}\n"
            )
        );
    }
}

#[test]
fn migrate_leaves_current_documents_alone_and_refuses_what_it_cannot_do() {
    let dir = scratch("migrate", WORKFLOW);
    for args in [
        &["migrate", "site.yaml"][..],
        &["migrate", "site.yaml", "--check"],
        &["migrate", "site.yaml", "--to", "meridian/v1"],
    ] {
        let out = meridian(&dir, args);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(stdout(&out), "site.yaml: already at meridian/v1\n");
    }
    assert_eq!(fs::read_to_string(dir.join("site.yaml")).unwrap(), WORKFLOW);

    let out = meridian(&dir, &["migrate", "site.yaml", "--to", "meridian/v2"]);
    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        "error: --to: unsupported apiVersion \"meridian/v2\": it is newer than this build, \
         which understands meridian/v1; upgrade meridian\n"
    );

    fs::write(
        dir.join("k8s.yaml"),
        "apiVersion: apps/v1\nkind: Deployment\n",
    )
    .unwrap();
    let out = meridian(&dir, &["migrate", "k8s.yaml", "missing.yaml"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains(
            "error: k8s.yaml: apiVersion \"apps/v1\" is not a Meridian language version\n"
        ),
        "{err}"
    );
    assert!(
        err.contains("error: missing.yaml: cannot read file: "),
        "{err}"
    );
}
