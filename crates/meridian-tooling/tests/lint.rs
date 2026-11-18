//! The lint rules, one test each, and the comments that silence them.

mod common;

use common::marked;
use meridian_tooling::catalog::Catalog;
use meridian_tooling::check::Diagnostic;
use meridian_tooling::lint::{apply_fixes, lint, RULES};

fn doc(resources: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: web\nspec:\n  \
         target: ansible\n  targetHost:\n    from: inline\n    host: web01\n  resources:\n{resources}"
    )
}

fn run(text: &str) -> Vec<Diagnostic> {
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    lint(&common::workspace(&[("doc.yaml", text)]), &catalog)
}

fn found(text: &str) -> Vec<(String, String)> {
    run(text)
        .iter()
        .map(|d| (d.code.clone(), marked(text, d).to_owned()))
        .collect()
}

fn pair(code: &str, text: &str) -> (String, String) {
    (code.to_owned(), text.to_owned())
}

const CLEAN: &str = "    - id: conf
      type: file
      params:
        path: /etc/app.conf
        content: x
        mode: \"0644\"
    - id: svc
      type: service
      params:
        name: app
      dependsOn:
        - resource: conf
";

#[test]
fn a_tidy_document_has_no_lints() {
    assert_eq!(found(&doc(CLEAN)), []);
}

#[test]
fn unquoted_mode_is_found_and_fixed() {
    let text = doc(&CLEAN.replace("\"0644\"", "0644"));
    assert_eq!(found(&text), [pair("unquoted-mode", "0644")]);
    let (fixed, applied) = apply_fixes("doc.yaml", &text, &run(&text));
    assert_eq!(applied, 1);
    assert_eq!(fixed, doc(CLEAN));
}

#[test]
fn lossy_number_is_found_and_fixed_where_the_spelling_is_lost() {
    // Each of these reaches the tool as a different spelling.
    let noisy = doc("    - id: app
      type: exec
      params:
        command: run
        creates: /x
        version: 1.10
        release: 3.0
        size: 1_000
        offset: +7
        ratio: .5
        bytes: 1e3
        tags:
          - 1.10
          - two
        limits:
          cpu: 2.50
");
    assert_eq!(
        found(&noisy),
        [
            pair("lossy-number", "1.10"),
            pair("lossy-number", "3.0"),
            pair("lossy-number", "1_000"),
            pair("lossy-number", "+7"),
            pair("lossy-number", ".5"),
            pair("lossy-number", "1e3"),
            pair("lossy-number", "1.10"),
            pair("lossy-number", "2.50"),
        ]
    );
    let diagnostics = run(&noisy);
    assert!(diagnostics.iter().all(|d| d.fix.is_some()));
    let (fixed, applied) = apply_fixes("doc.yaml", &noisy, &diagnostics);
    assert_eq!(applied, 8);
    assert_eq!(found(&fixed), []);
    assert!(fixed.contains("version: \"1.10\""), "{fixed}");
    assert!(fixed.contains("          - \"1.10\"\n"), "{fixed}");
    assert!(fixed.contains("cpu: \"2.50\""), "{fixed}");

    // A number whose spelling is its own, `08` (which is text), a quoted
    // one, an interpolation, and a `mode` (which `unquoted-mode` owns) are
    // not this rule's.
    let quiet = doc("    - id: app
      type: exec
      params:
        command: run
        creates: /x
        count: 42
        ratio: 1.5
        negative: -3
        build: 08
        version: \"1.10\"
        replicas: ${{ data.replicas }}
        flow: [1.10, 3.0]
");
    // A `[...]` or `{...}` flow collection is kept as text by the syntax
    // reader, so a number inside one cannot be placed and is not reported.
    assert_eq!(found(&quiet), []);

    // A param the target declares as a number is never reported: `uid` is a
    // whole number on every configuration management target.
    let declared = doc("    - id: deploy
      type: user
      params:
        name: deploy
        uid: 1_500
");
    assert_eq!(found(&declared), []);
}

#[test]
fn literal_secret() {
    let text = doc(
        "    - id: db\n      type: exec\n      params:\n        command: init\n        creates: /x\n",
    )
    .replace("target: ansible", "target: terraform")
    .replace("kind: ResourceSet", "kind: Infrastructure")
    .replace("  targetHost:\n    from: inline\n    host: web01\n", "")
    .replace("type: exec", "type: aws_db_instance")
    .replace("command: init", "password: hunter2")
    .replace("creates: /x", "token_file: /run/token");
    assert_eq!(found(&text), [pair("literal-secret", "hunter2")]);
    // A reference is not a literal.
    let referenced = text.replace("hunter2", "${secrets.db_password}");
    assert_eq!(found(&referenced), []);
}

#[test]
fn duplicate_resource() {
    let text = doc(
        "    - id: a\n      type: package\n      params:\n        name: nginx\n    \
         - id: b\n      type: package\n      params:\n        name: nginx\n",
    );
    let lints = run(&text);
    assert_eq!(lints.len(), 1);
    assert_eq!(lints[0].code, "duplicate-resource");
    assert_eq!(lints[0].span.start.line, 17);
    assert!(lints[0].message.contains("\"a\""), "{}", lints[0].message);
}

#[test]
fn redundant_dependency() {
    let text = doc(
        "    - id: a\n      type: package\n      params:\n        name: a\n    \
         - id: b\n      type: package\n      params:\n        name: b\n      dependsOn:\n        - resource: a\n    \
         - id: c\n      type: package\n      params:\n        name: c\n      dependsOn:\n        - resource: b\n        - resource: a\n        - resource: b\n",
    );
    let lints = run(&text);
    let messages: Vec<&str> = lints.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "\"a\" already comes first, because \"b\" depends on it",
            "\"b\" is already listed"
        ]
    );
}

#[test]
fn useless_if_missing() {
    let text = doc(&CLEAN.replace(
        "        - resource: conf\n",
        "        - resource: conf\n          ifMissing: skip\n",
    ));
    assert_eq!(found(&text), [pair("useless-if-missing", "skip")]);
    // With a `when` on the resource it names, the policy has a job.
    let conditional = text.replace(
        "      type: file\n",
        "      type: file\n      when: data.x == 1\n",
    );
    assert_eq!(found(&conditional), []);
}

#[test]
fn native_type() {
    let module = "    - id: workers\n      type: ansible.builtin.lineinfile\n      params:\n        path: /etc/app.conf\n        line: workers = 4\n";
    assert_eq!(
        found(&doc(module)),
        [pair("native-type", "ansible.builtin.lineinfile")]
    );
    let silenced = module.replace(
        "      type: ansible",
        "      # meridian-lint: disable=native-type\n      type: ansible",
    );
    assert_eq!(found(&doc(&silenced)), []);
    // The shared types are the portable ones, a typo is the compiler's to
    // refuse, and a target that has no shared types has nothing to say.
    assert_eq!(found(&doc(CLEAN)), []);
    assert_eq!(found(&doc(&CLEAN.replace("type: file", "type: fiel"))), []);
    let terraform = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: net\nspec:\n  target: terraform\n  resources:\n    - id: vm\n      type: aws_instance\n      params:\n        ami: ami-1\n";
    assert_eq!(found(terraform), []);
}

#[test]
fn unguarded_exec() {
    let exec =
        "    - id: migrate\n      type: exec\n      params:\n        command: /usr/bin/migrate\n";
    assert_eq!(found(&doc(exec)), [pair("unguarded-exec", "migrate")]);
    let guarded = exec.to_owned() + "        creates: /var/lib/app/migrated\n";
    assert_eq!(found(&doc(&guarded)), []);
    let notified = format!(
        "{exec}    - id: conf\n      type: file\n      params:\n        path: /c\n        content: x\n      \
         notifies:\n        - resource: migrate\n          action: run\n"
    );
    assert_eq!(found(&doc(&notified)), []);
}

#[test]
fn unreferenceable_id_and_unsafe_document_name() {
    let text = doc(&CLEAN
        .replace("id: conf", "id: app.conf")
        .replace("resource: conf", "resource: app.conf"))
    .replace("name: web\n", "name: web/prod\n");
    assert_eq!(
        found(&text),
        [
            pair("unsafe-document-name", "web/prod"),
            pair("unreferenceable-id", "app.conf")
        ]
    );
}

#[test]
fn unused_resource_set_host() {
    let text = doc(CLEAN).replace("    host: web01\n", "    host: web01\n    ref: network\n");
    assert_eq!(found(&text), [pair("unused-resource-set-host", "network")]);
}

#[test]
fn a_comment_silences_a_rule_on_its_line_the_next_line_or_the_file() {
    let noisy = doc(&CLEAN.replace("\"0644\"", "0644"));
    assert_eq!(found(&noisy).len(), 1);

    let same_line = noisy.replace(
        "mode: 0644",
        "mode: 0644 # meridian-lint: disable=unquoted-mode",
    );
    assert_eq!(found(&same_line), []);

    let line_above = noisy.replace(
        "        mode: 0644",
        "        # meridian-lint: disable=unquoted-mode\n        mode: 0644",
    );
    assert_eq!(found(&line_above), []);

    let whole_file = format!("# meridian-lint: disable-file=all\n{noisy}");
    assert_eq!(found(&whole_file), []);

    let another_rule = noisy.replace(
        "mode: 0644",
        "mode: 0644 # meridian-lint: disable=literal-secret",
    );
    assert_eq!(found(&another_rule).len(), 1);
}

/// A document in KYAML (one flow mapping, every string quoted) lints without
/// a panic. The syntax reader keeps a flow collection as text, so a rule
/// that needs a value's own position cannot point inside one; rules that
/// work from the parsed document still report, at the document's first line.
#[test]
fn a_kyaml_document_lints_without_a_panic() {
    let clean = "\
---
{
  apiVersion: \"meridian/v1\",
  kind: \"ResourceSet\",
  metadata: {name: \"web\",},
  spec: {
    target: \"ansible\",
    targetHost: {from: \"inline\", host: \"web01\",},
    resources: [{
      id: \"conf\",
      type: \"file\",
      params: {path: \"/etc/app.conf\", content: \"x\", mode: \"0644\",},
    }],
  },
}
";
    assert_eq!(found(clean), []);
    let noisy = clean
        .replace("id: \"conf\"", "id: \"a.b\"")
        .replace("mode: \"0644\"", "mode: 0644");
    let diagnostics = run(&noisy);
    let codes: Vec<&str> = diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(
        codes,
        ["unquoted-mode", "unreferenceable-id"],
        "{diagnostics:?}"
    );
    // The mode cannot be placed inside the flow mapping, so the report
    // lands on the document's first line (the `{` under the `---` marker),
    // names the number, and has no fix.
    assert_eq!(diagnostics[0].span.start.line, 1);
    assert!(diagnostics[0].fix.is_none());
    assert!(
        diagnostics[0].message.starts_with("mode 420 is read"),
        "{}",
        diagnostics[0].message
    );
}

#[test]
fn every_rule_is_documented_and_tested() {
    let reference = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/lint.md"),
    )
    .expect("docs/reference/lint.md exists");
    let tests = include_str!("lint.rs");
    for rule in RULES {
        assert!(
            reference.contains(&format!("## `{}`", rule.code)),
            "docs/reference/lint.md has no section for {}",
            rule.code
        );
        // A rule's test is named for it: `unquoted-mode` is tested by a
        // function with `unquoted_mode` in its name.
        assert!(
            tests.contains(&rule.code.replace('-', "_")),
            "no test is named for {}",
            rule.code
        );
        assert!(!rule.summary.is_empty() && !rule.explanation.is_empty());
    }
}
