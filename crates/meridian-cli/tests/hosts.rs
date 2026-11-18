//! A target host is a name, and nothing a tool reads as more: issue 134.
//!
//! The host goes to ssh, Bolt, `chef-run` and `salt-ssh` as an argument, into
//! an Ansible inventory as a line, and into a Puppet `site.pp` as a string.
//! Each reads some text as more than a name. These tests hold the command
//! line to the rule: a host that is written inline is refused by `validate`,
//! one that is placed from data is refused when it is placed, on every
//! target alike, and the plugins that write the host into a file refuse one
//! that reaches them any other way.

#![allow(clippy::result_large_err)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::graph_from_text;
use meridian_sdk::{Category, Data};

use support::{segment, SHARED_DIALECT};

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hosts-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .output()
        .expect("meridian runs")
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A ResourceSet for `target` whose host is `host`, written as YAML. Its one
/// resource is a `file`, which every target here and the fake one take.
fn document(target: &str, host: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  target: {target}\n  \
         targetHost:\n    from: inline\n    host: {host}\n  resources:\n    - id: motd\n      type: file\n      \
         params:\n        path: /etc/motd\n        content: hello\n"
    )
}

/// The hosts from the issue, and what the refusal must say about each.
const HOSTILE: &[(&str, &str)] = &[
    // An ssh option, with Puppet code after the quote.
    (
        "-oProxyCommand=touch /tmp/pwned' { } exec { 'x': command => 'touch /tmp/pwned' } node 'y",
        "starts with \"-\"",
    ),
    // An inventory line with a host variable and a new section.
    (
        "web01 ansible_connection=local\n[all:vars]\nansible_python_interpreter=/tmp/evil",
        "whitespace",
    ),
    ("x' { } node 'y", "'\\''"),
    ("web01;id", "';'"),
];

#[test]
fn a_hostile_host_written_inline_is_refused_by_validate_on_every_target() {
    let dir = scratch("inline");
    for target in SHARED_DIALECT.iter().chain(["fake", "dsc"].iter()) {
        for (host, says) in HOSTILE {
            let yaml = serde_yaml_quote(host);
            fs::write(dir.join("site.yaml"), document(target, &yaml)).unwrap();
            let output = meridian(&dir, &["validate", "site.yaml"]);
            let said = said(&output);
            assert!(
                !output.status.success()
                    && said.contains("spec.targetHost.host")
                    && said.contains(says),
                "{target}, {host:?}: {said}"
            );
        }
    }
}

#[test]
fn a_hostile_host_placed_from_data_is_refused_when_it_is_placed() {
    let dir = scratch("placed");
    fs::write(
        dir.join("site.yaml"),
        document("fake", "\"${{ data.host }}\""),
    )
    .unwrap();
    for (host, says) in HOSTILE {
        let data = format!("host: {}\n", serde_yaml_quote(host));
        fs::write(dir.join("data.yaml"), data).unwrap();
        let output = meridian(
            &dir,
            &[
                "compile",
                "site.yaml",
                "--data",
                "data.yaml",
                "--out",
                "build",
            ],
        );
        let said = said(&output);
        assert!(
            !output.status.success()
                && said.contains("spec.targetHost.host")
                && said.contains(says),
            "{host:?}: {said}"
        );
    }
    // A host that is a name compiles, and is what the plugin is given.
    fs::write(dir.join("data.yaml"), "host: deploy@web01.example.com\n").unwrap();
    let output = meridian(
        &dir,
        &[
            "compile",
            "site.yaml",
            "--data",
            "data.yaml",
            "--out",
            "build",
        ],
    );
    let said = said(&output);
    assert!(
        output.status.success() && said.contains("target fake on deploy@web01.example.com"),
        "{said}"
    );
}

#[test]
fn the_targets_that_write_the_host_into_a_file_refuse_a_hostile_one_themselves() {
    // The plugin's own line of defence, for a tree that did not come
    // through the core: the targets whose compiled files hold the host as
    // more than a quoted scalar refuse it with the same rule.
    let writes_the_host = ["ansible", "openvox", "puppet"];
    for emitter in segment(Category::ConfigurationManagement) {
        let name = emitter.name().to_owned();
        if !writes_the_host.contains(&name.as_str()) {
            continue;
        }
        let mut graph = graph_from_text("site.yaml", &document(&name, "web01"), &name);
        graph.host = HOSTILE[0].0.to_owned();
        let refused = emitter
            .emit(&graph, &Data::new())
            .expect_err(&format!("{name} should refuse the host"));
        assert_eq!(refused.rule, "invalid-host", "{name}: {refused}");
    }
}

/// `text` as a double-quoted YAML scalar.
fn serde_yaml_quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
