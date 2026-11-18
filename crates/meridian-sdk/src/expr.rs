//! The one piece of expression syntax a document can carry into a plugin: a
//! reference to something only the tool can know, written `${resource.path}`.
//!
//! An infrastructure document uses it for outputs (`${web_vm.public_ip}`) and
//! for wiring one resource to another. Each tool spells such a reference
//! differently, so the plugin rewrites it; this module only finds them.
//!
//! `$${` is the escape for a literal `${`.

use crate::rules;
use std::collections::BTreeMap;

use crate::{CompileError, Resource, ResourceGraph, Value};

/// One piece of a string that may hold references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Literal text, with any `$${` already turned back into `${`.
    Text(String),
    /// What stood between `${` and its closing brace.
    Expr(String),
}

/// Splits a string into literal text and `${...}` expressions.
///
/// Braces inside an expression are balanced, so `${merge({a = 1}, x)}` is one
/// expression. An unclosed `${` is an error, since no tool would read it the
/// way the author meant.
pub fn parse(text: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        // `$${` is a literal `${`.
        if rest[..at].ends_with('$') {
            literal.push_str(&rest[..at - 1]);
            literal.push_str("${");
            rest = &rest[at + 2..];
            continue;
        }
        literal.push_str(&rest[..at]);
        let body = &rest[at + 2..];
        let mut depth = 1usize;
        let mut end = None;
        for (i, c) in body.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            return Err(format!("{text:?} opens a ${{ that is never closed"));
        };
        if !literal.is_empty() {
            parts.push(Part::Text(std::mem::take(&mut literal)));
        }
        parts.push(Part::Expr(body[..end].trim().to_owned()));
        rest = &body[end + 1..];
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        parts.push(Part::Text(literal));
    }
    Ok(parts)
}

/// Whether a string holds any `${...}` expression at all.
pub fn has_expr(text: &str) -> bool {
    parse(text).is_ok_and(|parts| parts.iter().any(|p| matches!(p, Part::Expr(_))))
}

/// Splits an expression into its leading name and what follows it:
/// `web_vm.public_ip` is `("web_vm", ".public_ip")`, `subnets[0].id` is
/// `("subnets", "[0].id")`. The name is empty when the expression does not
/// start with one.
pub fn head(expr: &str) -> (&str, &str) {
    let end = expr
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
        .map_or(expr.len(), |(i, _)| i);
    expr.split_at(end)
}

/// Whether an expression is nothing but a path: a name followed by `.field`
/// and `[index]` steps. Anything else, such as a function call, is the tool's
/// own expression language.
pub fn is_path(expr: &str) -> bool {
    let (name, rest) = head(expr);
    !name.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-[]\"'*".contains(c))
}

/// Resolves the references of a document whose resources are plain
/// declarations: `${id.param}` becomes the value the document gives that param
/// on resource `id`.
///
/// This is for a target with nothing to read at apply time, such as a
/// configuration management tool: all a resource has is what the author wrote.
/// So a reference is settled at compile time, and anything that cannot be is
/// refused with the rule `invalid-reference`: a resource the document does not
/// declare, a param that resource does not set, a bare `${id}`, a deeper path,
/// or an expression that is not a path at all. `$${` comes out as a literal
/// `${`.
///
/// A reference that is a param's whole value keeps the type of what it reads.
/// Inside longer text it must read a scalar. References may chain; a chain
/// that comes back to itself is refused.
pub fn resolve_declared(
    graph: &ResourceGraph,
    target: &str,
) -> Result<ResourceGraph, CompileError> {
    let declared: BTreeMap<&str, &Resource> =
        graph.resources.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut resolved = graph.clone();
    for resource in &mut resolved.resources {
        let id = resource.id.clone();
        let pos = resource.pos.clone();
        for (name, value) in &mut resource.params {
            let mut trail = vec![(id.clone(), name.clone())];
            *value = resolve_value(value, &declared, &mut trail).map_err(|msg| CompileError {
                target: target.to_owned(),
                resource: id.clone(),
                rule: rules::INVALID_REFERENCE.name.to_owned(),
                hint: rules::INVALID_REFERENCE.hint.to_owned(),
                msg: format!("param {name:?}: {msg}"),
                pos: pos.clone(),
            })?;
        }
    }
    Ok(resolved)
}

fn resolve_value(
    value: &Value,
    declared: &BTreeMap<&str, &Resource>,
    trail: &mut Vec<(String, String)>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => resolve_text(text, declared, trail)?,
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| resolve_value(item, declared, trail))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(key, item)| Ok((key.clone(), resolve_value(item, declared, trail)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn resolve_text(
    text: &str,
    declared: &BTreeMap<&str, &Resource>,
    trail: &mut Vec<(String, String)>,
) -> Result<Value, String> {
    let parts = parse(text)?;
    if let [Part::Expr(expr)] = parts.as_slice() {
        return read(expr, declared, trail);
    }
    let mut out = String::new();
    for part in &parts {
        match part {
            Part::Text(literal) => out.push_str(literal),
            Part::Expr(expr) => match read(expr, declared, trail)? {
                Value::String(s) => out.push_str(&s),
                Value::Bool(b) => out.push_str(&b.to_string()),
                Value::Int(i) => out.push_str(&i.to_string()),
                Value::Uint(u) => out.push_str(&u.to_string()),
                Value::Float(f) => out.push_str(&f.to_string()),
                // A plugin is never sent a secret this way; if one were here it
                // would still be written as its mask.
                Value::Secret(_) => out.push_str(meridian_value::SENSITIVE),
                Value::Null | Value::List(_) | Value::Map(_) => {
                    return Err(format!(
                        "${{{expr}}} is not a single value, so it cannot be placed inside \
                         text; use it as the whole value of a param"
                    ))
                }
            },
        }
    }
    Ok(Value::String(out))
}

/// The value one `${...}` stands for.
fn read(
    expr: &str,
    declared: &BTreeMap<&str, &Resource>,
    trail: &mut Vec<(String, String)>,
) -> Result<Value, String> {
    const LITERAL: &str = "for a literal ${ write $${";
    if !is_path(expr) {
        return Err(format!(
            "${{{expr}}} is not a reference to a resource, and this target has no \
             expression language; {LITERAL}"
        ));
    }
    let (id, rest) = head(expr);
    let Some(resource) = declared.get(id) else {
        return Err(format!(
            "${{{expr}}} refers to a resource {id:?}, which this document does not \
             declare; {LITERAL}"
        ));
    };
    let set: Vec<&str> = resource.params.keys().map(String::as_str).collect();
    let param = rest.strip_prefix('.').unwrap_or("");
    if param.is_empty() {
        return Err(format!(
            "${{{expr}}} names a resource but not which of its params to read; \
             write ${{{id}.<param>}} (it sets: {})",
            set.join(", ")
        ));
    }
    if param.contains(['.', '[']) {
        return Err(format!(
            "${{{expr}}} reads inside a param, which this target cannot do; \
             a reference is ${{{id}.<param>}} and nothing deeper"
        ));
    }
    let Some(value) = resource.params.get(param) else {
        return Err(format!(
            "${{{expr}}} reads param {param:?}, which resource {id:?} does not set \
             (it sets: {}); on this target a reference can only read what the \
             document itself declares",
            set.join(", ")
        ));
    };
    let step = (id.to_owned(), param.to_owned());
    if trail.contains(&step) {
        let mut chain: Vec<String> = trail.iter().map(|(r, p)| format!("{r}.{p}")).collect();
        chain.push(format!("{id}.{param}"));
        return Err(format!(
            "${{{expr}}} refers back to itself: {}",
            chain.join(" -> ")
        ));
    }
    trail.push(step);
    let value = resolve_value(value, declared, trail);
    trail.pop();
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Part {
        Part::Text(s.to_owned())
    }
    fn expr(s: &str) -> Part {
        Part::Expr(s.to_owned())
    }

    #[test]
    fn references_are_found_among_literal_text() {
        assert_eq!(parse("plain").unwrap(), [text("plain")]);
        assert_eq!(parse("${vm.ip}").unwrap(), [expr("vm.ip")]);
        assert_eq!(
            parse("http://${ vm.ip }:${port}/x").unwrap(),
            [
                text("http://"),
                expr("vm.ip"),
                text(":"),
                expr("port"),
                text("/x")
            ]
        );
        assert_eq!(parse("").unwrap(), []);
    }

    #[test]
    fn a_doubled_dollar_is_a_literal_and_braces_balance() {
        assert_eq!(parse("say $${NAME} $5").unwrap(), [text("say ${NAME} $5")]);
        assert_eq!(
            parse("${merge({a = 1}, x)}").unwrap(),
            [expr("merge({a = 1}, x)")]
        );
        assert!(parse("${vm.ip").unwrap_err().contains("never closed"));
        assert!(has_expr("a ${b}"));
        assert!(!has_expr("a $${b}"));
    }

    #[test]
    fn an_expression_splits_into_its_name_and_path() {
        assert_eq!(head("web_vm.public_ip"), ("web_vm", ".public_ip"));
        assert_eq!(head("subnets[0].id"), ("subnets", "[0].id"));
        assert_eq!(head("vm"), ("vm", ""));
        assert_eq!(head("(1 + 2)"), ("", "(1 + 2)"));
        assert!(is_path("web_vm.public_ip"));
        assert!(is_path("lb.status.ingress[0].ip"));
        assert!(!is_path("upper(vm.name)"));
        assert!(!is_path("a ? b : c"));
    }
}
