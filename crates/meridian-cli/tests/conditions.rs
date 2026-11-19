//! `when` through the real command: the whole expression language, `--set`,
//! and the examples under `examples/conditions/` with the values that switch
//! each of their resources on and off.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::{plugin_binary, repo_root};

/// A scratch directory, per test.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("conditions-{test}"));
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A document for the fake target with one resource per condition. The fake
/// writes one file per resource, so the files are the resources kept.
fn document(conditions: &[(&str, &str)]) -> String {
    let mut text = String::from(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: site}\nspec:\n  target: \
         fake\n  targetHost: {from: inline, host: h}\n  resources:\n    - id: always\n      \
         type: file\n      params: {content: x}\n",
    );
    for (id, condition) in conditions {
        text.push_str(&format!(
            "    - id: {id}\n      type: file\n      when: {}\n      params: {{content: x}}\n",
            // As a JSON string, which is also a YAML one, so any condition
            // can be written without thinking about YAML's own quoting.
            meridian_sdk::Value::String((*condition).to_owned()).canonical()
        ));
    }
    text
}

/// Compiles `site.yaml` in `dir` and returns the ids of the resources kept.
fn kept(dir: &Path, args: &[&str]) -> Result<BTreeSet<String>, String> {
    let _ = fs::remove_dir_all(dir.join("build"));
    let mut all = vec!["compile", "site.yaml", "--out", "build"];
    all.extend_from_slice(args);
    let output = meridian(dir, &all);
    if !output.status.success() {
        return Err(format!("{}{}", stdout(&output), stderr(&output)));
    }
    Ok(fs::read_dir(dir.join("build/site/files"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect())
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

const DATA: &str = "env: prod\nreplicas: 3\nregion: {name: eu-west-1}\nports: [80, 443]\n\
                    features: {metrics: {}}\nversion: \"1.6.2\"\n";

#[test]
fn every_kind_of_condition_keeps_or_removes_its_resource() {
    let dir = scratch("kinds");
    fs::write(dir.join("data.yaml"), DATA).unwrap();
    fs::write(
        dir.join("site.yaml"),
        document(&[
            ("compare_yes", "data.replicas >= 3"),
            ("compare_no", "data.replicas < 3"),
            ("arithmetic_yes", "data.replicas * 2 + 1 == 7"),
            ("in_yes", "443 in data.ports"),
            ("in_no", "22 in data.ports"),
            ("not_in_yes", "data.env not in ['dev', 'test']"),
            ("key_yes", "'metrics' in data.features"),
            ("pattern_yes", "data.region.name =~ '^eu-'"),
            ("pattern_no", "data.region.name !~ '^eu-'"),
            (
                "choice_yes",
                "(data.env == 'prod' ? 3 : 1) == data.replicas",
            ),
            ("fallback_yes", "data.tls.enabled ?? true"),
            ("fallback_no", "data.missing ?? false"),
            (
                "function_yes",
                "length(data.ports) == 2 && upper(data.env) == 'PROD'",
            ),
            ("function_no", "startsWith(data.env, 'dev')"),
            ("map_yes", "all(map(data.ports, 'p', p < 1024))"),
            ("filter_no", "length(filter(data.ports, 'p', p > 1000)) > 0"),
            ("version_yes", "semverSatisfies(data.version, '>=1.6 <2')"),
            ("index_yes", "data.ports[0] == 80 && data.ports[-1] == 443"),
            ("literal_yes", "[1, 2] == [1.0, 2.0] && {a: 1}.a == 1"),
        ]),
    )
    .unwrap();
    let kept = kept(&dir, &["--data", "data.yaml"]).unwrap();
    let want: BTreeSet<String> = [
        "always",
        "compare_yes",
        "arithmetic_yes",
        "in_yes",
        "not_in_yes",
        "key_yes",
        "pattern_yes",
        "choice_yes",
        "fallback_yes",
        "function_yes",
        "map_yes",
        "version_yes",
        "index_yes",
        "literal_yes",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(kept, want);
}

#[test]
fn set_gives_one_value_and_wins_over_the_data_files() {
    let dir = scratch("set");
    fs::write(dir.join("data.yaml"), DATA).unwrap();
    fs::write(
        dir.join("site.yaml"),
        document(&[
            ("prod", "data.env == 'prod'"),
            ("many", "data.replicas > 1"),
            ("eu", "data.region.name =~ '^eu-'"),
            ("zoned", "defined(data.region.zone)"),
            ("flag", "data.flag ?? false"),
        ]),
    )
    .unwrap();
    let kept_with = |args: &[&str]| {
        let mut all = vec!["--data", "data.yaml"];
        all.extend_from_slice(args);
        kept(&dir, &all).unwrap()
    };

    assert_eq!(kept_with(&[]), set(&["always", "prod", "many", "eu"]));
    // A value replaces the data file's.
    assert_eq!(
        kept_with(&["--set", "env=dev"]),
        set(&["always", "many", "eu"])
    );
    // It is typed as YAML would type it: 1 is a number, true is a boolean.
    assert_eq!(
        kept_with(&["--set", "replicas=1", "--set", "flag=true"]),
        set(&["always", "prod", "eu", "flag"])
    );
    // A dotted key sets one key inside a mapping and keeps its neighbours.
    assert_eq!(
        kept_with(&["--set", "region.zone=a"]),
        set(&["always", "prod", "many", "eu", "zoned"])
    );
    assert_eq!(
        kept_with(&["--set", "region.name=us-east-1"]),
        set(&["always", "prod", "many"])
    );
    // A later --set wins over an earlier one.
    assert_eq!(
        kept_with(&["--set", "env=dev", "--set", "env=prod"]),
        set(&["always", "prod", "many", "eu"])
    );
    // It can create mappings that no file holds, and hold a list.
    assert_eq!(
        kept(
            &dir,
            &[
                "--set",
                "env=prod",
                "--set",
                "replicas=2",
                "--set",
                "region.name=eu-north-1",
            ]
        )
        .unwrap(),
        set(&["always", "prod", "many", "eu"])
    );
    // Text that is not a YAML value is the text it is.
    fs::write(
        dir.join("site.yaml"),
        document(&[
            ("url", "data.url == 'http://a:b@c'"),
            ("eq", "data.eq == 'a=b'"),
            ("list", "data.list == [1, 2]"),
            ("quoted", "data.version == '1.10'"),
            ("empty", "data.empty == null"),
        ]),
    )
    .unwrap();
    assert_eq!(
        kept(
            &dir,
            &[
                "--set",
                "url=http://a:b@c",
                "--set",
                "eq=a=b",
                "--set",
                "list=[1, 2]",
                "--set",
                "version=\"1.10\"",
                "--set",
                "empty=",
            ]
        )
        .unwrap(),
        set(&["always", "url", "eq", "list", "quoted", "empty"])
    );
}

#[test]
fn a_set_that_cannot_be_applied_is_refused_before_anything_runs() {
    let dir = scratch("set-refused");
    fs::write(dir.join("data.yaml"), DATA).unwrap();
    fs::write(dir.join("site.yaml"), document(&[])).unwrap();
    for (flags, want) in [
        (
            &["--set", "env"][..],
            "error: --set env: expected KEY=VALUE, as in --set env=prod\n",
        ),
        (
            &["--set", "=prod"],
            "error: --set =prod: the key is empty, or has an empty part between dots\n",
        ),
        (
            &["--set", "region..name=x"],
            "error: --set region..name=x: the key is empty, or has an empty part between dots\n",
        ),
        (
            &["--data", "data.yaml", "--set", "env.name=x"],
            "error: --set env.name=x: env is already set to a string, so it cannot hold the key \
             \"name\"\n",
        ),
        (
            &["--data", "data.yaml", "--set", "ports.first.x=1"],
            "error: --set ports.first.x=1: ports is already set to a list, so it cannot hold \
             the key \"first\"\n",
        ),
    ] {
        let mut all = vec!["compile", "site.yaml"];
        all.extend_from_slice(flags);
        let output = meridian(&dir, &all);
        assert_eq!(output.status.code(), Some(1), "{flags:?}");
        assert_eq!(stderr(&output), want, "{flags:?}");
        assert_eq!(stdout(&output), "", "{flags:?}");
    }
}

#[test]
fn a_condition_that_cannot_be_decided_stops_the_document_and_names_the_line() {
    let dir = scratch("undecided");
    fs::write(dir.join("data.yaml"), DATA).unwrap();
    for (condition, want) in [
        (
            "data.nope",
            "site.yaml:11:7: spec.resources[1].when: resource \"r\": data.nope is not set (give \
             it with --data or --set, or test for it with defined(data.nope))",
        ),
        (
            "data.env",
            "resource \"r\": data.env is a string, not true or false; compare it with == or !=",
        ),
        (
            "data.env > 3",
            "resource \"r\": cannot compare a string and a number with >; convert one first, \
             with toString or toNumber",
        ),
        (
            "data.replicas / 0 == 1",
            "resource \"r\": data.replicas is divided by zero",
        ),
        (
            "upper(data.replicas) == 'X'",
            "resource \"r\": argument 1 of upper(...) must be text, and is a number",
        ),
        (
            "data.ports[5] == 1",
            "resource \"r\": data.ports[5] is not set",
        ),
        (
            "data.env =~ '['",
            "resource \"r\": \"[\" is not a valid pattern",
        ),
    ] {
        fs::write(dir.join("site.yaml"), document(&[("r", condition)])).unwrap();
        let err = kept(&dir, &["--data", "data.yaml"]).expect_err(condition);
        assert!(err.contains("FAILED: "), "{condition}: {err}");
        assert!(err.contains(want), "{condition}: {err}");
    }
}

#[test]
fn validate_checks_the_language_of_a_condition_and_needs_no_data() {
    let dir = scratch("validate");
    for condition in [
        "data.a > 1 && data.b in ['x'] || data.c =~ '^y'",
        "length(filter(data.items, 'i', i.on)) > 0",
        "data.a ?? data.b ?? false",
        "(data.a ? 1 : 2) == 2",
    ] {
        fs::write(dir.join("site.yaml"), document(&[("r", condition)])).unwrap();
        let output = meridian(&dir, &["validate", "site.yaml"]);
        assert!(output.status.success(), "{condition}: {}", stderr(&output));
        assert_eq!(stdout(&output), "1 document valid\n");
    }
    for (condition, want) in [
        ("data.a >", "the condition ends where a value was expected"),
        (
            "data.a = 1",
            "unexpected \"=\" (to compare two values in a condition, write ==)",
        ),
        ("facts.os.family == 'debian'", "facts are not available"),
        (
            "param.x",
            "unknown name \"param\" (an expression reads data.<key>, var.<name>, cond.<name>, secret.<name> or meridian.run.id; text is quoted)",
        ),
        ("nosuch(data.a)", "unknown function \"nosuch\""),
        (
            "Length(data.a) > 0",
            "unknown function \"Length\" (did you mean length?)",
        ),
        (
            "length() > 0",
            "length(...) takes 1 argument, and was given 0",
        ),
        (
            "map(data.a, x, x)",
            "the second argument of map(...) is the name each item is given",
        ),
        ("data.a in [1, 2", "expected \",\", but the condition ends"),
    ] {
        fs::write(dir.join("site.yaml"), document(&[("r", condition)])).unwrap();
        let output = meridian(&dir, &["validate", "site.yaml"]);
        assert_eq!(output.status.code(), Some(1), "{condition}");
        let err = stderr(&output);
        assert!(
            err.contains("site.yaml:11:7: spec.resources[1].when: ") && err.contains(want),
            "{condition}: {err}"
        );
    }
}

/// Compiles an example for its real target and returns the compiled text of
/// every file, so a test can ask whether a resource is in it.
fn compile_example(name: &str, target: &str, sets: &[&str]) -> Result<String, String> {
    let dir = scratch(&format!("example-{name}"));
    let example = repo_root().join("examples/conditions").join(name);
    let plugin = plugin_binary(&format!("meridian-target-{target}"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command.env("MERIDIAN_ANNOTATIONS", "off");
    command
        .arg("compile")
        .arg(example.with_extension("yaml"))
        .arg("--data")
        .arg(example.with_extension("data.yaml"))
        .arg("--out")
        .arg(dir.join("build"))
        .arg("--plugin-dir")
        .arg(plugin.parent().unwrap());
    for set in sets {
        command.arg("--set").arg(set);
    }
    let output = command.output().unwrap();
    if !output.status.success() {
        return Err(format!("{}{}", stdout(&output), stderr(&output)));
    }
    let mut text = String::new();
    let mut pending = vec![dir.join("build")];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                text.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    Ok(text)
}

/// Which of `names` appear in `text`.
fn present<'a>(text: &str, names: &[&'a str]) -> Vec<&'a str> {
    names
        .iter()
        .copied()
        .filter(|name| text.contains(name))
        .collect()
}

#[test]
fn the_host_example_keeps_what_its_data_says() {
    let all = [
        "app_pkg",
        "debug_log",
        "tls_dir",
        "swap_file",
        "eu_mirror",
        "backup_tool",
        "legacy_shim",
        "app_conf",
        "app_svc",
    ];
    let compiled = |sets: &[&str]| {
        let text = compile_example("host-by-environment", "ansible", sets).unwrap();
        present(&text, &all)
    };
    // As the example's own data file has it: production, in the EU, with
    // plenty of memory, TLS and one backup target.
    assert_eq!(
        compiled(&[]),
        [
            "app_pkg",
            "tls_dir",
            "eu_mirror",
            "backup_tool",
            "app_conf",
            "app_svc"
        ]
    );
    // Development: the debug log appears and the TLS directory goes, and
    // app_conf carries on without it because its entry says ifMissing: skip.
    assert_eq!(
        compiled(&["env=dev"]),
        [
            "app_pkg",
            "debug_log",
            "eu_mirror",
            "backup_tool",
            "app_conf",
            "app_svc"
        ]
    );
    // A small, old machine outside the EU with nothing to back up.
    assert_eq!(
        compiled(&[
            "memory_mb=2048",
            "region=us-east-1",
            "os_version=\"20.04\"",
            "backup.targets=[]",
            "tls.enabled=false",
        ]),
        ["app_pkg", "swap_file", "legacy_shim", "app_conf", "app_svc"]
    );
    // A version given as a number is not text, and is refused, not guessed.
    let err = compile_example("host-by-environment", "ansible", &["os_version=20.04"]).unwrap_err();
    assert!(
        err.contains("argument 1 of semverSatisfies(...) must be text, and is a number"),
        "{err}"
    );
}

#[test]
fn the_network_example_keeps_what_its_data_says() {
    let all = [
        "aws_vpc",
        "aws_internet_gateway",
        "aws_security_group",
        "aws_eip",
        "aws_cloudwatch_log_group",
    ];
    let compiled = |sets: &[&str]| {
        let text = compile_example("network-by-size", "terraform", sets).unwrap();
        present(&text, &all)
    };
    assert_eq!(compiled(&[]), all);
    assert_eq!(
        compiled(&["bastion_ip=192.168.1.1"]),
        [
            "aws_vpc",
            "aws_internet_gateway",
            "aws_eip",
            "aws_cloudwatch_log_group"
        ]
    );
    assert_eq!(
        compiled(&["account=sandbox", "env=dev"]),
        ["aws_vpc", "aws_internet_gateway", "aws_security_group"]
    );
    assert_eq!(
        compiled(&["subnets=[{name: private-a, public: false}]"]),
        ["aws_vpc", "aws_security_group", "aws_cloudwatch_log_group"]
    );
}

#[test]
fn the_cluster_example_keeps_what_its_data_says() {
    let all = [
        "shop-config",
        "metrics-config",
        "PodDisruptionBudget",
        "ResourceQuota",
    ];
    let compiled = |sets: &[&str]| {
        let text = compile_example("cluster-by-feature", "kubernetes", sets).unwrap();
        present(&text, &all)
    };
    assert_eq!(compiled(&[]), all);
    assert_eq!(
        compiled(&["features={}", "replicas=1"]),
        ["shop-config", "ResourceQuota"]
    );
    assert_eq!(
        compiled(&["labels.env=dev", "namespace=monitoring"]),
        ["shop-config", "metrics-config"]
    );
}
