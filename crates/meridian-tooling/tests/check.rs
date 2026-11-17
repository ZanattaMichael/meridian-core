//! Diagnostics, against the real compiler and real plugins.

mod common;

use common::{check, marked};
use meridian_tooling::check::Severity;

const GOOD: &str = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx
    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
      dependsOn:
        - resource: nginx_pkg
";

#[test]
fn a_document_that_compiles_has_nothing_to_report() {
    assert_eq!(check(GOOD), []);
}

#[test]
fn a_validation_error_lands_on_the_text_it_is_about() {
    let text = GOOD.replace("- resource: nginx_pkg", "- resource: nginx_pkgs");
    let found = check(&text);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(marked(&text, &found[0]), "nginx_pkgs");
    assert_eq!(found[0].source, "meridian");
    assert!(
        found[0].message.contains("unknown resource"),
        "{}",
        found[0].message
    );
}

#[test]
fn an_unknown_field_is_marked_at_the_field() {
    let text = GOOD.replace("      state: running", "      stat: running");
    let found = check(&text);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(marked(&text, &found[0]), "stat");
    assert!(found[0].message.contains("unknown field"));
}

#[test]
fn every_resource_the_target_refuses_is_marked_not_only_the_first() {
    let text = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: puppet
  targetHost:
    from: inline
    host: web01
  resources:
    - id: a
      type: package
      params:
        name: nginx
        flavour: light
    - id: b
      type: service
      state: restarted
      params:
        name: nginx
    - id: c
      type: file
      params:
        mode: \"0644\"
    - id: d
      type: cron
";
    let found = check(text);
    let seen: Vec<(&str, &str)> = found
        .iter()
        .map(|d| (d.code.as_str(), marked(text, d)))
        .collect();
    assert_eq!(
        seen,
        [
            ("unsupported-param", "flavour"),
            ("unsupported-state", "restarted"),
            ("missing-param", "params"),
            ("unsupported-resource-type", "cron"),
        ],
        "{found:#?}"
    );
    assert!(found
        .iter()
        .all(|d| d.source == "puppet" && d.severity == Severity::Error));
}

#[test]
fn a_refusal_only_the_plugin_knows_about_still_arrives() {
    // A schema says which params and states a type takes. Whether the
    // target can express a condition checked at apply time is not in it, so
    // this refusal can only come from compiling.
    let text = GOOD.replace("target: ansible", "target: puppet").replace(
        "      state: running\n",
        "      state: running\n      runtimeWhen: x\n",
    );
    let found = check(&text);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].source, "puppet");
    assert_eq!(marked(&text, &found[0]), "x");
}

#[test]
fn a_target_warning_is_a_warning_on_the_resource() {
    let text = GOOD.replace("target: ansible", "target: cloud-init");
    let found = check(&text);
    assert!(!found.is_empty());
    assert!(
        found
            .iter()
            .all(|d| d.severity == Severity::Warning && d.code == "target-warning"),
        "{found:#?}"
    );
}

#[test]
fn a_target_no_plugin_provides_is_marked_at_its_name() {
    let text = GOOD.replace("target: ansible", "target: ansibel");
    let found = check(&text);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "unknown-target");
    assert_eq!(marked(&text, &found[0]), "ansibel");
    assert!(found[0].message.contains("ansible"));
}

#[test]
fn a_kind_the_target_cannot_take_and_a_version_it_does_not_have() {
    let text = GOOD.replace("target: ansible", "target: terraform");
    let found = check(&text);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "kind-does-not-fit-target");
    assert_eq!(marked(&text, &found[0]), "ResourceSet");

    let text = GOOD.replace("target: ansible", "target: ansible\n  targetVersion: 9");
    let found = check(&text);
    assert_eq!(found[0].code, "unsupported-target-version");
    assert_eq!(marked(&text, &found[0]), "9");
}

#[test]
fn one_broken_document_does_not_hide_the_others() {
    let text = format!(
        "{GOOD}---\napiVersion: meridian/v1\nkind: Nonsense\n---\n{}",
        GOOD.replace("name: web", "name: web2")
            .replace("type: package", "type: pkg")
    );
    let found = check(&text);
    let codes: Vec<&str> = found.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(
        codes,
        ["invalid-document", "unsupported-resource-type"],
        "{found:#?}"
    );
    assert_eq!(found[0].span.start.line, 22);
    assert_eq!(marked(&text, &found[1]), "pkg");
}

#[test]
fn text_that_is_not_yaml_is_a_syntax_error_where_it_breaks() {
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nspec:\n  resources: [a, b\n";
    let found = check(text);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "syntax");
}

#[test]
fn yaml_that_is_not_meridian_is_left_alone() {
    assert_eq!(check("name: something else\nitems: [1, 2]\n"), []);
    assert_eq!(check(""), []);
}

#[test]
fn a_reference_between_files_is_checked_across_them() {
    let registry = common::registry();
    let catalog = meridian_tooling::catalog::Catalog::from_registry(&registry);
    let infra = "\
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: terraform
  resources:
    - id: vm
      type: aws_instance
      params:
        ami: ami-1
  outputs:
    - name: address
      value: ${vm.public_ip}
";
    let host = GOOD.replace(
        "    from: inline\n    host: web01\n",
        "    from: infrastructure\n    ref: network\n    output: adress\n",
    );
    let ws = common::workspace(&[("infra.yaml", infra), ("web.yaml", &host)]);
    let env = meridian_tooling::check::Env {
        registry: Some(&registry),
        catalog: &catalog,
        data: None,
    };
    let found = meridian_tooling::check::check(&ws, &env);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].file, "web.yaml");
    assert!(found[0].message.contains("adress"), "{}", found[0].message);

    let fixed = host.replace("adress", "address");
    let ws = common::workspace(&[("infra.yaml", infra), ("web.yaml", &fixed)]);
    assert_eq!(meridian_tooling::check::check(&ws, &env), []);
}

#[test]
fn a_conditional_resource_is_checked_as_if_its_condition_held() {
    let text = GOOD.replace(
        "      type: package\n",
        "      type: package\n      when: data.env == \"prod\"\n",
    );
    assert_eq!(check(&text), []);
}

fn codes(text: &str) -> Vec<String> {
    check(text).into_iter().map(|d| d.code).collect()
}

#[test]
fn a_document_that_needs_no_data_is_worked_out_and_compiled() {
    // The variable is placed before the plugin sees the document, so a
    // value the plugin would refuse is found.
    let text = GOOD
        .replace(
            "  resources:\n",
            "  variables:\n    wanted: bouncing\n  resources:\n",
        )
        .replace("state: running", "state: \"${{ var.wanted }}\"");
    assert_eq!(codes(&text), ["unsupported-state"]);
    let text = text.replace("wanted: bouncing", "wanted: running");
    assert_eq!(check(&text), []);
}

#[test]
fn a_document_that_needs_data_is_checked_as_far_as_it_can_be_without_it() {
    // Nobody can place `data.name` in an editor, so the plugin is not asked
    // about text its tool will never see...
    let text = GOOD.replace(
        "name: nginx\n    - id",
        "name: \"${{ data.name }}\"\n    - id",
    );
    assert_eq!(check(&text), []);
    // ...but what the target says about each resource is still checked.
    let text = text.replace("state: running", "state: running\n      colour: blue");
    assert_eq!(codes(&text), ["invalid-document"]);
    let text = GOOD
        .replace(
            "name: nginx\n    - id",
            "name: \"${{ data.name }}\"\n    - id",
        )
        .replace(
            "        name: nginx\n      dependsOn",
            "        name: nginx\n        colour: blue\n      dependsOn",
        );
    assert_eq!(codes(&text), ["unsupported-param"]);
}

#[test]
fn the_new_fields_of_the_language_are_accepted_where_they_belong() {
    let text = GOOD
        .replace(
            "  resources:\n",
            "  when: \"true\"\n  conditions:\n    always: \"true\"\n  secrets:\n    - name: token\n      \
             from: env\n      key: TOKEN\n      optional: true\n  resources:\n",
        )
        .replace(
            "        - resource: nginx_pkg\n",
            "        - resource: nginx_pkg\n          when: cond.always\n",
        );
    assert_eq!(check(&text), []);
    // A field the language does not have is still refused, there.
    let text = text.replace("key: TOKEN", "kee: TOKEN");
    assert_eq!(codes(&text), ["invalid-document"]);
}
