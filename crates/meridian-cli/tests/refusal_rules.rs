//! The refusal rules reference is generated from the registry in
//! `meridian_sdk::rules` and from what each plugin declares as its own, and
//! this test fails when the page differs. Record it again with
//! `UPDATE_GOLDEN=1` after a rule is added or its meaning changes on purpose.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;

use meridian_plugin_testkit::repo_root;
use meridian_sdk::rules::{self, Rule, GENERAL};

const PATH: &str = "docs/reference/refusal-rules.md";

const INTRODUCTION: &str = "\
# Refusal rules

Every rule a compile-time refusal can carry, with what it refuses and what to
write instead. This page is generated: `crates/meridian-cli/tests/refusal_rules.rs`
writes it from the registry in `meridian_sdk::rules` and from what each plugin
declares, and fails when it is out of date, so a rule cannot appear in the code
without appearing here.

A refusal is a target saying no to a document at compile time: a resource type
it does not map, a param it does not take, a secret where it cannot keep one.
It is not an [error](errors.md). Each refusal names the target, the resource
and the line it is about, carries a message that says what was wrong and what
that target accepts, and carries a *rule*: one of the names below. The name is
the same for the same mistake on every target, and it does not change once it
is published, so a policy check, an editor, a test or a script can key off it.
Beside the rule travels a *hint*, what to write instead in general terms; the
message is the specific one.

`meridian` prints the rule in square brackets after the resource, and the hint
in parentheses after the message:

```
site.yaml:12:5: ansible: resource \"app\" [unsupported-param]: param \"provider\" ... (hint: remove the param, or use the name the message lists)
```

## General rules

The same mistake has the same name on every target, however the target finds
it. A plugin uses one of these wherever it fits.

| Rule | What it refuses | What to write instead |
|---|---|---|
";

const OWN_INTRODUCTION: &str = "
## Rules of one target

A mistake that exists on one tool only has a rule of that target's own, which
the plugin declares in one table and `meridian plugins --rules` prints. A
target that is not listed here refuses only under the general rules.

";

fn row(rule: &Rule) -> String {
    let cell = |text: &str| text.replace('|', "\\|");
    format!(
        "| `{}` | {} | {} |\n",
        rule.name,
        cell(rule.meaning),
        cell(rule.hint)
    )
}

fn page() -> String {
    let mut out = INTRODUCTION.to_owned();
    for rule in GENERAL {
        out.push_str(&row(rule));
    }
    out.push_str(OWN_INTRODUCTION);
    let mut targets: Vec<_> = meridian_all_targets::ALL
        .iter()
        .map(|t| (t.name, t.emitter().rules()))
        .filter(|(_, rules)| !rules.is_empty())
        .collect();
    targets.sort_by_key(|(name, _)| *name);
    for (name, rules) in targets {
        let _ = writeln!(out, "### [{name}](../targets/{name}.md)\n");
        out.push_str("| Rule | What it refuses | What to write instead |\n|---|---|---|\n");
        for rule in &rules {
            out.push_str(&row(rule));
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_documented_rules_are_what_the_registry_and_the_plugins_declare() {
    let path = repo_root().join(PATH);
    let generated = page();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with the rule registry; run the tests with UPDATE_GOLDEN=1 to record it again"
    );
}

/// A plugin's own rule is a name no general rule has, is well formed, says
/// what it refuses, and is declared once.
#[test]
fn a_plugins_own_rules_are_distinct_from_the_general_ones_and_well_formed() {
    for target in meridian_all_targets::ALL {
        let mut seen = BTreeSet::new();
        for rule in target.emitter().rules() {
            let at = format!("{}: rule {:?}", target.name, rule.name);
            assert!(
                rules::well_formed(rule.name),
                "{at} is not lower-case words joined by hyphens"
            );
            assert!(
                rules::general(rule.name).is_none(),
                "{at} is a general rule; use `meridian_sdk::rules` instead of declaring it"
            );
            assert!(seen.insert(rule.name), "{at} is declared twice");
            assert!(!rule.meaning.is_empty(), "{at} has no meaning");
        }
    }
}

/// The registry is the plugins' only source of rule names: every name a
/// plugin's source refuses with is a constant, so a literal rule name in a
/// plugin would be a name the reference page cannot know about.
#[test]
fn no_plugin_writes_a_rule_name_as_a_literal() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for target in meridian_all_targets::ALL {
        let src = root.join("plugins").join(target.name).join("src");
        for entry in walk(&src) {
            let text = fs::read_to_string(&entry).unwrap();
            // Tests compare a refusal's rule against the name it expects,
            // which is the one place a literal belongs.
            let code = text.split("#[cfg(test)]").next().unwrap_or_default();
            for (number, line) in code.lines().enumerate() {
                if line.contains("rule: \"") {
                    offenders.push(format!(
                        "{}:{}: {}",
                        entry.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "rule names written as literals:\n{}",
        offenders.join("\n")
    );
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}
