//! One shape for every diagnostic.
//!
//! A small model fails on prose, so every tool that can refuse returns a
//! list of the same object: the rule, where it is, the resource, the
//! message, what is accepted instead when the compiler said, a fix when the
//! linter has one, and the `explain` call that says more. The fields a
//! diagnostic shares with `meridian lint --output json` have the same names
//! and meanings there, so a reader of one reads the other.

use meridian_tooling::check::{Diagnostic, Parsed, Workspace};
use meridian_tooling::syntax::parse_path;
use serde_json::{json, Value};

/// The phrases the compiler and the checker use to list what a target
/// accepts, in their messages.
const ACCEPTED_PHRASES: &[&str] = &[
    "(it takes: ",
    "(it has: ",
    "(it can: ",
    "(loaded targets: ",
    "(accepted: ",
    "compiles for: ",
    "(it takes ",
];

/// The list of accepted values a message carries, if it carries one.
pub(crate) fn accepted_in(message: &str) -> Vec<String> {
    for phrase in ACCEPTED_PHRASES {
        let Some(at) = message.find(phrase) else {
            continue;
        };
        let rest = &message[at + phrase.len()..];
        let list = rest.split([')', ';']).next().unwrap_or("").trim();
        if list.is_empty() || list == "none" {
            return Vec::new();
        }
        return list
            .split(", ")
            .map(|s| s.trim().trim_matches('"').to_owned())
            .filter(|s| !s.is_empty())
            .collect();
    }
    Vec::new()
}

/// The id of the resource a line falls in, if it falls in one.
fn resource_at(file: &Parsed, line: usize) -> Option<String> {
    let doc = file.file.doc_at(line)?;
    let root = file.file.docs.get(doc)?.root.as_ref()?;
    let resources = root.get(&parse_path("spec.resources"))?;
    resources
        .items()
        .iter()
        .find(|item| item.line <= line && line < item.end_line)
        .and_then(|item| item.value.as_ref())
        .and_then(|v| v.text("id"))
        .map(str::to_owned)
}

/// A diagnostic as the server returns it. `target` is the document's
/// target when it is known, for the `explain` call.
pub fn diagnostic(workspace: &Workspace, d: &Diagnostic) -> Value {
    let file = workspace.file(&d.file);
    let resource = file.and_then(|f| resource_at(f, d.span.start.line));
    let target = file
        .and_then(|f| {
            f.file
                .doc_at(d.span.start.line)
                .and_then(|i| f.docs.iter().find(|p| p.index == i))
        })
        .map(|p| p.doc.target().to_owned())
        .filter(|t| !t.is_empty() && !meridian_expr::has_interpolation(t));
    let explain = match &target {
        Some(target) => format!("explain(rule='{}', target='{target}')", d.code),
        None => format!("explain(rule='{}')", d.code),
    };
    let mut out = json!({
        "file": d.file,
        "line": d.span.start.line + 1,
        "column": d.span.start.col + 1,
        "endLine": d.span.end.line + 1,
        "endColumn": d.span.end.col + 1,
        "severity": d.severity.as_str(),
        "code": d.code,
        "source": d.source,
        "message": d.message,
        "fixable": d.fix.is_some(),
    });
    let object = out.as_object_mut().expect("made as an object");
    if let Some(resource) = resource {
        object.insert("resource".to_owned(), resource.into());
    }
    let accepted = accepted_in(&d.message);
    if !accepted.is_empty() {
        object.insert("accepted".to_owned(), accepted.into());
    }
    if let Some(fix) = &d.fix {
        object.insert(
            "fix".to_owned(),
            json!({
                "title": fix.title,
                "line": fix.span.start.line + 1,
                "column": fix.span.start.col + 1,
                "endLine": fix.span.end.line + 1,
                "endColumn": fix.span.end.col + 1,
                "replace": fix.text,
            }),
        );
    }
    object.insert("explain".to_owned(), explain.into());
    out
}

/// How many of `diagnostics` are errors, warnings and notes.
pub(crate) fn summary(diagnostics: &[Diagnostic]) -> Value {
    use meridian_tooling::check::Severity;
    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    json!({
        "errors": errors,
        "warnings": warnings,
        "notes": diagnostics.len() - errors - warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_values_are_read_from_the_message() {
        assert_eq!(
            accepted_in("resource type \"service\" does not take state \"restarted\" on target \"puppet\" (it takes: running, stopped)"),
            vec!["running", "stopped"]
        );
        assert_eq!(
            accepted_in("target \"ansible\" compiles for: 2.14, 2.15"),
            vec!["2.14", "2.15"]
        );
        assert!(accepted_in("resource type \"x\" takes no state on target \"y\"").is_empty());
        assert!(accepted_in("param \"a\" does not take \"b\" (it takes: none)").is_empty());
    }
}
