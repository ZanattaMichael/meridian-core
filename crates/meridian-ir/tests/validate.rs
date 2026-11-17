mod common;

use common::*;
use meridian_ir::{parse, validate, Bundle};

const HEAD: &str =
    "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n";

#[test]
fn accepts_good_documents() {
    for doc in [VALID_INFRASTRUCTURE, VALID_RESOURCE_SET] {
        validate(&parse_one(doc)).unwrap();
    }
}

#[test]
fn rejects_a_duplicate_resource_id() {
    let doc = format!("{HEAD}  resources:\n    - id: nginx\n      type: package\n    - id: nginx\n      type: service\n");
    let err = validate(&parse_one(&doc)).unwrap_err().to_string();
    assert!(err.contains(r#"duplicate resource id "nginx""#), "{err}");
    // The message points at the first declaration too, so the author can find
    // both halves of the collision.
    assert!(err.contains("first declared at doc.yaml:7:7"), "{err}");
}

#[test]
fn rejects_dangling_edges() {
    let doc = format!(
        "{HEAD}  resources:\n    - id: nginx_conf\n      type: file\n      dependsOn:\n        - resource: absent_pkg\n      notifies:\n        - resource: absent_svc\n          action: restart\n"
    );
    let err = validate(&parse_one(&doc)).unwrap_err().to_string();
    for want in [
        r#"depends on unknown resource "absent_pkg""#,
        r#"notifies unknown resource "absent_svc""#,
    ] {
        assert!(
            err.contains(want),
            "error = {err}, want it to contain {want:?}"
        );
    }
}

#[test]
fn rejects_self_reference() {
    let doc = format!("{HEAD}  resources:\n    - id: nginx\n      type: service\n      dependsOn:\n        - resource: nginx\n");
    let err = validate(&parse_one(&doc)).unwrap_err().to_string();
    assert!(err.contains("cannot depend on itself"), "{err}");
}

#[test]
fn rejects_a_document_name_that_is_not_one_directory_name() {
    // Issue 133: the name becomes a directory under `--out` and under the
    // state directory, so it must be one plain path segment.
    let v = "apiVersion: meridian/v1\nkind: ResourceSet\n";
    let doc = |name: &str| {
        format!(
            "{v}metadata:\n  name: {name:?}\nspec:\n  target: ansible\n  resources:\n    - \
             {{id: a, type: package}}\n"
        )
    };
    for bad in [
        "../../esc",
        "a/b",
        "a\\b",
        "a b;c",
        ".hidden",
        "..",
        "-flag",
        "caf\u{e9}",
        "a\nb",
        &"x".repeat(129),
    ] {
        let err = validate(&parse_one(&doc(bad))).unwrap_err().to_string();
        assert!(
            err.contains("metadata.name") && err.contains("cannot name a document"),
            "{bad:?}: {err}"
        );
    }
    for good in ["web", "web-vm_1.2", "0", "A.B", &"x".repeat(128)] {
        validate(&parse_one(&doc(good))).unwrap_or_else(|e| panic!("{good:?}: {e}"));
    }
}

#[test]
fn rejects_missing_required_fields() {
    let v = "apiVersion: meridian/v1\nkind: ResourceSet\n";
    for (name, doc, want) in [
        (
            "no metadata.name",
            format!("{v}spec:\n  target: ansible\n  resources:\n    - {{id: a, type: package}}\n"),
            "metadata.name: missing required field",
        ),
        (
            "no spec.target",
            format!("{v}metadata: {{name: a}}\nspec:\n  resources:\n    - {{id: a, type: package}}\n"),
            "spec.target: missing required field",
        ),
        ("no resources", format!("{HEAD}  resources: []\n"), "at least one resource is required"),
        (
            "resource without id",
            format!("{HEAD}  resources:\n    - {{type: package}}\n"),
            "spec.resources[0].id: missing required field",
        ),
        (
            "resource without type",
            format!("{HEAD}  resources:\n    - {{id: a}}\n"),
            "spec.resources[0].type: missing required field",
        ),
        (
            "unknown ifMissing policy",
            format!(
                "{HEAD}  resources:\n    - {{id: a, type: package}}\n    - {{id: b, type: service, dependsOn: [{{resource: a, ifMissing: shrug}}]}}\n"
            ),
            r#"unknown policy "shrug" (expected "error" or "skip")"#,
        ),
        (
            "notify without action",
            format!(
                "{HEAD}  resources:\n    - {{id: a, type: package}}\n    - {{id: b, type: service, notifies: [{{resource: a}}]}}\n"
            ),
            "notifies[0].action: missing required field",
        ),
    ] {
        let err = validate(&parse_one(&doc)).expect_err(name).to_string();
        assert!(err.contains(want), "{name}: error = {err}, want it to contain {want:?}");
    }
}

#[test]
fn reports_every_problem_at_once() {
    let doc = format!(
        "{HEAD}  resources:\n    - id: a\n      type: package\n    - id: a\n      type: service\n      dependsOn:\n        - resource: ghost\n"
    );
    let err = validate(&parse_one(&doc)).unwrap_err();
    assert_eq!(err.len(), 2, "{err}");
    assert!(
        err.to_string().starts_with("2 errors:\n  doc.yaml:9:7: "),
        "{err}"
    );
}

#[test]
fn a_bundle_resolves_target_host_across_documents() {
    let bundle = Bundle::new(parse("bundle.yaml", bundle_text().as_bytes()).unwrap());
    bundle.validate().unwrap();
    let rs = bundle
        .resource_set("web-server-baseline")
        .expect("the ResourceSet");
    let (infra, out) = bundle.resolve_target_host(rs).unwrap();
    assert_eq!(infra.metadata.name, "web-vm");
    assert_eq!(
        (out.name.as_str(), out.value.as_str()),
        ("web_vm_ip", "${web_vm.public_ip}")
    );
}

#[test]
fn a_bundle_rejects_an_unresolvable_target_host() {
    for (name, from, to, want) in [
        (
            "unknown ref",
            "ref: web-vm",
            "ref: nope",
            "no Infrastructure document named",
        ),
        (
            "unknown output",
            "output: web_vm_ip",
            "output: nope",
            "declares no output",
        ),
    ] {
        let text = format!(
            "{VALID_INFRASTRUCTURE}---\n{}",
            VALID_RESOURCE_SET.replacen(from, to, 1)
        );
        let bundle = Bundle::new(parse("bundle.yaml", text.as_bytes()).unwrap());
        let err = bundle.validate().expect_err(name).to_string();
        assert!(
            err.contains(want),
            "{name}: error = {err}, want it to contain {want:?}"
        );
    }
}

#[test]
fn a_bundle_validates_target_host_from_inline() {
    let doc = format!(
        "{HEAD}  targetHost:\n    from: inline\n  resources:\n    - {{id: a, type: package}}\n"
    );
    let err = Bundle::new([parse_one(&doc)])
        .validate()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("targetHost.host: missing required field"),
        "{err}"
    );
}

#[test]
fn a_bundle_rejects_duplicate_document_names() {
    let text = format!("{VALID_RESOURCE_SET}---\n{VALID_RESOURCE_SET}");
    let bundle = Bundle::new(parse("bundle.yaml", text.as_bytes()).unwrap());
    let err = bundle.validate().unwrap_err().to_string();
    assert!(err.contains("duplicate ResourceSet document name"), "{err}");
    assert!(err.contains("first declared at bundle.yaml:1:1"), "{err}");
}

/// A document whose resources are `(id, depends on ...)`.
fn chain(resources: &[(&str, &[&str])]) -> String {
    let mut doc = format!("{HEAD}  resources:\n");
    for (id, deps) in resources {
        doc.push_str(&format!("    - id: {id}\n      type: package\n"));
        if !deps.is_empty() {
            doc.push_str("      dependsOn:\n");
            for dep in *deps {
                doc.push_str(&format!("        - resource: {dep}\n"));
            }
        }
    }
    doc
}

#[test]
fn rejects_a_dependency_cycle_and_names_the_resources_in_it() {
    // Two resources waiting for each other.
    let err = validate(&parse_one(&chain(&[("a", &["b"]), ("b", &["a"])])))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "doc.yaml:7:7: spec.resources[0].dependsOn: dependency cycle among 2 resources: a -> b -> a"
    );

    // A longer loop, with resources outside it left out of the report.
    let doc = chain(&[
        ("base", &[]),
        ("a", &["c", "base"]),
        ("b", &["a"]),
        ("c", &["b"]),
        ("leaf", &["c"]),
    ]);
    let err = validate(&parse_one(&doc)).unwrap_err();
    assert_eq!(err.len(), 1, "one cycle is one diagnostic: {err}");
    assert!(
        err.to_string()
            .ends_with("dependency cycle among 3 resources: a -> b -> c -> a"),
        "{err}"
    );
}

#[test]
fn a_cycle_is_found_through_the_whole_bundle_check_too() {
    let docs = parse(
        "doc.yaml",
        chain(&[("a", &["b"]), ("b", &["a"])]).as_bytes(),
    )
    .unwrap();
    let err = Bundle::new(docs).validate().unwrap_err().to_string();
    assert!(err.contains("dependency cycle among 2 resources"), "{err}");
}

#[test]
fn accepts_dependencies_that_only_look_like_a_cycle() {
    // A diamond: two paths to one resource, and no way back.
    validate(&parse_one(&chain(&[
        ("top", &[]),
        ("left", &["top"]),
        ("right", &["top"]),
        ("bottom", &["left", "right"]),
    ])))
    .unwrap();

    // A notification loop is legal: notify is a trigger, not an order.
    let doc = format!(
        "{HEAD}  resources:\n    - id: a\n      type: service\n      notifies:\n        - resource: b\n          action: restart\n    - id: b\n      type: service\n      notifies:\n        - resource: a\n          action: restart\n"
    );
    validate(&parse_one(&doc)).unwrap();
}

#[test]
fn a_notify_fires_on_a_change_and_on_nothing_else() {
    let notify = |on: &str| {
        format!(
            "{HEAD}  resources:\n    - id: conf\n      type: file\n      notifies:\n        - resource: svc\n          action: restart\n{on}    - id: svc\n      type: service\n"
        )
    };
    validate(&parse_one(&notify(""))).unwrap();
    validate(&parse_one(&notify("          on: changed\n"))).unwrap();

    let err = validate(&parse_one(&notify("          on: always\n")))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with(
            "doc.yaml:10:11: spec.resources[0].notifies[0].on: unknown event \"always\" (expected \"changed\""
        ),
        "{err}"
    );
}

#[test]
fn a_when_is_read_when_the_document_is_validated() {
    let when = |condition: &str| {
        format!("{HEAD}  resources:\n    - id: a\n      type: package\n      when: {condition}\n")
    };
    for good in [
        "data.install",
        "\"data.env == 'prod' && !data.legacy\"",
        "\"defined(data.region) || data.count != 3\"",
    ] {
        validate(&parse_one(&when(good))).unwrap();
    }
    for (bad, want) in [
        (
            "\"data.env ==\"",
            "spec.resources[0].when: the condition ends where a value was expected",
        ),
        (
            "\"facts.os == 'linux'\"",
            "spec.resources[0].when: facts are not available",
        ),
        (
            "installed",
            "spec.resources[0].when: unknown name \"installed\"",
        ),
        (
            "\"data.a & data.b\"",
            "spec.resources[0].when: unexpected \"&\"",
        ),
    ] {
        let err = validate(&parse_one(&when(bad))).unwrap_err().to_string();
        assert!(err.contains(want), "{bad}: {err}");
    }
}
