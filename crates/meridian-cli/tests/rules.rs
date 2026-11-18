//! `meridian rules check`, run as a process over real files, including the
//! example the documentation shows.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::repo_root;

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn example() -> PathBuf {
    repo_root().join("examples/compile-rules")
}

/// A copy of the example's configuration that a test may change.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("rules-{test}"));
    let _ = fs::remove_dir_all(&dir);
    copy(&example().join("config"), &dir.join("config"));
    dir
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap().path();
        let target = to.join(entry.file_name().unwrap());
        if entry.is_dir() {
            copy(&entry, &target);
        } else {
            fs::copy(&entry, &target).unwrap();
        }
    }
}

const WEB01: &[&str] = &[
    "rules",
    "check",
    "--hierarchy",
    "config/hierarchy.yaml",
    "--set",
    "env=prod",
    "--set",
    "role=web",
    "--set",
    "node=web01",
];

fn web01_with(extra: &[&'static str]) -> Vec<&'static str> {
    WEB01.iter().copied().chain(extra.iter().copied()).collect()
}

#[test]
fn the_example_passes_for_a_node_that_keeps_the_rules() {
    let out = meridian(&example(), WEB01);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "rules: config/rules.yaml (9 rules)\n\
         layers, least specific first:\n  \
         common.yaml            organization\n  \
         environment/prod.yaml  environment\n  \
         role/web.yaml          team\n  \
         node/web01.yaml        team\n  \
         --set                  (command line)\n\
         no violations\n"
    );
}

#[test]
fn the_example_refuses_the_rogue_node_and_lists_every_violation() {
    let out = meridian(
        &example(),
        &[
            "rules",
            "check",
            "--hierarchy",
            "config/hierarchy.yaml",
            "--set",
            "env=prod",
            "--set",
            "role=web",
            "--set",
            "node=rogue",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    // The same text docs/reference/compile-rules.md shows.
    let want = fs::read_to_string(example().join("rogue.expected.txt")).unwrap();
    assert_eq!(stderr(&out), want);
}

#[test]
fn without_a_rules_file_nothing_is_checked_unless_rules_are_required() {
    let dir = scratch("none");
    fs::remove_file(dir.join("config/rules.yaml")).unwrap();
    let out = meridian(&dir, WEB01);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "no rules file (rules.yaml beside config/hierarchy.yaml): nothing was checked\n"
    );

    let out = meridian(&dir, &web01_with(&["--require-rules"]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: compile rules are required for this run, and there is no rules.yaml beside \
         config/hierarchy.yaml and no --rules file\n"
    );
}

#[test]
fn a_rules_file_kept_elsewhere_is_used_instead_of_the_one_beside_the_hierarchy() {
    let dir = scratch("elsewhere");
    // The one in the repository is emptied; the one the pipeline owns holds.
    fs::write(dir.join("config/rules.yaml"), "rulesVersion: 1\n").unwrap();
    fs::write(
        dir.join("pipeline-rules.yaml"),
        "rulesVersion: 1\nrules:\n  - {id: no-nginx, rule: forbidden-value, keys: [packages], values: [nginx]}\n",
    )
    .unwrap();
    let out = meridian(&dir, &web01_with(&["--rules", "pipeline-rules.yaml"]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: compile rules: 1 violation; nothing was compiled\n  \
         - [no-nginx] layer \"role/web.yaml\" sets \"packages[0]\" to a value the rule forbids (values[0])\n"
    );
}

#[test]
fn adversarial_the_rules_file_cannot_be_made_a_layer() {
    let dir = scratch("as-layer");
    let hierarchy = dir.join("config/hierarchy.yaml");
    // By name, through a `..`, and through a symbolic link.
    std::os::unix::fs::symlink(
        dir.join("config/rules.yaml"),
        dir.join("config/node/linked.yaml"),
    )
    .unwrap();
    for (layer, shown) in [
        ("rules.yaml", "rules.yaml"),
        ("node/../rules.yaml", "rules.yaml"),
        ("node/linked.yaml", "node/linked.yaml"),
    ] {
        let text = fs::read_to_string(&hierarchy).unwrap();
        fs::write(
            &hierarchy,
            text.replace(
                "  - node/%{node}.yaml",
                &format!("  - node/%{{node}}.yaml\n  - {layer}"),
            ),
        )
        .unwrap();
        let out = meridian(&dir, WEB01);
        assert_eq!(out.status.code(), Some(1), "{layer}");
        assert_eq!(
            stderr(&out),
            format!(
                "error: the rules file config/rules.yaml is also the hierarchy layer {shown:?}; the rules file \
                 is never a layer, so take it out of the hierarchy\n"
            ),
            "{layer}"
        );
        fs::write(&hierarchy, text).unwrap();
    }
}

#[test]
fn adversarial_a_set_value_on_the_command_line_is_held_to_the_rules() {
    let dir = scratch("set");
    let out = meridian(&dir, &web01_with(&["--set", "variables.Region=us"]));
    assert_eq!(out.status.code(), Some(1));
    // --set names one key and keeps its siblings, so only the change is
    // reported; nothing is dropped.
    assert_eq!(
        stderr(&out),
        "error: compile rules: 2 violations; nothing was compiled\n  \
         - [meridian/protect-variables] layer \"--set\" ((command line)) changes \"variables.Region\", which \
         layer \"common.yaml\" (organization) set\n  \
         - [region-is-final] layer \"--set\" ((command line)) sets \"variables.Region\", which is final once \
         set, and layer \"common.yaml\" (organization) set it\n"
    );
}

#[test]
fn adversarial_a_data_file_on_the_command_line_is_held_to_the_rules() {
    let dir = scratch("data");
    fs::write(
        dir.join("override.yaml"),
        "variables: {OrganizationName: Fabrikam}\n",
    )
    .unwrap();
    let out = meridian(&dir, &web01_with(&["--data", "override.yaml"]));
    assert_eq!(out.status.code(), Some(1));
    // A --data file replaces the keys it names whole, so it changes the one
    // variable it sets and drops every other variable any layer set.
    let text = stderr(&out);
    assert!(
        text.starts_with("error: compile rules: 9 violations; nothing was compiled\n"),
        "{text}"
    );
    for line in [
        "  - [meridian/protect-variables] layer \"--data override.yaml\" ((command line)) changes \
         \"variables.OrganizationName\", which layer \"common.yaml\" (organization) set\n",
        "  - [meridian/protect-variables] layer \"--data override.yaml\" ((command line)) sets \"variables\", \
         which a command-line value replaces whole, and so drops \"variables.CostCentre\", which layer \
         \"common.yaml\" (organization) set\n",
        "  - [cost-centre-is-set] no layer sets \"variables.CostCentre\", which must be set\n",
    ] {
        assert!(text.contains(line), "{text}");
    }

    // A file that sets nothing protected is let through.
    fs::write(dir.join("override.yaml"), "motd: hello\n").unwrap();
    let out = meridian(&dir, &web01_with(&["--data", "override.yaml"]));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("  --data override.yaml   (command line)\n"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn adversarial_a_layer_outside_every_group_is_refused() {
    let dir = scratch("unclaimed");
    let hierarchy = dir.join("config/hierarchy.yaml");
    let text = fs::read_to_string(&hierarchy).unwrap();
    fs::write(
        &hierarchy,
        text.replace(
            "  - node/%{node}.yaml",
            "  - node/%{node}.yaml\n  - extra.yaml",
        ),
    )
    .unwrap();
    fs::write(dir.join("config/extra.yaml"), "anything: 1\n").unwrap();
    let out = meridian(&dir, WEB01);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: compile rules: 1 violation; nothing was compiled\n  \
         - [meridian/unclaimed-layer] layer \"extra.yaml\" is not claimed by any group under `layers`; a layer \
         nobody owns may set nothing\n"
    );
}

#[test]
fn mistakes_in_the_command_and_the_files_are_plain_errors() {
    let dir = scratch("mistakes");
    let out = meridian(&dir, &web01_with(&["--set", "novalue"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("--set novalue: expected KEY=VALUE"),
        "{}",
        stderr(&out)
    );

    let out = meridian(
        &dir,
        &["rules", "check", "--hierarchy", "config/hierarchy.yaml"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("undefined variable %{env}"),
        "{}",
        stderr(&out)
    );

    fs::write(dir.join("config/rules.yaml"), "rulesVersion: 1\nruls: []\n").unwrap();
    let out = meridian(&dir, WEB01);
    assert_eq!(
        stderr(&out),
        "error: config/rules.yaml: rules file: unknown field \"ruls\"\n"
    );
}
