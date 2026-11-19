//! Variables, named conditions, `when` on documents and edges, and
//! interpolation, through the real command: what is printed, what reaches a
//! plugin, and the examples under `examples/language/` with the values that
//! change them.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::{plugin_binary, repo_root};

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("interpolation-{test}"));
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

/// A document for the fake target, which writes each resource's `content`
/// to a file named after the resource: the files are what the plugin was
/// given.
const SITE: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
  labels:
    env: ${{ data.env }}
spec:
  target: fake
  targetHost:
    from: inline
    host: ${{ var.name }}.example.com
  variables:
    name: ${{ data.project }}-${{ data.env }}
    ports: "${{ cond.is_prod ? [80, 443] : [8080] }}"
  conditions:
    is_prod: data.env == 'prod'
    needs_tls: cond.is_prod || (data.force_tls ?? false)
  resources:
    - id: conf
      type: file
      params:
        content: "name=${{ var.name }} ports=${{ join(var.ports, ',') }} n=${{ length(var.ports) }} note=${{ data.note ?? 'none' }} literal=$${{ kept }}"
      dependsOn:
        - resource: cert
          when: cond.needs_tls
    - id: cert
      type: file
      when: cond.needs_tls
      params:
        content: cert for ${{ var.name }}
    - id: debug
      type: file
      when: "!cond.is_prod"
      params:
        content: debug
"#;

fn compile(dir: &Path, args: &[&str]) -> Output {
    let _ = fs::remove_dir_all(dir.join("build"));
    let mut all = vec!["compile", "site.yaml", "--out", "build"];
    all.extend_from_slice(args);
    meridian(dir, &all)
}

fn written(dir: &Path, id: &str) -> Option<String> {
    fs::read_to_string(dir.join("build/site/files").join(id)).ok()
}

#[test]
fn what_the_plugin_is_given_holds_the_values_and_no_expression() {
    let dir = scratch("placed");
    fs::write(dir.join("site.yaml"), SITE).unwrap();

    let output = compile(&dir, &["--set", "env=prod", "--set", "project=shop"]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        written(&dir, "conf").unwrap(),
        // The escaped opening is handed over as written: turning it into
        // the literal is the plugin's own rule for an escaped reference,
        // which the fake target does not apply.
        "name=shop-prod ports=80,443 n=2 note=none literal=$${{ kept }}"
    );
    assert_eq!(written(&dir, "cert").unwrap(), "cert for shop-prod");
    assert_eq!(written(&dir, "debug"), None);
    let out = stdout(&output);
    assert!(
        out.starts_with("site (ResourceSet, target fake on shop-prod.example.com)\n"),
        "{out}"
    );
    assert!(
        out.contains("  when: 1 removed\n    resource \"debug\": !cond.is_prod is false\n"),
        "{out}"
    );

    let output = compile(&dir, &["--set", "env=dev", "--set", "project=shop"]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        written(&dir, "conf").unwrap(),
        "name=shop-dev ports=8080 n=1 note=none literal=$${{ kept }}"
    );
    assert_eq!(written(&dir, "cert"), None);
    assert_eq!(written(&dir, "debug").unwrap(), "debug");
    let out = stdout(&output);
    assert!(
        out.contains(
            "  when: 2 removed\n    resource \"cert\": cond.needs_tls is false\n    \
             \"conf\" depends on \"cert\": cond.needs_tls is false\n"
        ),
        "{out}"
    );

    // A named condition reads another, and data that may not be set.
    let output = compile(
        &dir,
        &[
            "--set",
            "env=dev",
            "--set",
            "project=shop",
            "--set",
            "force_tls=true",
        ],
    );
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(written(&dir, "cert").unwrap(), "cert for shop-dev");
}

#[test]
fn a_value_from_data_is_literal_text_to_the_plugin_whatever_it_looks_like() {
    let dir = scratch("literal");
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    // The note looks like a reference to another resource's param. Had it
    // been written in the document it would be one; from data it is handed
    // to the plugin escaped, which every plugin reads as literal text.
    let output = compile(
        &dir,
        &[
            "--set",
            "env=prod",
            "--set",
            "project=shop",
            "--set",
            "note=${cert.content}",
        ],
    );
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let conf = written(&dir, "conf").unwrap();
    assert!(conf.contains("note=$${cert.content} "), "{conf}");
}

#[test]
fn a_label_selector_matches_the_interpolated_label() {
    let dir = scratch("labels");
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    let sets = ["--set", "env=prod", "--set", "project=shop"];
    let output = compile(&dir, &[&sets[..], &["-l", "env=prod"]].concat());
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let output = compile(&dir, &[&sets[..], &["-l", "env=dev"]].concat());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no document's labels match the selector env=dev"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_value_that_is_not_given_fails_the_stage_and_names_the_place() {
    let dir = scratch("unset");
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    let output = compile(&dir, &["--set", "env=prod"]);
    assert_eq!(output.status.code(), Some(1));
    let out = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        out.contains("FAILED: ")
            && out.contains("spec.variables.name")
            && out.contains("in ${{ data.project }}")
            && out.contains("data.project is not set (give it with --data or --set"),
        "{out}"
    );
    assert!(!dir.join("build/site").exists(), "nothing is written");
}

const TWO: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: site}
spec:
  target: fake
  targetHost: {from: inline, host: h}
  resources:
    - {id: a, type: file, params: {content: x}}
---
apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: extras}
spec:
  target: fake
  when: data.extras ?? false
  targetHost: {from: inline, host: h}
  resources:
    - {id: b, type: file, params: {content: "${{ data.only_with_extras }}"}}
"#;

#[test]
fn a_document_its_when_leaves_out_is_reported_as_skipped_and_is_not_a_failure() {
    let dir = scratch("skipped");
    fs::write(dir.join("site.yaml"), TWO).unwrap();
    let output = compile(&dir, &[]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let out = stdout(&output);
    assert!(
        out.contains(
            "extras (ResourceSet, target fake)\n  skipped: its when (data.extras ?? false) is false\n"
        ),
        "{out}"
    );
    assert!(dir.join("build/site/files/a").exists());
    assert!(!dir.join("build/extras").exists());

    let output = compile(
        &dir,
        &["--set", "extras=true", "--set", "only_with_extras=yes"],
    );
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(dir.join("build/extras/files/b")).unwrap(),
        "yes"
    );
}

#[test]
fn validate_checks_every_expression_and_needs_no_data() {
    let dir = scratch("validate");
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    let output = meridian(&dir, &["validate", "site.yaml"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "1 document valid\n");

    for (from, to, want) in [
        (
            "name=${{ var.name }}",
            "name=${{ var.nmae }}",
            "spec.resources[0].params.content: var.nmae is not declared (spec.variables \
             declares: name, ports)",
        ),
        (
            "when: cond.needs_tls\n      params",
            "when: cond.tls\n      params",
            "spec.resources[1].when: cond.tls is not declared (spec.conditions declares: \
             is_prod, needs_tls)",
        ),
        (
            "is_prod: data.env == 'prod'",
            "is_prod: cond.needs_tls && data.env == 'prod'",
            "these read each other in a circle, so none can be worked out: cond.is_prod -> \
             cond.needs_tls -> cond.is_prod",
        ),
        (
            "name: ${{ data.project }}-${{ data.env }}",
            "name: ${{ data.project }}-${{ data.env",
            "spec.variables.name: \"${{ data.project }}-${{ data.env\" opens an interpolation \
             that is never closed with }}",
        ),
        (
            "- id: cert\n",
            "- id: cert-${{ data.env }}\n",
            "spec.resources[1].id: cannot be interpolated",
        ),
        (
            "host: ${{ var.name }}.example.com",
            "host: ${{ endpoint.host }}",
            "spec.targetHost.host: in ${{ endpoint.host }}: unknown name \"endpoint\"",
        ),
    ] {
        assert!(SITE.contains(from), "{from}");
        fs::write(dir.join("site.yaml"), SITE.replace(from, to)).unwrap();
        let output = meridian(&dir, &["validate", "site.yaml"]);
        assert_eq!(output.status.code(), Some(1), "{to}");
        let err = stderr(&output);
        assert!(err.contains(want), "{to}: {err}");
    }
}

#[test]
fn an_unquoted_interpolation_that_trips_yaml_says_to_quote_it() {
    let dir = scratch("yaml");
    fs::write(
        dir.join("site.yaml"),
        SITE.replace(
            "ports: \"${{ cond.is_prod ? [80, 443] : [8080] }}\"",
            "ports: ${{ cond.is_prod ? [80, 443] : [8080] }}",
        ),
    )
    .unwrap();
    let output = meridian(&dir, &["validate", "site.yaml"]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("site.yaml:14:") && err.contains("put the whole value in double quotes"),
        "{err}"
    );
}

/// Compiles an example for its real target and returns what was printed and
/// the compiled text of every file.
fn compile_example(name: &str, target: &str, extra: &[&str]) -> Result<(String, String), String> {
    let dir = scratch(&format!("example-{name}"));
    let example = repo_root().join("examples/language").join(name);
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
        .arg(plugin.parent().unwrap())
        .args(extra);
    let output = command.output().unwrap();
    if !output.status.success() {
        return Err(format!("{}{}", stdout(&output), stderr(&output)));
    }
    let mut text = String::new();
    let mut pending = vec![dir.join("build")];
    while let Some(next) = pending.pop() {
        let Ok(entries) = fs::read_dir(next) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                text.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    Ok((stdout(&output), text))
}

#[test]
fn the_site_example_follows_its_environment() {
    let (out, prod) = compile_example("site-by-environment", "ansible", &[]).unwrap();
    assert!(out.contains("on shop-prod.example.com"), "{out}");
    for want in [
        "/srv/shop-prod",
        "/etc/nginx/conf.d/shop-prod.conf",
        "owned by WEB",
        "worker_processes 8;",
        "listen 80 443;",
        "/etc/ssl/shop-prod.pem",
    ] {
        assert!(prod.contains(want), "{want}:\n{prod}");
    }
    assert!(!prod.contains("debug.html"), "{prod}");
    assert!(!prod.contains("${{"), "{prod}");

    let (out, dev) =
        compile_example("site-by-environment", "ansible", &["--set", "env=dev"]).unwrap();
    assert!(
        out.contains("resource \"site_cert\": cond.needs_tls is false"),
        "{out}"
    );
    for want in [
        "/srv/shop-dev",
        "worker_processes 1;",
        "listen 8080;",
        "/srv/shop-dev/debug.html",
    ] {
        assert!(dev.contains(want), "{want}:\n{dev}");
    }
    assert!(!dev.contains(".pem"), "{dev}");

    // A value from data that looks like a reference is literal text in what
    // Ansible is given: it is not resolved against the document's resources.
    let (_, shaped) = compile_example(
        "site-by-environment",
        "ansible",
        &["--set", "team=${web_pkg.name}"],
    )
    .unwrap();
    assert!(shaped.contains("owned by ${WEB_PKG.NAME}"), "{shaped}");

    // The label is interpolated, and the selector sees the value.
    assert!(compile_example("site-by-environment", "ansible", &["-l", "env=prod"]).is_ok());
    let err = compile_example("site-by-environment", "ansible", &["-l", "env=dev"]).unwrap_err();
    assert!(err.contains("no document's labels match"), "{err}");
}

#[test]
fn the_network_example_places_values_beside_references() {
    let (_, prod) = compile_example("network-by-environment", "terraform", &[]).unwrap();
    let tf: serde_json::Value = serde_json::from_str(&prod).unwrap();
    let vpc = &tf["resource"]["aws_vpc"]["main"];
    assert_eq!(vpc["cidr_block"], "10.20.0.0/16");
    assert_eq!(vpc["enable_dns_hostnames"], true);
    assert_eq!(vpc["tags"]["Owner"], "PLATFORM");
    assert_eq!(vpc["tags"]["Environment"], "prod");
    let subnet = &tf["resource"]["aws_subnet"]["public"];
    assert_eq!(subnet["cidr_block"], "10.20.0.0/24");
    assert_eq!(
        subnet["vpc_id"], "${aws_vpc.main.id}",
        "a reference is still one"
    );
    assert_eq!(subnet["tags"]["Name"], "shop-prod-public");
    let logs = &tf["resource"]["aws_cloudwatch_log_group"]["flow_logs"];
    assert_eq!(logs["retention_in_days"], 90, "a number stays a number");
    assert_eq!(logs["name"], "/vpc/shop-prod/flow-logs");
    assert_eq!(tf["provider"]["aws"]["region"], "eu-west-1");
    assert!(tf["output"]["public_subnet_id"].is_object());

    let (out, private) = compile_example(
        "network-by-environment",
        "terraform",
        &["--set", "public_ports=[]", "--set", "env=dev"],
    )
    .unwrap();
    let tf: serde_json::Value = serde_json::from_str(&private).unwrap();
    assert!(tf["resource"]["aws_subnet"].is_null(), "{private}");
    assert!(
        tf["resource"]["aws_internet_gateway"].is_null(),
        "{private}"
    );
    assert!(tf["output"]["public_subnet_id"].is_null(), "{private}");
    assert!(tf["output"]["vpc_id"].is_object());
    assert_eq!(
        tf["resource"]["aws_cloudwatch_log_group"]["flow_logs"]["retention_in_days"],
        7
    );
    assert_eq!(
        tf["resource"]["aws_vpc"]["main"]["enable_dns_hostnames"],
        false
    );
    assert!(
        out.contains("output \"public_subnet_id\": cond.public is false"),
        "{out}"
    );
}

#[test]
fn the_monitoring_example_is_one_document_or_two() {
    let (out, both) = compile_example("optional-monitoring", "kubernetes", &[]).unwrap();
    assert!(!out.contains("skipped"), "{out}");
    for want in [
        "SCRAPE_INTERVAL: 15s",
        "TARGET_NAMESPACE: shop-prod",
        "LOG_LEVEL: info",
    ] {
        assert!(both.contains(want), "{want}:\n{both}");
    }

    let (out, one) = compile_example(
        "optional-monitoring",
        "kubernetes",
        &["--set", "monitoring.enabled=false", "--set", "env=dev"],
    )
    .unwrap();
    assert!(
        out.contains("monitoring (Infrastructure, target kubernetes)\n  skipped: its when (data.monitoring.enabled ?? false) is false"),
        "{out}"
    );
    assert!(!one.contains("SCRAPE_INTERVAL"), "{one}");
    assert!(one.contains("LOG_LEVEL: debug"), "{one}");
}

const TAGGED: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata: {name: site}
spec:
  target: fake
  targetHost: {from: inline, host: h}
  resources:
    - {id: tag, type: file, params: {content: "web-${{ meridian.run.id }}"}}
"#;

#[test]
fn the_run_id_is_read_as_meridian_run_id_and_never_reaches_the_plugin_as_data() {
    let dir = scratch("run-id");
    fs::write(dir.join("site.yaml"), TAGGED).unwrap();
    let output = compile(&dir, &["--run-id", "lab-7"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(written(&dir, "tag").as_deref(), Some("web-lab-7"));
    // The fake plugin writes the data it is handed to data.json, and only
    // when it is handed some.
    assert!(!dir.join("build/site/data.json").exists());

    let output = compile(&dir, &[]);
    assert_eq!(output.status.code(), Some(1));
    let out = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        out.contains("meridian.run is not set (the run id is given with --run-id"),
        "{out}"
    );
}

#[test]
fn the_run_id_and_its_key_are_checked() {
    let dir = scratch("run-id-refused");
    fs::write(dir.join("site.yaml"), TAGGED).unwrap();
    fs::write(dir.join("own.yaml"), "meridian: {run: {id: x}}\n").unwrap();
    for (args, said) in [
        (
            vec!["--run-id", "Lab_7"],
            "--run-id: the run id \"Lab_7\" must be 1 to 32 lowercase letters",
        ),
        (
            vec!["--set", "meridian.run.id=x"],
            "--set meridian.run.id=x: data.meridian is Meridian's own; give the run id with --run-id",
        ),
        (
            vec!["--data", "own.yaml"],
            "a --data file sets meridian, which is Meridian's own key",
        ),
    ] {
        let output = compile(&dir, &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(stderr(&output).contains(said), "{args:?}: {}", stderr(&output));
    }
}

#[test]
fn a_saved_plan_keeps_the_run_id() {
    let dir = scratch("run-id-plan");
    fs::write(dir.join("site.yaml"), TAGGED).unwrap();
    let plan = meridian(
        &dir,
        &[
            "plan",
            "site.yaml",
            "--run-id",
            "lab-7",
            "--out",
            "site.mplan",
        ],
    );
    assert!(plan.status.success(), "{}", stderr(&plan));
    let apply = meridian(&dir, &["apply", "--plan", "site.mplan"]);
    assert!(apply.status.success(), "{}", stderr(&apply));
    let tag = dir.join(".meridian/resourceset/site/fake/current/files/tag");
    assert_eq!(fs::read_to_string(tag).unwrap(), "web-lab-7");
}
