//! What a stage came to, as a value an expression can read, and the checks
//! an expectation makes of it.

use std::collections::BTreeMap;

use meridian_expr::Scope;
use meridian_ir::Data;
use meridian_orchestrator::{Outcome, Reason, StageReport};
use meridian_value::Value;

use crate::document::{Case, Expect};

/// The word a stage's outcome is known by in a test.
pub(crate) fn outcome_word(report: &StageReport) -> &'static str {
    match &report.outcome {
        Outcome::Compiled => "compiled",
        Outcome::Planned(_) => "planned",
        Outcome::Deferred { .. } => "deferred",
        Outcome::LeftOut { .. } => "left-out",
        Outcome::Failed(_) if report.reason == Some(Reason::CompileRefused) => "refused",
        Outcome::Failed(_) => "failed",
        Outcome::NotRun { .. } => "not-run",
        Outcome::Skipped { .. } => "skipped",
        Outcome::Applied { .. } => "applied",
        Outcome::Drift(_) => "drift",
    }
}

/// A refusal, taken apart: the rule, the resource and the message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub rule: String,
    pub resource: String,
    pub message: String,
}

/// Reads a refusal's text as the SDK writes it: `[file:line: ]target[:
/// resource "id"][ [rule]]: message[ (hint: ...)]`. A refusal the
/// orchestrator makes itself, such as a document of the wrong kind for its
/// target, carries no rule and no resource, and is read as its message.
pub(crate) fn parse_refusal(text: &str) -> Refusal {
    let mut refusal = Refusal {
        message: text.to_owned(),
        ..Refusal::default()
    };
    // The rule sits in brackets right before the message's colon; a
    // position or a resource name never holds "]: ".
    let Some(end) = text.find("]: ") else {
        return refusal;
    };
    let Some(start) = text[..end].rfind(" [") else {
        return refusal;
    };
    let rule = &text[start + 2..end];
    if rule.is_empty()
        || !rule
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/')
    {
        return refusal;
    }
    refusal.rule = rule.to_owned();
    refusal.message = text[end + 3..].to_owned();
    let head = &text[..start];
    if let Some(at) = head.find(": resource \"") {
        let rest = &head[at + ": resource \"".len()..];
        if let Some(close) = rest.find('"') {
            refusal.resource = rest[..close].to_owned();
        }
    }
    refusal
}

/// What a stage came to, as the value `stages.<name>` holds.
///
/// `outcome` is the word; `target` the target; `files` the emitted files by
/// path, each as its text; `warnings` the compile's warnings as text;
/// `refusal` the rule, resource and message of a refusal, or `null`;
/// `changes` the plan's changes, each with `action`, `resource` and
/// `summary`; `unknown` whether the tool could not preview; `condition`
/// the `when` that left the document out, or `null`.
pub fn stage_value(report: &StageReport) -> Value {
    let text = |s: &str| Value::String(s.to_owned());
    let mut map = BTreeMap::new();
    map.insert("name".to_owned(), text(&report.id.name));
    map.insert("kind".to_owned(), text(report.id.kind.as_str()));
    map.insert("target".to_owned(), text(&report.target));
    map.insert("outcome".to_owned(), text(outcome_word(report)));
    let files: BTreeMap<String, Value> = report
        .artifact
        .as_ref()
        .map(|a| {
            a.files
                .iter()
                .map(|(path, content)| (path.clone(), text(content)))
                .collect()
        })
        .unwrap_or_default();
    map.insert("files".to_owned(), Value::Map(files));
    map.insert(
        "warnings".to_owned(),
        Value::List(
            report
                .warnings
                .iter()
                .map(|w| text(&w.to_string()))
                .collect(),
        ),
    );
    let refusal = match (&report.outcome, report.reason) {
        (Outcome::Failed(why), Some(Reason::CompileRefused)) => {
            let parsed = parse_refusal(why);
            Value::Map(
                [
                    ("rule", text(&parsed.rule)),
                    ("resource", text(&parsed.resource)),
                    ("message", text(&parsed.message)),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            )
        }
        _ => Value::Null,
    };
    map.insert("refusal".to_owned(), refusal);
    let (changes, unknown) = match &report.outcome {
        Outcome::Planned(plan) => (plan.changes.as_slice(), plan.unknown),
        _ => (&[][..], false),
    };
    map.insert(
        "changes".to_owned(),
        Value::List(
            changes
                .iter()
                .map(|c| {
                    Value::Map(
                        [
                            ("action", text(c.action.as_str())),
                            ("resource", text(&c.resource)),
                            ("summary", text(&c.summary)),
                        ]
                        .into_iter()
                        .map(|(k, v)| (k.to_owned(), v))
                        .collect(),
                    )
                })
                .collect(),
        ),
    );
    map.insert("unknown".to_owned(), Value::Bool(unknown));
    map.insert(
        "condition".to_owned(),
        match &report.outcome {
            Outcome::LeftOut { condition } => text(condition),
            _ => Value::Null,
        },
    );
    map.insert(
        "error".to_owned(),
        match &report.outcome {
            Outcome::Failed(why) => text(why),
            _ => Value::Null,
        },
    );
    Value::Map(map)
}

/// Checks a case against what its run came to. Every failure is listed,
/// not only the first.
pub(crate) fn check(case: &Case, stages: &[StageReport], data: &Data) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, expect) in &case.expect {
        let Some(report) = stages.iter().find(|s| s.id.name == *name) else {
            let known: Vec<&str> = stages.iter().map(|s| s.id.name.as_str()).collect();
            failures.push(format!(
                "document {name:?}: no such document in this case (the case has: {})",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ));
            continue;
        };
        expectations(name, expect, report, &mut failures);
    }
    if !case.assert.is_empty() {
        let stage_values: BTreeMap<String, Value> = stages
            .iter()
            .map(|s| (s.id.name.clone(), stage_value(s)))
            .collect();
        let scope = Scope::new()
            .with("stages", Value::Map(stage_values))
            .with("data", Value::Map(data.clone()))
            .with_unset_hint(
                "stages",
                "the case has no document called that, or the stage has no such field",
            );
        for assertion in &case.assert {
            match assertion.expr.truth(&scope) {
                Ok(true) => {}
                Ok(false) => failures.push(match &assertion.message {
                    Some(message) => format!("{message} (assert {})", assertion.text),
                    None => format!("assert {}: false", assertion.text),
                }),
                Err(e) => failures.push(format!("assert {}: {e}", assertion.text)),
            }
        }
    }
    failures
}

fn expectations(name: &str, expect: &Expect, report: &StageReport, failures: &mut Vec<String>) {
    let fail = |failures: &mut Vec<String>, what: String| {
        failures.push(format!("document {name:?}: {what}"))
    };
    let word = outcome_word(report);
    if let Some(wanted) = &expect.outcome {
        if wanted != word {
            let detail = match &report.outcome {
                Outcome::Failed(why) => format!(": {why}"),
                Outcome::Deferred { waiting_on, output } => {
                    format!(": waiting for the output {output:?} of {waiting_on}")
                }
                Outcome::LeftOut { condition } => format!(": its when ({condition}) is false"),
                _ => String::new(),
            };
            fail(
                failures,
                format!("expected outcome {wanted}, was {word}{detail}"),
            );
        }
    }
    let refusal = match (&report.outcome, report.reason) {
        (Outcome::Failed(why), Some(Reason::CompileRefused)) => Some(parse_refusal(why)),
        _ => None,
    };
    if let Some(rule) = &expect.rule {
        match &refusal {
            Some(r) if r.rule == *rule => {}
            Some(r) if r.rule.is_empty() => fail(
                failures,
                format!(
                    "expected the rule {rule}, and the refusal carries no rule: {}",
                    r.message
                ),
            ),
            Some(r) => fail(
                failures,
                format!("expected the rule {rule}, was {}", r.rule),
            ),
            None => fail(
                failures,
                format!("expected a refusal under the rule {rule}, and the document was {word}"),
            ),
        }
    }
    if let Some(resource) = &expect.resource {
        match &refusal {
            Some(r) if r.resource == *resource => {}
            Some(r) => fail(
                failures,
                format!(
                    "expected the refusal to name the resource {resource:?}, and it names {:?}",
                    r.resource
                ),
            ),
            None => fail(
                failures,
                format!("expected a refusal naming the resource {resource:?}, and the document was {word}"),
            ),
        }
    }
    if let Some(text) = &expect.message {
        match &refusal {
            Some(r) if r.message.contains(text.as_str()) => {}
            Some(r) => fail(
                failures,
                format!(
                    "expected the refusal to say {text:?}, and it says: {}",
                    r.message
                ),
            ),
            None => fail(
                failures,
                format!("expected a refusal saying {text:?}, and the document was {word}"),
            ),
        }
    }
    for (path, want) in &expect.files {
        let Some(content) = report.artifact.as_ref().and_then(|a| a.files.get(path)) else {
            let known: Vec<&str> = report
                .artifact
                .as_ref()
                .map(|a| a.files.keys().map(String::as_str).collect())
                .unwrap_or_default();
            fail(
                failures,
                format!(
                    "no file {path:?} was emitted ({})",
                    if known.is_empty() {
                        "the document compiled to no file".to_owned()
                    } else {
                        format!("the files are: {}", known.join(", "))
                    }
                ),
            );
            continue;
        };
        for text in &want.contains {
            if !content.contains(text.as_str()) {
                fail(failures, format!("file {path:?} does not contain {text:?}"));
            }
        }
        for text in &want.not_contains {
            if content.contains(text.as_str()) {
                fail(
                    failures,
                    format!("file {path:?} contains {text:?}, and must not"),
                );
            }
        }
        if let Some(equals) = &want.equals {
            if content != equals {
                fail(
                    failures,
                    format!("file {path:?} is not the text expected; it holds:\n{content}"),
                );
            }
        }
        for pattern in &want.matches {
            if !regex_matches(content, pattern) {
                fail(
                    failures,
                    format!("file {path:?} does not match the pattern {pattern:?}"),
                );
            }
        }
    }
    for text in &expect.warnings {
        if !report
            .warnings
            .iter()
            .any(|w| w.to_string().contains(text.as_str()))
        {
            let had: Vec<String> = report.warnings.iter().map(ToString::to_string).collect();
            fail(
                failures,
                format!(
                    "expected a warning saying {text:?}; {}",
                    if had.is_empty() {
                        "there was none".to_owned()
                    } else {
                        format!("the warnings were: {}", had.join("; "))
                    }
                ),
            );
        }
    }
    let changes = match &report.outcome {
        Outcome::Planned(plan) => Some(plan),
        _ => None,
    };
    if !expect.changes.is_empty() || expect.change_count.is_some() {
        match changes {
            None => fail(
                failures,
                format!("expected a plan's changes, and the document was {word}"),
            ),
            Some(plan) => {
                for want in &expect.changes {
                    let found = plan.changes.iter().any(|c| {
                        want.action
                            .as_deref()
                            .is_none_or(|a| a == c.action.as_str())
                            && want.resource.as_deref().is_none_or(|r| r == c.resource)
                    });
                    if !found {
                        let shown: Vec<String> =
                            plan.changes.iter().map(ToString::to_string).collect();
                        fail(
                            failures,
                            format!(
                                "no change {}{}; the plan has: {}",
                                want.action.as_deref().unwrap_or("of any action"),
                                want.resource
                                    .as_deref()
                                    .map(|r| format!(" {r}"))
                                    .unwrap_or_default(),
                                if shown.is_empty() {
                                    "nothing".to_owned()
                                } else {
                                    shown.join("; ")
                                }
                            ),
                        );
                    }
                }
                if let Some(count) = expect.change_count {
                    if plan.changes.len() != count {
                        fail(
                            failures,
                            format!(
                                "expected {}, the plan has {}",
                                plural(count, "change"),
                                plan.changes.len()
                            ),
                        );
                    }
                    if plan.unknown {
                        fail(
                            failures,
                            "the tool could not preview, so the number of changes is not known"
                                .to_owned(),
                        );
                    }
                }
            }
        }
    }
}

/// `1 change`, `2 changes`.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The same patterns as the expression language's `regexMatch`, decided by
/// the same code, so a test and a `when` never disagree about a pattern.
fn regex_matches(text: &str, pattern: &str) -> bool {
    let quote = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"));
    meridian_expr::Expr::parse(&format!("regexMatch({}, {})", quote(text), quote(pattern)))
        .and_then(|e| e.truth(&Scope::new()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_taken_apart_into_rule_resource_and_message() {
        let r = parse_refusal(
            "site.yaml:7: ansible: resource \"web\" [unsupported-param]: the param \"x\" is not one of file's (hint: remove it)",
        );
        assert_eq!(r.rule, "unsupported-param");
        assert_eq!(r.resource, "web");
        assert_eq!(
            r.message,
            "the param \"x\" is not one of file's (hint: remove it)"
        );
    }

    #[test]
    fn a_refusal_without_a_rule_is_its_message() {
        let r = parse_refusal("a ResourceSet cannot be compiled for target \"terraform\"");
        assert_eq!(r.rule, "");
        assert_eq!(r.resource, "");
        assert_eq!(
            r.message,
            "a ResourceSet cannot be compiled for target \"terraform\""
        );
        let r = parse_refusal("fake [not a rule]: text");
        assert_eq!(r.rule, "");
    }

    #[test]
    fn a_target_wide_refusal_has_a_rule_and_no_resource() {
        let r = parse_refusal("puppet [native/unknown-type]: no such type");
        assert_eq!(r.rule, "native/unknown-type");
        assert_eq!(r.resource, "");
        assert_eq!(r.message, "no such type");
    }
}
