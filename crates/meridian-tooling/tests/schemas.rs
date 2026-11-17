//! The schemas a tool itself described, read from the cache that
//! `meridian schema sync` writes: native types are completed, documented
//! and checked, at every level, from what the cache says.

mod common;

use std::path::PathBuf;

use common::{cursor, marked};
use meridian_sdk::{ParamKind, ParamSchema, ResourceSchema, SchemaExport};
use meridian_tooling::analysis::{complete, hover, Completion, CompletionKind};
use meridian_tooling::catalog::Catalog;
use meridian_tooling::check::{self, Env};
use meridian_tooling::schemas::{Cached, Freshness, Store};

/// A document on terraform, whose plugin knows `<provider>_<type>` only.
const HEAD: &str = "\
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: net
spec:
  target: terraform
  resources:
";

/// What a sync of terraform might have cached for `aws_instance`.
fn cached_terraform() -> Cached {
    let mut export = SchemaExport::new("terraform", "1.13.1");
    export.pin("registry.terraform.io/hashicorp/aws", "5.60.0");
    export.schemas.push(
        ResourceSchema::closed("aws_instance", "Provides an EC2 instance resource.")
            .param(
                ParamSchema::new("ami", ParamKind::String, "AMI to use for the instance.")
                    .required(),
            )
            .param(
                ParamSchema::new("instance_type", ParamKind::String, "Instance type to use.")
                    .required(),
            )
            .param(
                ParamSchema::new(
                    "tags",
                    ParamKind::Map,
                    "Map of tags to assign to the resource.",
                )
                .open(),
            )
            .param(
                ParamSchema::new("root_block_device", ParamKind::List, "Configuration block.")
                    .with_params(vec![
                        ParamSchema::new(
                            "volume_size",
                            ParamKind::Integer,
                            "Size of the volume in gibibytes.",
                        ),
                        ParamSchema::new("volume_type", ParamKind::String, "Type of volume.")
                            .values(&["gp2", "gp3", "io1"]),
                    ]),
            )
            .param(
                ParamSchema::new("metadata_options", ParamKind::Map, "Metadata options.")
                    .with_params(vec![ParamSchema::new(
                        "http_tokens",
                        ParamKind::String,
                        "Whether session tokens are required.",
                    )
                    .values(&["optional", "required"])]),
            )
            .param(ParamSchema::new("arn", ParamKind::String, "ARN of the instance.").computed())
            .param(
                ParamSchema::new("public_ip", ParamKind::String, "Public IP address.").computed(),
            )
            .from_source("registry.terraform.io/hashicorp/aws 5.60.0"),
    );
    Cached::from_export("terraform", export)
}

fn catalog() -> Catalog {
    let mut catalog = Catalog::from_registry(&common::registry());
    catalog
        .target_mut("terraform")
        .unwrap()
        .add_cached(&cached_terraform(), Freshness::Unrecorded);
    catalog
}

fn completions(marked: &str) -> Vec<Completion> {
    let (text, pos) = cursor(marked);
    complete(
        &common::workspace(&[("doc.yaml", &text)]),
        &catalog(),
        "doc.yaml",
        pos,
    )
}

fn labels(marked: &str) -> Vec<String> {
    completions(marked).into_iter().map(|c| c.label).collect()
}

fn hover_text(marked: &str) -> String {
    let (text, pos) = cursor(marked);
    hover(
        &common::workspace(&[("doc.yaml", &text)]),
        &catalog(),
        "doc.yaml",
        pos,
    )
    .map(|h| h.markdown)
    .unwrap_or_default()
}

fn diagnostics(text: &str) -> Vec<check::Diagnostic> {
    let registry = common::registry();
    let catalog = catalog();
    let ws = common::workspace(&[("doc.yaml", text)]);
    check::check(
        &ws,
        &Env {
            registry: Some(&registry),
            catalog: &catalog,
            data: None,
        },
    )
}

#[test]
fn a_cached_type_is_offered_by_name_with_its_params_at_every_level() {
    // The exact type is offered beside the plugin's own patterns.
    let types = labels(&format!("{HEAD}    - id: web\n      type: |"));
    assert!(types.contains(&"aws_instance".to_owned()), "{types:?}");
    assert!(types.contains(&"<provider>_<type>".to_owned()), "{types:?}");

    // Its params, required first, the computed ones left out.
    let found = completions(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        |"
    ));
    let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        names,
        [
            "ami",
            "instance_type",
            "tags",
            "root_block_device",
            "metadata_options"
        ]
    );
    assert_eq!(found[0].detail, "required · string");
    assert!(found[0].doc.contains("AMI to use"), "{}", found[0].doc);
    assert_eq!(found[3].insert, "root_block_device:\n  - volume_size: $0");
    assert_eq!(found[4].insert, "metadata_options:\n  $0");

    // Inside a nested mapping: its own keys.
    let nested = labels(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        ami: a\n        \
         metadata_options:\n          |"
    ));
    assert_eq!(nested, ["http_tokens"]);
    // Inside a list item: the item's keys, without the one already set.
    let item = labels(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        \
         root_block_device:\n          - volume_size: 20\n            |"
    ));
    assert_eq!(item, ["volume_type"]);
    // A nested param's fixed values.
    let values = labels(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        \
         root_block_device:\n          - volume_type: |"
    ));
    assert_eq!(values, ["gp2", "gp3", "io1"]);
    // An open mapping offers nothing, and refuses nothing.
    let tags = labels(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        tags:\n          |"
    ));
    assert!(tags.is_empty(), "{tags:?}");
}

#[test]
fn a_cached_type_and_its_nested_params_are_documented() {
    let on_type = hover_text(&format!("{HEAD}    - id: web\n      type: aws_in|stance"));
    assert!(on_type.contains("Provides an EC2 instance"), "{on_type}");
    assert!(
        on_type.contains("Described by registry.terraform.io/hashicorp/aws 5.60.0"),
        "{on_type}"
    );
    assert!(
        on_type.contains("| `ami` | string | **required.**"),
        "{on_type}"
    );
    assert!(
        on_type.contains("| `arn` | string | set by the tool."),
        "{on_type}"
    );

    let nested = hover_text(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        \
         metadata_options:\n          http_to|kens: required"
    ));
    assert!(
        nested.starts_with("**`metadata_options.http_tokens`** · optional · string"),
        "{nested}"
    );
    assert!(
        nested.contains("Accepts: `optional`, `required`"),
        "{nested}"
    );

    let block = hover_text(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        root_block_de|vice:\n          - volume_size: 1"
    ));
    assert!(
        block.contains("Each item holds: `volume_size`, `volume_type`"),
        "{block}"
    );

    let target = hover_text(&HEAD.replace("target: terraform", "target: terra|form"));
    assert!(
        target.contains("Tool schemas: 1 types described by terraform 1.13.1"),
        "{target}"
    );
}

#[test]
fn what_the_tool_sets_is_offered_inside_a_reference() {
    let found = completions(&format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        ami: a\n    - id: \
         dns\n      type: aws_route53_record\n      params:\n        records: ${{web.|"
    ));
    let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
    // What is written first, then what the tool sets, then the rest.
    assert_eq!(names[0], "ami");
    assert_eq!(&names[1..3], ["arn", "public_ip"]);
    assert!(names.contains(&"instance_type"));
    assert!(
        found[1].detail.starts_with("set by the tool"),
        "{}",
        found[1].detail
    );
}

#[test]
fn a_cached_type_is_checked_at_every_level() {
    let text = format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        ami: ami-1\n        \
         instance_typo: t3.micro\n        arn: x\n        tags:\n          Name: web\n        \
         metadata_options:\n          http_tokens: maybe\n          other: 1\n        \
         root_block_device:\n          - volume_size: 20\n            volume_colour: red\n          \
         - volume_type: gp3\n        subnet_id: ${{net.id}}\n"
    );
    let found = diagnostics(&text);
    let codes: Vec<(String, String)> = found
        .iter()
        .map(|d| (d.code.clone(), marked(&text, d).to_owned()))
        .collect();
    assert!(
        codes.contains(&("unsupported-param".into(), "instance_typo".into())),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("missing-param".into(), "params".into())),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("computed-param".into(), "arn".into())),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("unsupported-value".into(), "http_tokens".into())),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("unsupported-param".into(), "other".into())),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("unsupported-param".into(), "volume_colour".into())),
        "{codes:?}"
    );
    assert!(
        codes.contains(&("unsupported-param".into(), "subnet_id".into())),
        "{codes:?}"
    );
    // Open mappings and references are not refused.
    assert!(
        !codes
            .iter()
            .any(|(_, at)| at == "Name" || at == "volume_type"),
        "{codes:?}"
    );
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("\"other\" in \"metadata_options\"")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("\"volume_colour\" in \"root_block_device[0]\"")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("read it with `${web.arn}`")),
        "{messages:?}"
    );

    // The wrong shape is a different error from the wrong name.
    let shape = format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        ami: a\n        \
         instance_type: t\n        tags: web\n        root_block_device:\n          size: 1\n"
    );
    let found = diagnostics(&shape);
    let codes: Vec<(String, String)> = found
        .iter()
        .map(|d| (d.code.clone(), marked(&shape, d).to_owned()))
        .collect();
    assert_eq!(
        codes,
        [
            ("wrong-param-kind".to_owned(), "tags".to_owned()),
            (
                "wrong-param-kind".to_owned(),
                "root_block_device".to_owned()
            )
        ]
    );

    // A document that uses the type as the tool takes it is clean.
    let good = format!(
        "{HEAD}    - id: web\n      type: aws_instance\n      params:\n        ami: a\n        \
         instance_type: t\n        tags:\n          Name: web\n        root_block_device:\n          \
         - volume_size: 20\n            volume_type: gp3\n"
    );
    assert!(diagnostics(&good).is_empty(), "{:?}", diagnostics(&good));
}

#[test]
fn a_type_the_cache_does_not_describe_is_still_open() {
    let text = format!(
        "{HEAD}    - id: b\n      type: aws_s3_bucket\n      params:\n        anything: goes\n"
    );
    assert!(diagnostics(&text).is_empty());
}

#[test]
fn the_cache_is_read_from_the_state_directory_and_judged_against_the_lock() {
    let dir = std::env::temp_dir().join(format!("meridian-tooling-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = dir.join(".meridian");
    let store = Store::new(&state);
    let cached = cached_terraform();
    let hash = store.save(&cached).unwrap();

    // Unrecorded until the lock says so.
    let mut catalog = Catalog::from_registry(&common::registry());
    assert!(catalog.read_cache(&store, None).is_empty());
    let t = catalog.target("terraform").unwrap();
    assert_eq!(t.cache.as_ref().unwrap().freshness, Freshness::Unrecorded);
    assert!(t.resource("aws_instance").is_some_and(|s| !s.open));

    // Recorded, then differing once the file changes.
    let mut lock = meridian_module::Lock::default();
    lock.set_schemas(cached.lock_entry(&hash));
    std::fs::write(dir.join("meridian.lock"), lock.render()).unwrap();
    let lock = meridian_tooling::schemas::lock_in(&dir).unwrap();
    let mut catalog = Catalog::from_registry(&common::registry());
    catalog.read_cache(&store, Some(&lock));
    assert_eq!(
        catalog
            .target("terraform")
            .unwrap()
            .cache
            .as_ref()
            .unwrap()
            .freshness,
        Freshness::Current
    );
    assert!(catalog.missing_caches(&lock).is_empty());
    let mut other = cached.clone();
    other.tool_version = "1.14.0".to_owned();
    store.save(&other).unwrap();
    let mut catalog = Catalog::from_registry(&common::registry());
    catalog.read_cache(&store, Some(&lock));
    assert_eq!(
        catalog
            .target("terraform")
            .unwrap()
            .cache
            .as_ref()
            .unwrap()
            .freshness,
        Freshness::Differs
    );
    // Gone from disk: missing.
    store.remove("terraform").unwrap();
    let mut catalog = Catalog::from_registry(&common::registry());
    catalog.read_cache(&store, Some(&lock));
    assert_eq!(catalog.missing_caches(&lock), ["terraform"]);
    // A cache for a target with no plugin is ignored, a broken one reported.
    std::fs::write(store.path("nothing"), "{}").unwrap();
    std::fs::write(store.path("broken"), "{").unwrap();
    let mut catalog = Catalog::from_registry(&common::registry());
    let problems = catalog.read_cache(&store, None);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("broken.json"), "{problems:?}");
    let _ = std::fs::remove_dir_all(&dir);
    let _: PathBuf = dir;
}

#[test]
fn functions_and_names_are_offered_in_an_expression() {
    // Inside `${{ }}` in any value: the names, then every function.
    let doc = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  variables:
    size: 3
  conditions:
    is_prod: data.env == 'prod'
  secrets:
    - name: token
      from: env
      key: TOKEN
  resources:
    - id: motd
      type: file
      params:
        path: /etc/motd
        content: ${{ up|";
    let found = completions(doc);
    let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        &names[..4],
        ["data.", "var.size", "cond.is_prod", "secret.token"]
    );
    assert!(names.contains(&"upper()"), "{names:?}");
    assert!(names.contains(&"cidrHost()"), "{names:?}");
    let upper = found.iter().find(|c| c.label == "upper()").unwrap();
    assert_eq!(upper.kind, CompletionKind::Function);
    assert_eq!(upper.insert, "upper($1)");
    assert_eq!(upper.detail, "(text)");
    assert!(upper.doc.contains("the text in one case"), "{}", upper.doc);
    // The word typed so far is what gets replaced.
    assert_eq!(upper.span.start.col, doc.lines().last().unwrap().len() - 3);
    assert_eq!(upper.span.end.col, doc.lines().last().unwrap().len() - 1);

    // In a `when` too, beside what a condition can read.
    let when = labels(&doc.replace("        content: ${{ up|", "      when: |"));
    assert!(
        when.contains(&"defined()".to_owned()) && when.contains(&"semverSatisfies()".to_owned()),
        "{when:?}"
    );

    // Hover on a function, in a condition and in an interpolation.
    let on_when = hover_text(&doc.replace(
        "        content: ${{ up|",
        "      when: len|gth(var.size) > 1",
    ));
    assert!(
        on_when.starts_with("**`length(x)`** · Lists and mappings"),
        "{on_when}"
    );
    let on_expr = hover_text(&doc.replace("${{ up|", "${{ upp|er(data.x) }}"));
    assert!(
        on_expr.contains("Example: `upper('web')` is `'WEB'`"),
        "{on_expr}"
    );
    // Not on a word that is no function, nor outside an expression.
    assert!(hover_text(&doc.replace("${{ up|", "${{ nothing|(1) }}")).is_empty());
    assert!(
        !hover_text(&doc.replace("        content: ${{ up|", "        content: up|per(x)"))
            .contains("Text")
    );
    // Literal text is not an expression.
    assert!(labels(&doc.replace("${{ up|", "$${{ up|")).is_empty());
}
