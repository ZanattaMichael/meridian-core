//! What a rule name means, and what to do about it.
//!
//! The lint rules explain themselves (`meridian_tooling::lint::RULES`).
//! The rules the compiler, the validator and the schema check produce are
//! named in `meridian-tooling`'s checker and in each plugin's refusals;
//! until the SDK carries a registry of them (issue 88), the ones every
//! target shares are described here, in the words of the checker that
//! raises them. A rule not listed is a target's own, and the answer says so
//! and where to read about it.

use meridian_tooling::lint;
use serde_json::{json, Value};

use crate::Server;

/// One rule the compiler or the checker raises, with what it means and
/// what to write instead.
struct Rule {
    name: &'static str,
    meaning: &'static str,
    fix: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        name: "syntax",
        meaning: "The text is not YAML or JSON that can be read.",
        fix: "Fix the YAML at the line named: indentation, a missing colon, an unclosed quote. \
              `format` cannot help until the text parses.",
    },
    Rule {
        name: "invalid-document",
        meaning: "The document does not fit the language: a field that does not exist, a \
                  value of the wrong kind, a missing required field, a dependency on a \
                  resource that does not exist, or a cycle.",
        fix: "Call `describe_document` for the fields the kind takes. The message names the \
              path of the field.",
    },
    Rule {
        name: "invalid-condition",
        meaning: "A `when` condition or an expression cannot be read or evaluated.",
        fix: "Read the expressions page (resource `meridian://docs/reference/dsl/expressions`). \
              Conditions read `data.<key>`, `var.<name>` and `cond.<name>` only.",
    },
    Rule {
        name: "unknown-target",
        meaning: "No loaded plugin provides the target `spec.target` names.",
        fix: "Use one of the targets `list_targets` returns; the message lists them.",
    },
    Rule {
        name: "kind-does-not-fit-target",
        meaning: "The document's `kind` is not one this target takes: an infrastructure-as-code \
                  or container-orchestration target takes `Infrastructure`, a \
                  configuration-management target takes `ResourceSet`.",
        fix: "Change `kind`, and the fields that go with it: a ResourceSet has `spec.targetHost`, \
              an Infrastructure has `spec.outputs`. `list_targets` gives each target's kinds.",
    },
    Rule {
        name: "unsupported-target-version",
        meaning: "`spec.targetVersion` names a version of the tool the target does not compile \
                  for, or the target names no versions at all.",
        fix: "Use a version from `list_targets`, or remove `spec.targetVersion`.",
    },
    Rule {
        name: "unsupported-resource-type",
        meaning: "The resource's `type` is not one this target maps.",
        fix: "Use a type from `list_resource_types`; `accepted` lists them when there are few.",
    },
    Rule {
        name: "unsupported-param",
        meaning: "The resource gives a param its type does not take on this target.",
        fix: "Remove the param, or use one `describe_resource_type` lists; `accepted` has the \
              names.",
    },
    Rule {
        name: "missing-param",
        meaning: "The resource leaves out a param its type requires on this target.",
        fix: "Add the param named in the message under `params`.",
    },
    Rule {
        name: "invalid-param",
        meaning: "A param holds a value of the wrong kind for the target: a number where text \
                  is wanted, such as a file mode, or text where a list is.",
        fix: "Write the value as the kind `describe_resource_type` gives; quote a mode \
              (`\"0644\"`) or a version so YAML reads it as text.",
    },
    Rule {
        name: "empty-param",
        meaning: "A param is present but holds nothing.",
        fix: "Give the param a value, or remove it.",
    },
    Rule {
        name: "wrong-param-kind",
        meaning: "A param was given text where the target wants a list or a mapping, or the \
                  other way round.",
        fix: "`describe_resource_type` gives each param's kind; write a list as `- item` lines \
              and a mapping as `key: value` lines.",
    },
    Rule {
        name: "unsupported-value",
        meaning: "A param with a fixed set of values was given one not in the set.",
        fix: "Use one of the values in `accepted`.",
    },
    Rule {
        name: "computed-param",
        meaning: "The param is set by the tool after apply, so a document cannot write it.",
        fix: "Remove it; read it from another resource with `${id.param}` if you need its value.",
    },
    Rule {
        name: "unsupported-state",
        meaning: "The resource's `state` is not one its type takes on this target.",
        fix: "Use one of the states in `accepted`, or leave `state` out for the default.",
    },
    Rule {
        name: "unsupported-action",
        meaning: "A `notifies` entry asks the notified resource for an action its type cannot \
                  perform on this target.",
        fix: "Use an action from `accepted`; `describe_resource_type` lists the actions of \
              the notified type.",
    },
    Rule {
        name: "unsupported-notify",
        meaning: "The target cannot act when another resource changes, so `notifies` cannot be \
                  compiled for it.",
        fix: "Remove `notifies` and order the resources with `dependsOn`, or choose a target \
              whose `capabilities` include notify.",
    },
    Rule {
        name: "unsupported-runtime-condition",
        meaning: "The target cannot evaluate a condition on the node at apply time, which \
                  `runtimeWhen` asks for.",
        fix: "Use `when`, which Meridian decides at compile time from data, or remove the \
              condition.",
    },
    Rule {
        name: "unsupported-setting",
        meaning: "`spec.settings` names a key this target does not take.",
        fix: "Remove the key; the message lists the keys the target accepts, if any.",
    },
    Rule {
        name: "missing-target-host",
        meaning: "A ResourceSet names no `spec.targetHost`, so there is no machine to configure.",
        fix: "Add `targetHost: { from: inline, host: <name> }`, or `from: infrastructure` with \
              `ref` and `output` naming an Infrastructure document's output.",
    },
    Rule {
        name: "target-warning",
        meaning: "The target compiled the document but could not keep something as written, \
                  and says what it changed.",
        fix: "Read the message; the document compiles as it stands.",
    },
    Rule {
        name: "plugin-failed",
        meaning: "The target's plugin process failed, which says nothing about the document.",
        fix: "Try again; if it persists the plugin is broken on this machine.",
    },
    Rule {
        name: "refused",
        meaning: "The target refused the document without naming a rule.",
        fix: "Read the message; it says what the target cannot do.",
    },
    Rule {
        name: "needs-data",
        meaning: "The document reads `data.<key>` or a secret in a condition or an \
                  interpolation, and the server has no data and fetches no secret, so it was \
                  checked against the language only and not compiled.",
        fix: "This is not a mistake. `meridian compile --data FILE` compiles it with values; \
              to check it here, give the values with `spec.variables` instead.",
    },
    Rule {
        name: "import-source",
        meaning: "The importer could not read the source text as its tool's configuration.",
        fix: "Fix the source at the line named, or give it as the format the target imports \
              (`list_targets` lists each target's formats).",
    },
];

/// Answers `explain`.
pub(crate) fn explain(server: &Server, rule: &str, target: Option<&str>) -> Value {
    let target_page = target.map(|t| format!("meridian://docs/targets/{t}"));
    if let Some(found) = lint::rule(rule) {
        return json!({
            "rule": found.code,
            "kind": "lint",
            "severity": found.severity.as_str(),
            "meaning": found.summary,
            "fix": found.explanation,
            "reference": "meridian://docs/reference/lint",
        });
    }
    if let Some(found) = RULES.iter().find(|r| r.name == rule) {
        let mut out = json!({
            "rule": found.name,
            "kind": "compiler",
            "meaning": found.meaning,
            "fix": found.fix,
        });
        if let Some(target) = target {
            if let Some(t) = server.catalog().target(target) {
                out["target"] = json!({
                    "name": t.name,
                    "category": t.category.as_str(),
                    "resourceTypes": t.types.len(),
                });
            }
        }
        if let Some(page) = target_page {
            out["reference"] = page.into();
        }
        return out;
    }
    let mut out = json!({
        "rule": rule,
        "kind": "target",
        "meaning": format!(
            "`{rule}` is not a rule of the language or the linter, so it is one the target's \
             own compiler raises for something its tool cannot express."
        ),
        "fix": "The diagnostic's message says what the target refuses and, where the target \
                knows, what it accepts instead. Prefer the shared types and the params \
                `describe_resource_type` lists; a tool's own type passes its params through \
                and is checked by the tool.",
    });
    if let Some(page) = target_page {
        out["reference"] = page.into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_names_are_lower_case_words() {
        for rule in RULES {
            assert!(rule
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'));
            assert!(!rule.meaning.is_empty() && !rule.fix.is_empty());
        }
        let mut names: Vec<&str> = RULES.iter().map(|r| r.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), RULES.len(), "every rule is named once");
    }
}
