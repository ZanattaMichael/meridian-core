//! Completion, hover, definitions, references and the outline, driven by
//! the language's own tables and by what real plugins declare.

mod common;

use common::cursor;
use meridian_tooling::analysis::{
    complete, definition, hover, references, symbols, Completion, CompletionKind,
};
use meridian_tooling::catalog::Catalog;
use meridian_tooling::syntax::Span;

fn completions(marked: &str) -> Vec<Completion> {
    let (text, pos) = cursor(marked);
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    complete(
        &common::workspace(&[("doc.yaml", &text)]),
        &catalog,
        "doc.yaml",
        pos,
    )
}

fn labels(marked: &str) -> Vec<String> {
    completions(marked).into_iter().map(|c| c.label).collect()
}

fn hover_text(marked: &str) -> String {
    let (text, pos) = cursor(marked);
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    hover(
        &common::workspace(&[("doc.yaml", &text)]),
        &catalog,
        "doc.yaml",
        pos,
    )
    .map(|h| h.markdown)
    .unwrap_or_default()
}

const HEAD: &str = "\
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
      params:
        name: nginx
";

#[test]
fn an_empty_file_offers_whole_documents_and_the_top_level_fields() {
    let found = completions("|");
    let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        names,
        [
            "ResourceSet document",
            "Infrastructure document",
            "apiVersion",
            "kind",
            "metadata",
            "spec"
        ]
    );
    assert!(found[0].snippet && found[0].insert.contains("kind: ResourceSet"));
    // The only version there is gets written out in full.
    assert_eq!(found[2].insert, "apiVersion: meridian/v1");
}

#[test]
fn fields_come_from_the_language_and_leave_out_what_is_already_there() {
    assert_eq!(
        labels(&format!("{HEAD}      |")),
        ["state", "when", "runtimeWhen", "dependsOn", "notifies"]
    );
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  |"),
        [
            "target",
            "targetVersion",
            "when",
            "variables",
            "conditions",
            "secrets",
            "targetHost",
            "imports",
            "resources"
        ]
    );
    assert_eq!(
        labels("kind: Infrastructure\nspec:\n  |"),
        [
            "target",
            "targetVersion",
            "when",
            "variables",
            "conditions",
            "secrets",
            "imports",
            "resources",
            "outputs"
        ]
    );
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  tar|"),
        [
            "target",
            "targetVersion",
            "when",
            "variables",
            "conditions",
            "secrets",
            "targetHost",
            "imports",
            "resources"
        ]
    );
    assert_eq!(
        labels(&format!("{HEAD}      dependsOn:\n        - |")),
        ["resource", "ifMissing", "when"]
    );
    assert_eq!(
        labels(&format!(
            "{HEAD}      notifies:\n        - resource: nginx_pkg\n          |"
        )),
        ["on", "action", "ifMissing", "when"]
    );
}

#[test]
fn a_field_documents_itself_and_inserts_its_shape() {
    let found = completions(&format!("{HEAD}      |"));
    let depends = found.iter().find(|c| c.label == "dependsOn").unwrap();
    assert_eq!(depends.insert, "dependsOn:\n  - resource: $0");
    assert!(
        depends.doc.contains("must be applied before"),
        "{}",
        depends.doc
    );
    // After a dash, continuation lines sit under the key, not the dash.
    let found = completions(&format!("{HEAD}    - |"));
    let params = found.iter().find(|c| c.label == "params").unwrap();
    assert_eq!(params.insert, "params:\n    $0");
}

#[test]
fn values_with_a_fixed_set_are_offered() {
    assert_eq!(
        labels("apiVersion: meridian/v1\nkind: |"),
        ["Infrastructure", "ResourceSet"]
    );
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  targetHost:\n    from: |"),
        ["inline", "infrastructure"]
    );
    assert_eq!(
        labels(&format!(
            "{HEAD}      dependsOn:\n        - resource: nginx_pkg\n          ifMissing: |"
        )),
        ["error", "skip"]
    );
}

#[test]
fn targets_come_from_the_loaded_plugins_and_fit_the_kind() {
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  target: |"),
        [
            "ansible",
            "chef",
            "cloud-init",
            "dsc",
            "openvox",
            "puppet",
            "salt"
        ]
    );
    assert_eq!(
        labels("kind: Infrastructure\nspec:\n  target: |"),
        ["terraform"]
    );
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  target: dsc\n  targetVersion: |"),
        ["2", "3"]
    );
}

#[test]
fn types_states_and_params_come_from_the_target_plugin() {
    assert_eq!(
        labels(&format!("{HEAD}    - id: x\n      type: |")),
        ["exec", "file", "group", "package", "service", "user"]
    );
    assert_eq!(
        labels(&format!("{HEAD}      state: |")),
        ["reloaded", "restarted", "running", "started", "stopped"]
    );
    // Puppet describes a state a node is left in, so it has fewer.
    let puppet = HEAD.replace("target: ansible", "target: puppet");
    assert_eq!(
        labels(&format!("{puppet}      state: |")),
        ["running", "started", "stopped"]
    );

    let found = completions(&format!("{HEAD}        |"));
    let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(names, ["enabled"]);
    assert_eq!(found[0].insert, "enabled: ${1|true,false|}");
    assert!(
        found[0].doc.contains("starts when the machine boots"),
        "{}",
        found[0].doc
    );

    // cloud-init takes user params the others do not.
    let cloud = HEAD.replace("target: ansible", "target: cloud-init");
    let user = format!("{cloud}    - id: u\n      type: user\n      params:\n        |");
    assert!(labels(&user).contains(&"ssh_authorized_keys".to_owned()));
    let ansible_user = format!("{HEAD}    - id: u\n      type: user\n      params:\n        |");
    assert!(!labels(&ansible_user).contains(&"ssh_authorized_keys".to_owned()));
}

#[test]
fn a_mode_is_inserted_quoted() {
    let found = completions(&format!(
        "{HEAD}    - id: f\n      type: file\n      params:\n        path: /x\n        mo|"
    ));
    let mode = found.iter().find(|c| c.label == "mode").unwrap();
    assert_eq!(mode.insert, "mode: \"${1:0644}\"");
}

#[test]
fn a_pattern_type_becomes_a_snippet() {
    let found = completions(
        "kind: Infrastructure\nspec:\n  target: terraform\n  resources:\n    - id: vm\n      type: |",
    );
    let pattern = found
        .iter()
        .find(|c| c.label == "<provider>_<type>")
        .unwrap();
    assert!(pattern.snippet);
    assert_eq!(pattern.insert, "${1:provider}_${2:type}");
    assert!(found.iter().any(|c| c.label == "module" && !c.snippet));
}

#[test]
fn a_new_resource_can_be_started_from_its_type() {
    let found = completions(&format!("{HEAD}    - |"));
    let file = found.iter().find(|c| c.label == "file resource").unwrap();
    assert_eq!(file.kind, CompletionKind::Snippet);
    assert_eq!(
        file.insert,
        "id: ${1:file_id}\n  type: file\n  params:\n    path: $2"
    );
}

#[test]
fn references_offer_the_other_resources_and_notify_actions_those_of_the_notified() {
    assert_eq!(
        labels(&format!("{HEAD}      dependsOn:\n        - resource: |")),
        ["nginx_pkg"]
    );
    let text = HEAD.replace(
        "        name: nginx\n    - id: nginx_svc",
        "        name: nginx\n      notifies:\n        - resource: nginx_svc\n          action: |\n    - id: nginx_svc",
    );
    assert_eq!(labels(&text), ["reload", "restart", "start", "stop"]);
    let puppet = text.replace("target: ansible", "target: puppet");
    assert_eq!(labels(&puppet), ["refresh", "restart"]);
}

#[test]
fn inside_a_reference_ids_then_params_are_offered() {
    let base =
        format!("{HEAD}    - id: conf\n      type: file\n      params:\n        path: /etc/x\n");
    let found = completions(&format!("{base}        content: \"listen ${{|\"\n"));
    let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(names, ["nginx_pkg", "nginx_svc", "conf"]);
    assert_eq!(
        labels(&format!("{base}        content: \"${{nginx_pkg.|}}\"\n")),
        ["name"]
    );
    // An escaped `$${` is literal text.
    assert_eq!(
        labels(&format!("{base}        content: \"$${{|\"\n")),
        Vec::<String>::new()
    );
}

#[test]
fn hover_explains_fields_values_types_and_references() {
    assert!(hover_text("apiVersion: meridian/v1\nki|nd: ResourceSet\n")
        .contains("What the document describes"));
    assert!(hover_text("apiVersion: meridian/v1\nkind: Resource|Set\n").contains("already exists"));
    let target = hover_text(&HEAD.replace("target: ansible", "target: ans|ible"));
    assert!(
        target.contains("configuration-management target") && target.contains("`package`"),
        "{target}"
    );
    let ty = hover_text(&HEAD.replace("type: service", "type: ser|vice"));
    assert!(
        ty.contains("| `enabled` | boolean |") && ty.contains("`running` (default)"),
        "{ty}"
    );
    assert!(ty.contains("Can be notified to: `reload`"), "{ty}");
    let param = hover_text(&HEAD.replace(
        "        name: nginx\n    - id: nginx_svc",
        "        na|me: nginx\n    - id: nginx_svc",
    ));
    assert!(
        param.contains("required") && param.contains("The package's name."),
        "{param}"
    );
    let reference = hover_text(&format!(
        "{HEAD}      dependsOn:\n        - resource: nginx|_pkg\n"
    ));
    assert!(
        reference.contains("resource of type `package`") && reference.contains("line 11"),
        "{reference}"
    );
    let field = hover_text(&format!(
        "{HEAD}      depends|On:\n        - resource: nginx_pkg\n"
    ));
    assert!(
        field.contains("`spec.resources.dependsOn`") && field.contains("optional"),
        "{field}"
    );
    assert_eq!(hover_text(&HEAD.replace("name: web", "name: we|b")), "");
}

#[test]
fn a_reference_leads_to_its_declaration_and_back() {
    let marked = format!(
        "{HEAD}      dependsOn:\n        - resource: nginx|_pkg\n    - id: conf\n      type: file\n      \
         params:\n        path: /x\n        content: \"${{nginx_pkg.name}} and $${{nginx_pkg.name}}\"\n"
    );
    let (text, pos) = cursor(&marked);
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    let ws = common::workspace(&[("doc.yaml", &text)]);
    let found = definition(&ws, &catalog, "doc.yaml", pos).unwrap();
    assert_eq!(found.span, Span::on_line(10, 10, 19));

    let spans: Vec<Span> = references(&ws, &catalog, "doc.yaml", pos)
        .into_iter()
        .map(|l| l.span)
        .collect();
    assert_eq!(
        spans,
        [
            Span::on_line(10, 10, 19),
            Span::on_line(19, 20, 29),
            Span::on_line(24, 20, 29),
        ]
    );
    // From the declaration itself the same three are found.
    let from_id = references(
        &ws,
        &catalog,
        "doc.yaml",
        meridian_tooling::syntax::Pos::new(10, 12),
    );
    assert_eq!(from_id.len(), 3);
    // And from inside the `${...}`.
    let from_expr = definition(
        &ws,
        &catalog,
        "doc.yaml",
        meridian_tooling::syntax::Pos::new(24, 22),
    )
    .unwrap();
    assert_eq!(from_expr.span, Span::on_line(10, 10, 19));
}

#[test]
fn a_target_host_leads_to_the_infrastructure_document_in_another_file() {
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
  outputs:
    - name: address
      value: ${vm.public_ip}
";
    let marked = HEAD.replace(
        "    from: inline\n    host: web01\n",
        "    from: infrastructure\n    ref: netw|ork\n    output: address\n",
    );
    let (text, pos) = cursor(&marked);
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    let ws = common::workspace(&[("infra.yaml", infra), ("web.yaml", &text)]);
    let found = definition(&ws, &catalog, "web.yaml", pos).unwrap();
    assert_eq!(
        (found.file.as_str(), found.span),
        ("infra.yaml", Span::on_line(3, 8, 15))
    );

    let on_output = meridian_tooling::syntax::Pos::new(pos.line + 1, 14);
    let found = definition(&ws, &catalog, "web.yaml", on_output).unwrap();
    assert_eq!(
        (found.file.as_str(), found.span),
        ("infra.yaml", Span::on_line(10, 12, 19))
    );
    let shown = hover(&ws, &catalog, "web.yaml", on_output)
        .unwrap()
        .markdown;
    assert!(shown.contains("${vm.public_ip}"), "{shown}");

    // And both names are offered while typing.
    let (typing, at) = cursor(&marked.replace("ref: netw|ork", "ref: |"));
    let ws = common::workspace(&[("infra.yaml", infra), ("web.yaml", &typing)]);
    let names: Vec<String> = complete(&ws, &catalog, "web.yaml", at)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(names, ["network"]);
    let (typing, at) = cursor(
        &marked
            .replace("netw|ork", "network")
            .replace("output: address", "output: |"),
    );
    let ws = common::workspace(&[("infra.yaml", infra), ("web.yaml", &typing)]);
    let names: Vec<String> = complete(&ws, &catalog, "web.yaml", at)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(names, ["address"]);
}

#[test]
fn the_outline_lists_documents_with_their_resources_and_outputs() {
    let text = format!("{HEAD}---\napiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  outputs:\n    - name: ip\n      value: x\n");
    let ws = common::workspace(&[("doc.yaml", &text)]);
    let outline = symbols(ws.file("doc.yaml").unwrap());
    let shape: Vec<(String, String, Vec<String>)> = outline
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.detail.clone(),
                s.children
                    .iter()
                    .map(|c| format!("{} {}", c.name, c.detail))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (
                "web".to_owned(),
                "ResourceSet".to_owned(),
                vec![
                    "nginx_pkg package".to_owned(),
                    "nginx_svc service".to_owned()
                ]
            ),
            (
                "net".to_owned(),
                "Infrastructure".to_owned(),
                vec!["ip x".to_owned()]
            ),
        ]
    );
}

#[test]
fn secrets_and_what_a_condition_can_read_are_offered() {
    let head = HEAD.replace(
        "  resources:\n",
        "  variables:\n    size: 3\n  conditions:\n    is_prod: data.env == 'prod'\n  secrets:\n    \
         - name: token\n      from: env\n      key: TOKEN\n  resources:\n",
    );
    let found = labels(&format!("{head}      when: |"));
    for expected in [
        "data.",
        "defined()",
        "cond.is_prod",
        "var.size",
        "secret.token",
    ] {
        assert!(
            found.iter().any(|l| l == expected),
            "{expected} in {found:?}"
        );
    }
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  secrets:\n    - name: token\n      from: |"),
        ["env", "file", "exec"]
    );
    assert_eq!(
        labels("kind: ResourceSet\nspec:\n  secrets:\n    - name: token\n      from: env\n      |"),
        [
            "key",
            "path",
            "command",
            "optional",
            "allowWideMode",
            "keepTrailingNewline"
        ]
    );
}
