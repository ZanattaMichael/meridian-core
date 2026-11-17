//! Interpolation: expressions written inside a value between an opening
//! dollar and two braces, and two closing braces.
//!
//! The spelling sits beside the reference a tool resolves, `${id.path}`, and
//! collides with nothing a tool owns (`{{ }}`, `%{ }`). What is not an
//! interpolation is passed through byte for byte, including a reference and
//! the escape for a literal one.

use std::collections::BTreeSet;

use meridian_secret::Piece;
use meridian_value::{PlacedEscape, SecretRef, Value};

use crate::{Expr, Names, Scope};

/// What opens an interpolation.
pub const OPEN: &str = "${{";
/// What closes one.
pub const CLOSE: &str = "}}";

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Text(String),
    Expr { source: String, expr: Expr },
}

/// A value's text, split into what is literal and what is decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    parts: Vec<Part>,
}

/// Whether `text` holds an interpolation at all. Text that holds none is
/// never touched, so everything written before interpolation existed keeps
/// its exact bytes.
pub fn has_interpolation(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        if !rest[..at].ends_with('$') {
            return true;
        }
        rest = &rest[at + OPEN.len()..];
    }
    false
}

/// Where the expression that starts at the beginning of `body` ends: the
/// offset of its closing braces. Quoted text and nested brackets are stepped
/// over, so a mapping literal or a quoted brace does not end it early.
fn closing(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' | b'[' | b'(' => depth += 1,
            b'}' if depth == 0 => {
                return (bytes.get(i + 1) == Some(&b'}')).then_some(i);
            }
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    None
}

impl Template {
    /// Splits `text` and reads every expression in it. Nothing is decided.
    pub fn parse(text: &str) -> Result<Template, String> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        while let Some(at) = rest.find(OPEN) {
            // An escaped opening is left exactly as written: the plugin's
            // own rule for an escaped reference turns it into the literal.
            if rest[..at].ends_with('$') {
                literal.push_str(&rest[..at + OPEN.len()]);
                rest = &rest[at + OPEN.len()..];
                continue;
            }
            literal.push_str(&rest[..at]);
            let body = &rest[at + OPEN.len()..];
            // When the brackets inside do not balance, the first closing
            // braces end it, and the expression itself says what is wrong.
            let Some(end) = closing(body).or_else(|| body.find(CLOSE)) else {
                return Err(format!(
                    "{text:?} opens an interpolation that is never closed with {CLOSE}"
                ));
            };
            let source = body[..end].trim();
            if source.is_empty() {
                return Err(format!("{text:?} holds an empty interpolation"));
            }
            let expr =
                Expr::parse(source).map_err(|why| format!("in {OPEN} {source} {CLOSE}: {why}"))?;
            if !literal.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut literal)));
            }
            parts.push(Part::Expr {
                source: source.to_owned(),
                expr,
            });
            rest = &body[end + CLOSE.len()..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Template { parts })
    }

    /// Whether the text holds no interpolation, and so is its own value.
    pub fn is_literal(&self) -> bool {
        !self.parts.iter().any(|p| matches!(p, Part::Expr { .. }))
    }

    /// The single expression the text consists of, when it is nothing else.
    /// Its value keeps its type.
    fn only(&self) -> Option<(&str, &Expr)> {
        match self.parts.as_slice() {
            [Part::Expr { source, expr }] => Some((source, expr)),
            _ => None,
        }
    }

    /// Checks every expression's names and functions; see [`Expr::check`].
    pub fn check(&self, names: &Names) -> Result<(), String> {
        for part in &self.parts {
            if let Part::Expr { source, expr } = part {
                expr.check(names)
                    .map_err(|why| format!("in {OPEN} {source} {CLOSE}: {why}"))?;
            }
        }
        Ok(())
    }

    /// Every `(root, key)` the expressions read; see [`Expr::reads`].
    pub fn reads(&self) -> BTreeSet<(String, Option<String>)> {
        let mut out = BTreeSet::new();
        for part in &self.parts {
            if let Part::Expr { expr, .. } = part {
                out.extend(expr.reads());
            }
        }
        out
    }

    /// The expression's own text, when the whole of the text is one
    /// interpolation. A condition may be written this way, and means the
    /// same as the expression written bare.
    pub fn sole_source(&self) -> Option<&str> {
        self.only().map(|(source, _)| source)
    }

    /// [`Template::eval`], for a value a plugin will read afterwards: every
    /// value an interpolation places is made literal, so text that came from
    /// data can never turn into a reference the tool resolves. What the
    /// author wrote around the interpolation is left as written.
    pub fn eval_literal(&self, scope: &Scope) -> Result<Value, String> {
        self.decide(scope, Some(&[]))
    }

    /// [`Template::eval_literal`], for a target that has expression syntax
    /// of its own. Each [`PlacedEscape`] the target declares is applied to
    /// every placed value as well, so text from data cannot open one of the
    /// tool's expressions either: Jinja's `{{` for Ansible, a leading `[` for
    /// an ARM template. An opening that two pieces would form between them,
    /// one of them placed, is made literal too.
    pub fn eval_literal_with(
        &self,
        scope: &Scope,
        escapes: &[PlacedEscape],
    ) -> Result<Value, String> {
        self.decide(scope, Some(escapes))
    }

    /// Decides the text.
    ///
    /// Text that is one interpolation and nothing else becomes that
    /// expression's value with its type: a number stays a number and a list
    /// stays a list. An interpolation inside longer text must be text, a
    /// number or a boolean, and is placed as text.
    pub fn eval(&self, scope: &Scope) -> Result<Value, String> {
        self.decide(scope, None)
    }

    /// With `escapes`, every placed value is made literal: for Meridian's
    /// own `${` always, and for each opening the target declared.
    fn decide(&self, scope: &Scope, escapes: Option<&[PlacedEscape]>) -> Result<Value, String> {
        let literal = escapes.is_some();
        let rules = escapes.unwrap_or(&[]);
        let placed = |value: Value| if literal { escape(value, rules) } else { value };
        if let Some((source, expr)) = self.only() {
            return expr
                .eval(scope)
                .map(placed)
                .map_err(|why| format!("in {OPEN} {source} {CLOSE}: {why}"));
        }
        // Text gathers in `out`. When a secret is placed, what has gathered
        // so far and the secret become pieces, and the whole is a secret.
        let mut out = String::new();
        // Whether the text last added was placed by an interpolation. A
        // placed value that ends in a dollar, followed by text that opens a
        // brace (or the other way round), would join into a reference that
        // neither side wrote, so the join is made literal as well.
        let mut last_placed = false;
        let mut add = |out: &mut String, text: &str, is_placed: bool| -> Result<(), String> {
            if text.is_empty() {
                return Ok(());
            }
            // The other way round there is nothing to add that would help: a
            // placed value that ends in a dollar, directly before a
            // reference the author wrote, is read by every plugin as the
            // escape for a literal, and the reference would be handed over
            // as text. No spelling keeps both the dollar and the reference,
            // so it is refused, and the author is told how to keep them
            // apart.
            if literal
                && last_placed
                && !is_placed
                && out.ends_with('$')
                && text.starts_with(REFERENCE)
            {
                let reference = text.split_inclusive('}').next().unwrap_or(text);
                return Err(format!(
                    "the value placed before {reference} ends in \"$\", and a \"$\" directly \
                     before a reference makes it literal text, so the reference would be \
                     lost; put a space or other text between the two"
                ));
            }
            if literal && (is_placed || last_placed) && out.ends_with('$') && text.starts_with('{')
            {
                out.push('$');
            }
            // The same for an opening of the tool's own: two halves, one of
            // them placed, must not meet as an expression nobody wrote.
            let mut text = text;
            if literal && (is_placed || last_placed) {
                if let Some((k, rule)) = straddles(out, text, rules) {
                    out.truncate(out.len() - k);
                    out.push_str(&rule.written);
                    text = &text[rule.text.len() - k..];
                }
            }
            out.push_str(text);
            last_placed = is_placed;
            Ok(())
        };
        let mut pieces: Vec<(String, SecretRef)> = Vec::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => add(&mut out, text, false)?,
                Part::Expr { source, expr } => {
                    let fail = |why: String| format!("in {OPEN} {source} {CLOSE}: {why}");
                    match expr.eval(scope).map_err(fail)? {
                        Value::String(s) => {
                            let s = if literal {
                                // A value is at the start of the text only
                                // when nothing, not even a secret, came
                                // before it.
                                escape_text(&s, rules, out.is_empty() && pieces.is_empty())
                            } else {
                                s
                            };
                            add(&mut out, &s, true)?;
                        }
                        value @ (Value::Bool(_)
                        | Value::Int(_)
                        | Value::Uint(_)
                        | Value::Float(_)) => add(&mut out, &value.canonical(), true)?,
                        Value::Secret(secret) => pieces.push((std::mem::take(&mut out), secret)),
                        Value::Null => {
                            return Err(fail(
                                "the value is null, which has no text form; give a default \
                                 with ??"
                                    .to_owned(),
                            ))
                        }
                        other => {
                            return Err(fail(format!(
                                "{} cannot be placed inside text; use join(...) or \
                                 toJson(...), or make the interpolation the whole value",
                                other.describe()
                            )))
                        }
                    }
                }
            }
        }
        if pieces.is_empty() {
            return Ok(Value::String(out));
        }
        let mut joined = Vec::with_capacity(pieces.len() * 2 + 1);
        for (before, secret) in &pieces {
            joined.push(Piece::Text(before));
            joined.push(Piece::Secret(*secret));
        }
        joined.push(Piece::Text(&out));
        scope
            .secrets
            .as_deref()
            .ok_or_else(|| {
                "a secret cannot be placed here: this place holds no secrets".to_owned()
            })?
            .join(&joined)
            .map(Value::Secret)
            .map_err(|e| e.to_string())
    }
}

/// What opens a reference the tool resolves, and its escaped, literal form.
const REFERENCE: &str = concat!("$", "{");
const ESCAPED: &str = concat!("$$", "{");

/// Makes every reference opening in a placed value literal, however deep,
/// and every opening of the target's own with it.
fn escape(value: Value, rules: &[PlacedEscape]) -> Value {
    match value {
        Value::String(s) => Value::String(escape_text(&s, rules, true)),
        Value::List(items) => Value::List(items.into_iter().map(|v| escape(v, rules)).collect()),
        Value::Map(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k, escape(v, rules)))
                .collect(),
        ),
        other => other,
    }
}

/// One placed string, made literal. `at_start` says whether the string
/// begins the value it is placed in, which is where an opening that counts
/// only at the start can count.
///
/// The openings are found in one pass, so what one is written as is never
/// read as another: `{%` written as `{{ '{%' }}` must not then have its
/// `{{` escaped again.
fn escape_text(text: &str, rules: &[PlacedEscape], at_start: bool) -> String {
    let text = if text.contains(REFERENCE) {
        text.replace(REFERENCE, ESCAPED)
    } else {
        text.to_owned()
    };
    if rules.is_empty() {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    let mut first = at_start;
    while !rest.is_empty() {
        let rule = rules.iter().find(|r| {
            !r.text.is_empty() && (first || !r.only_at_start) && rest.starts_with(r.text.as_str())
        });
        match rule {
            Some(rule) => {
                out.push_str(&rule.written);
                rest = &rest[rule.text.len()..];
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        first = false;
    }
    out
}

/// Whether an opening is formed across the join of `before` and `after`:
/// `before` ends with the first `k` bytes of a rule's text and `after`
/// begins with the rest. Gives `k` and the rule.
fn straddles<'a>(
    before: &str,
    after: &str,
    rules: &'a [PlacedEscape],
) -> Option<(usize, &'a PlacedEscape)> {
    for rule in rules.iter().filter(|r| !r.only_at_start) {
        let text = rule.text.as_str();
        for k in (1..text.len()).rev() {
            if text.is_char_boundary(k)
                && before.ends_with(&text[..k])
                && after.starts_with(&text[k..])
            {
                return Some((k, rule));
            }
        }
    }
    None
}
