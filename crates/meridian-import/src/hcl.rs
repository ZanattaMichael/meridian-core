//! Reading HashiCorp's configuration language (HCL) into the shape its JSON
//! syntax has.
//!
//! Terraform, OpenTofu and Packer each accept a configuration written either
//! in HCL or in JSON, and the JSON form is what Meridian's targets for them
//! write. So an importer does not need to understand HCL's expressions, only
//! to restate them: a literal stays a literal, a string stays the template it
//! was, a list or a map is restated item by item, and any other expression
//! (a reference, a function call, a conditional) becomes the string
//! `${expression}`, which is exactly how the JSON syntax spells it.
//!
//! The reader therefore parses structure (blocks, attributes, lists, maps,
//! strings) and only finds the extent of everything else. It keeps the line
//! of every block and attribute so a note can point at the source.

use std::collections::BTreeMap;

use meridian_sdk::{ImportError, Value};

/// What a block holds: attributes and nested blocks, each in the order
/// written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub attributes: Vec<Attribute>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub expr: Expr,
    /// 1-based.
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub r#type: String,
    pub labels: Vec<String>,
    pub body: Body,
    /// 1-based.
    pub line: usize,
}

/// An attribute's value, parsed as far as the JSON syntax needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// `null`, a boolean or a number.
    Literal(Value),
    /// A quoted string or a heredoc, as the template text the JSON syntax
    /// would hold: escapes decoded, `${...}` left as written.
    Template {
        text: String,
        /// Whether the template uses a `%{ ... }` directive.
        directive: bool,
    },
    /// `[a, b]`.
    Tuple(Vec<Expr>),
    /// `{ a = b }`, in the order written.
    Object(Vec<(String, Expr)>),
    /// Any other expression, as its source text with comments removed and
    /// line breaks folded away.
    Raw(String),
}

impl Expr {
    /// The value the JSON syntax would hold for this expression.
    pub fn to_json(&self) -> Value {
        match self {
            Expr::Literal(v) => v.clone(),
            Expr::Template { text, .. } => Value::String(text.clone()),
            Expr::Tuple(items) => Value::List(items.iter().map(Expr::to_json).collect()),
            Expr::Object(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
            Expr::Raw(text) => Value::String(format!("${{{text}}}")),
        }
    }

    /// The value the JSON syntax would hold where the language wants a name
    /// and not a value, as in `depends_on`, `provider` or a variable's
    /// `type`: there the expression is written as plain text, with no `${}`.
    pub fn to_static_json(&self) -> Value {
        match self {
            Expr::Raw(text) => Value::String(text.clone()),
            Expr::Tuple(items) => Value::List(items.iter().map(Expr::to_static_json).collect()),
            Expr::Object(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_static_json()))
                    .collect(),
            ),
            other => other.to_json(),
        }
    }

    /// Whether any template in the expression uses a `%{ ... }` directive.
    pub fn has_directive(&self) -> bool {
        match self {
            Expr::Template { directive, .. } => *directive,
            Expr::Tuple(items) => items.iter().any(Expr::has_directive),
            Expr::Object(entries) => entries.iter().any(|(_, v)| v.has_directive()),
            Expr::Literal(_) | Expr::Raw(_) => false,
        }
    }
}

impl Body {
    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.name == name)
    }

    /// The body as the JSON syntax would hold it. A nested block becomes a
    /// map under its type and labels; blocks that share a type and labels
    /// become a list of maps, in the order written. `is_static` says, by
    /// path from this body (`lifecycle.ignore_changes`), which attributes
    /// hold names and not values.
    pub fn to_json(&self, is_static: &dyn Fn(&str) -> bool) -> BTreeMap<String, Value> {
        self.to_json_at("", is_static)
    }

    fn to_json_at(&self, path: &str, is_static: &dyn Fn(&str) -> bool) -> BTreeMap<String, Value> {
        let join = |name: &str| {
            if path.is_empty() {
                name.to_owned()
            } else {
                format!("{path}.{name}")
            }
        };
        let mut out = BTreeMap::new();
        for a in &self.attributes {
            let value = if is_static(&join(&a.name)) {
                a.expr.to_static_json()
            } else {
                a.expr.to_json()
            };
            out.insert(a.name.clone(), value);
        }
        for b in &self.blocks {
            let body = Value::Map(b.body.to_json_at(&join(&b.r#type), is_static));
            let mut level = &mut out;
            let mut key = b.r#type.clone();
            for label in &b.labels {
                let next = level
                    .entry(key)
                    .or_insert_with(|| Value::Map(BTreeMap::new()));
                let Value::Map(next) = next else {
                    // An attribute and a block share a name; the block wins,
                    // as it is the later of the two.
                    *next = Value::Map(BTreeMap::new());
                    let Value::Map(next) = next else {
                        unreachable!()
                    };
                    level = next;
                    key = label.clone();
                    continue;
                };
                level = next;
                key = label.clone();
            }
            match level.get_mut(&key) {
                Some(Value::List(items)) => items.push(body),
                Some(existing @ Value::Map(_)) => {
                    let first = std::mem::replace(existing, Value::Null);
                    *existing = Value::List(vec![first, body]);
                }
                _ => {
                    level.insert(key, body);
                }
            }
        }
        out
    }
}

/// Parses an HCL file.
pub fn parse(file: &str, text: &str) -> Result<Body, ImportError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let src: Vec<char> = text.chars().collect();
    let mut line_starts = vec![0];
    for (i, c) in src.iter().enumerate() {
        if *c == '\n' {
            line_starts.push(i + 1);
        }
    }
    let mut p = Parser {
        src,
        i: 0,
        file,
        line_starts,
        depth: 0,
    };
    p.body(false)
}

/// Where an expression sits, which decides what ends it.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    /// An attribute's value: ends at the end of the line, or at the `}` of a
    /// one-line block.
    Attribute,
    /// An item of `[...]`: ends at `,` or `]`.
    Item,
    /// A value of `{...}`: ends at `,`, the end of the line, or `}`.
    Entry,
}

struct Parser<'a> {
    src: Vec<char>,
    i: usize,
    file: &'a str,
    line_starts: Vec<usize>,
    depth: usize,
}

/// Nesting deeper than this is refused instead of risking the stack.
const MAX_DEPTH: usize = 128;

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

impl Parser<'_> {
    fn line(&self, at: usize) -> usize {
        match self.line_starts.binary_search(&at) {
            Ok(n) => n + 1,
            Err(n) => n,
        }
    }

    fn err<T>(&self, at: usize, msg: impl Into<String>) -> Result<T, ImportError> {
        Err(ImportError::at(self.file, self.line(at), msg))
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.i).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.src.get(self.i + ahead).copied()
    }

    fn starts(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(n, c)| self.peek_at(n) == Some(c))
    }

    /// Skips blanks and comments, and line breaks too when `newlines`.
    fn skip(&mut self, newlines: bool) -> Result<(), ImportError> {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => self.i += 1,
                Some('\n') if newlines => self.i += 1,
                Some('#') => self.skip_line(),
                Some('/') if self.peek_at(1) == Some('/') => self.skip_line(),
                Some('/') if self.peek_at(1) == Some('*') => {
                    let start = self.i;
                    self.i += 2;
                    while !self.starts("*/") {
                        if self.peek().is_none() {
                            return self.err(start, "a /* comment is never closed");
                        }
                        self.i += 1;
                    }
                    self.i += 2;
                }
                _ => return Ok(()),
            }
        }
    }

    fn skip_line(&mut self) {
        while !matches!(self.peek(), None | Some('\n')) {
            self.i += 1;
        }
    }

    fn ident(&mut self) -> Option<String> {
        if !self.peek().is_some_and(ident_start) {
            return None;
        }
        let start = self.i;
        while self.peek().is_some_and(ident_part) {
            self.i += 1;
        }
        Some(self.src[start..self.i].iter().collect())
    }

    fn body(&mut self, braced: bool) -> Result<Body, ImportError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err(self.i, "the configuration is nested too deeply");
        }
        let opened = self.i;
        let mut body = Body::default();
        loop {
            self.skip(true)?;
            match self.peek() {
                None if braced => return self.err(opened, "a block's { is never closed"),
                None => break,
                Some('}') if braced => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            let at = self.i;
            let Some(name) = self.ident() else {
                return self.err(
                    at,
                    format!(
                        "expected an attribute or a block, found {:?}",
                        self.peek().unwrap_or(' ')
                    ),
                );
            };
            self.skip(false)?;
            if self.peek() == Some('=') && self.peek_at(1) != Some('=') {
                self.i += 1;
                let expr = self.expr(Ctx::Attribute)?;
                body.attributes.push(Attribute {
                    name,
                    expr,
                    line: self.line(at),
                });
                continue;
            }
            let mut labels = Vec::new();
            loop {
                self.skip(false)?;
                match self.peek() {
                    Some('{') => {
                        self.i += 1;
                        break;
                    }
                    Some('"') => {
                        let (text, _) = self.quoted()?;
                        labels.push(text);
                    }
                    Some(c) if ident_start(c) => labels.push(self.ident().unwrap_or_default()),
                    _ => return self.err(at, format!("expected = or {{ after {name:?}")),
                }
            }
            let inner = self.body(true)?;
            body.blocks.push(Block {
                r#type: name,
                labels,
                body: inner,
                line: self.line(at),
            });
        }
        self.depth -= 1;
        Ok(body)
    }

    /// Whether the cursor stands where an expression in `ctx` may end.
    fn at_end(&mut self, ctx: Ctx) -> Result<bool, ImportError> {
        self.skip(ctx == Ctx::Item)?;
        Ok(match (ctx, self.peek()) {
            (_, None) => true,
            (Ctx::Attribute, Some(c)) => c == '\n' || c == '}',
            (Ctx::Item, Some(c)) => c == ',' || c == ']',
            (Ctx::Entry, Some(c)) => c == ',' || c == '\n' || c == '}',
        })
    }

    fn expr(&mut self, ctx: Ctx) -> Result<Expr, ImportError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err(self.i, "the configuration is nested too deeply");
        }
        self.skip(ctx == Ctx::Item)?;
        let start = self.i;
        let expr = match self.structured()? {
            Some(expr) if self.at_end(ctx)? => expr,
            _ => {
                self.i = start;
                self.raw(ctx)?
            }
        };
        self.depth -= 1;
        Ok(expr)
    }

    /// Whether the next word is the `for` that opens a for expression.
    fn opens_for(&mut self) -> Result<bool, ImportError> {
        let back = self.i;
        self.skip(true)?;
        let found = self.ident().as_deref() == Some("for");
        self.i = back;
        Ok(found)
    }

    /// Parses a literal, a string, a list or a map, or returns `None` when
    /// what stands here is some other expression.
    fn structured(&mut self) -> Result<Option<Expr>, ImportError> {
        match self.peek() {
            Some('"') => {
                let (text, directive) = self.quoted()?;
                Ok(Some(Expr::Template { text, directive }))
            }
            Some('<') if self.peek_at(1) == Some('<') => {
                let (text, directive) = self.heredoc()?;
                Ok(Some(Expr::Template { text, directive }))
            }
            Some(c) if c.is_ascii_digit() => Ok(self.number(false)),
            Some('-') if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => {
                self.i += 1;
                Ok(self.number(true))
            }
            Some(c) if ident_start(c) => {
                let word = self.ident().unwrap_or_default();
                Ok(match word.as_str() {
                    "true" => Some(Expr::Literal(Value::Bool(true))),
                    "false" => Some(Expr::Literal(Value::Bool(false))),
                    "null" => Some(Expr::Literal(Value::Null)),
                    _ => None,
                })
            }
            Some('[') => {
                self.i += 1;
                if self.opens_for()? {
                    return Ok(None);
                }
                let mut items = Vec::new();
                loop {
                    self.skip(true)?;
                    match self.peek() {
                        Some(']') => {
                            self.i += 1;
                            return Ok(Some(Expr::Tuple(items)));
                        }
                        None => return Ok(None),
                        _ => {}
                    }
                    items.push(self.expr(Ctx::Item)?);
                    self.skip(true)?;
                    if self.peek() == Some(',') {
                        self.i += 1;
                    }
                }
            }
            Some('{') => {
                self.i += 1;
                if self.opens_for()? {
                    return Ok(None);
                }
                let mut entries = Vec::new();
                loop {
                    self.skip(true)?;
                    let key = match self.peek() {
                        Some('}') => {
                            self.i += 1;
                            return Ok(Some(Expr::Object(entries)));
                        }
                        Some('"') => {
                            let (text, _) = self.quoted()?;
                            // A key that is itself computed has no JSON form.
                            if text.contains("${") {
                                return Ok(None);
                            }
                            text
                        }
                        Some(c) if ident_start(c) || c.is_ascii_digit() => {
                            let start = self.i;
                            while self.peek().is_some_and(|c| ident_part(c) || c == '.') {
                                self.i += 1;
                            }
                            self.src[start..self.i].iter().collect()
                        }
                        _ => return Ok(None),
                    };
                    self.skip(false)?;
                    match self.peek() {
                        Some('=')
                            if self.peek_at(1) != Some('=') && self.peek_at(1) != Some('>') =>
                        {
                            self.i += 1
                        }
                        Some(':') => self.i += 1,
                        _ => return Ok(None),
                    }
                    self.skip(false)?;
                    let value = self.expr(Ctx::Entry)?;
                    entries.push((key, value));
                    self.skip(false)?;
                    if self.peek() == Some(',') {
                        self.i += 1;
                    }
                }
            }
            _ => Ok(None),
        }
    }

    fn number(&mut self, negative: bool) -> Option<Expr> {
        let start = self.i;
        let digits = |p: &mut Self| {
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
        };
        digits(self);
        let mut whole = true;
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            whole = false;
            self.i += 1;
            digits(self);
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let back = self.i;
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                whole = false;
                digits(self);
            } else {
                self.i = back;
            }
        }
        let text: String = self.src[start..self.i].iter().collect();
        let text = if negative { format!("-{text}") } else { text };
        if whole {
            if let Ok(i) = text.parse::<i64>() {
                return Some(Expr::Literal(Value::Int(i)));
            }
            if let Ok(u) = text.parse::<u64>() {
                return Some(Expr::Literal(Value::Uint(u)));
            }
        }
        text.parse::<f64>()
            .ok()
            .map(|f| Expr::Literal(Value::Float(f)))
    }

    /// Reads a quoted string, leaving the cursor after its closing quote.
    /// Returns the template text and whether it uses a directive.
    fn quoted(&mut self) -> Result<(String, bool), ImportError> {
        let start = self.i;
        self.i += 1;
        let mut out = String::new();
        let mut directive = false;
        loop {
            let Some(c) = self.peek() else {
                return self.err(start, "a string's opening quote is never closed");
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok((out, directive));
                }
                '\n' => return self.err(start, "a string's opening quote is never closed"),
                '\\' => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return self.err(start, "a string's opening quote is never closed");
                    };
                    self.i += 1;
                    match e {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' | 'U' => {
                            let len = if e == 'u' { 4 } else { 8 };
                            let hex: String = (0..len).filter_map(|n| self.peek_at(n)).collect();
                            let decoded = (hex.chars().count() == len)
                                .then(|| u32::from_str_radix(&hex, 16).ok())
                                .flatten()
                                .and_then(char::from_u32);
                            match decoded {
                                Some(ch) => {
                                    out.push(ch);
                                    self.i += len;
                                }
                                None => {
                                    return self.err(self.i, "an invalid \\u escape in a string")
                                }
                            }
                        }
                        other => out.push(other),
                    }
                }
                '$' | '%' if self.peek_at(1) == Some('{') => {
                    // A doubled sign is the template's own escape for a
                    // literal one, and it stays as written.
                    if out.ends_with(c) && self.src.get(self.i.wrapping_sub(1)) == Some(&c) {
                        out.push(c);
                        out.push('{');
                        self.i += 2;
                        continue;
                    }
                    if c == '%' {
                        directive = true;
                    }
                    let end = self.interpolation_end(self.i + 2)?;
                    out.extend(&self.src[self.i..end]);
                    self.i = end;
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Finds the end of a `${ ... }` or `%{ ... }` whose inside starts at
    /// `from`: the index just past its closing brace.
    fn interpolation_end(&self, from: usize) -> Result<usize, ImportError> {
        let mut depth = 1usize;
        let mut i = from;
        while let Some(&c) = self.src.get(i) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i + 1);
                    }
                }
                '"' => i = self.string_end(i)? - 1,
                _ => {}
            }
            i += 1;
        }
        self.err(from, "a ${ in a string is never closed")
    }

    /// Finds the end of the quoted string that opens at `from`: the index
    /// just past its closing quote.
    fn string_end(&self, from: usize) -> Result<usize, ImportError> {
        let mut i = from + 1;
        while let Some(&c) = self.src.get(i) {
            match c {
                '"' => return Ok(i + 1),
                '\\' => i += 1,
                '$' | '%'
                    if self.src.get(i + 1) == Some(&'{')
                        && self.src.get(i.wrapping_sub(1)) != Some(&c) =>
                {
                    i = self.interpolation_end(i + 2)? - 1;
                }
                '\n' => break,
                _ => {}
            }
            i += 1;
        }
        self.err(from, "a string's opening quote is never closed")
    }

    /// Reads a heredoc, leaving the cursor after its closing word.
    fn heredoc(&mut self) -> Result<(String, bool), ImportError> {
        let start = self.i;
        self.i += 2;
        let indented = self.peek() == Some('-');
        if indented {
            self.i += 1;
        }
        let Some(word) = self.ident() else {
            return self.err(start, "a heredoc needs a word after <<");
        };
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.i += 1;
        }
        if self.peek() != Some('\n') {
            return self.err(
                start,
                "a heredoc's text starts on the line after its opening word",
            );
        }
        self.i += 1;
        let mut lines: Vec<String> = Vec::new();
        loop {
            if self.peek().is_none() {
                return self.err(
                    start,
                    format!("the heredoc opened with {word} is never closed"),
                );
            }
            let line_start = self.i;
            self.skip_line();
            let line: String = self.src[line_start..self.i].iter().collect();
            let line = line.strip_suffix('\r').unwrap_or(&line).to_owned();
            if line.trim() == word {
                break;
            }
            lines.push(line);
            if self.peek() == Some('\n') {
                self.i += 1;
            }
        }
        if indented {
            let margin = lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start_matches([' ', '\t']).len())
                .min()
                .unwrap_or(0);
            for line in &mut lines {
                let cut = margin.min(line.len() - line.trim_start_matches([' ', '\t']).len());
                line.drain(..cut);
            }
        }
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        let directive = text
            .match_indices("%{")
            .any(|(at, _)| !text[..at].ends_with('%'));
        Ok((text, directive))
    }

    /// Reads an expression this reader does not take apart, as its source
    /// text: comments removed, line breaks folded away.
    fn raw(&mut self, ctx: Ctx) -> Result<Expr, ImportError> {
        let start = self.i;
        // The open brackets, and for a `{` whether it is a for expression.
        let mut open: Vec<(char, bool)> = Vec::new();
        let mut out = String::new();
        // A line break inside a map separates its entries; elsewhere inside
        // brackets it means nothing.
        let mut pending: Option<bool> = None;
        loop {
            self.skip(false)?;
            let Some(c) = self.peek() else { break };
            if open.is_empty() {
                let ends = match ctx {
                    Ctx::Attribute => c == '\n' || c == '}',
                    Ctx::Item => c == ',' || c == ']',
                    Ctx::Entry => c == ',' || c == '\n' || c == '}',
                };
                if ends {
                    break;
                }
            }
            if c == '\n' {
                self.i += 1;
                let in_map = matches!(open.last(), Some(('{', false)));
                pending = Some(pending.unwrap_or(false) || in_map);
                continue;
            }
            if let Some(separates) = pending.take() {
                let last = out.trim_end().chars().last();
                if separates && c != '}' && !matches!(last, Some('{' | ',') | None) {
                    out.truncate(out.trim_end().len());
                    out.push_str(", ");
                } else if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            } else if self.i > start
                && matches!(self.src[self.i - 1], ' ' | '\t' | '\r' | '/' | '\n')
                && !out.ends_with(' ')
                && !out.is_empty()
            {
                // Blanks and comments between two tokens fold to one space.
                out.push(' ');
            }
            match c {
                '"' => {
                    let end = self.string_end(self.i)?;
                    out.extend(&self.src[self.i..end]);
                    self.i = end;
                }
                '<' if self.peek_at(1) == Some('<')
                    && self.peek_at(2).is_some_and(|c| c == '-' || ident_start(c)) =>
                {
                    return self.err(
                        self.i,
                        "a heredoc inside a larger expression cannot be imported; assign the \
                         heredoc to a local value and refer to that",
                    );
                }
                '(' | '[' => {
                    open.push((c, false));
                    out.push(c);
                    self.i += 1;
                }
                '{' => {
                    self.i += 1;
                    let is_for = self.opens_for()?;
                    open.push(('{', is_for));
                    out.push('{');
                }
                ')' | ']' | '}' => {
                    let wanted = match c {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    match open.pop() {
                        Some((opened, _)) if opened == wanted => {}
                        _ => return self.err(self.i, format!("unexpected {c:?} in an expression")),
                    }
                    out.push(c);
                    self.i += 1;
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        if !open.is_empty() {
            return self.err(start, "a bracket in an expression is never closed");
        }
        let text = out.trim().to_owned();
        if text.is_empty() {
            return self.err(start, "expected a value");
        }
        Ok(Expr::Raw(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::decode_value;

    fn json(text: &str) -> Value {
        let body = parse("test.tf", text).unwrap_or_else(|e| panic!("{e}"));
        Value::Map(body.to_json(&|path| path.ends_with("depends_on")))
    }

    fn want(json: &str) -> Value {
        decode_value(json.as_bytes()).unwrap()
    }

    #[test]
    fn literals_strings_lists_and_maps_keep_their_shape() {
        let got = json(
            r#"
            a = 1
            b = -2.5
            c = true
            d = null
            e = "plain"
            f = ["x", 2, false]
            g = { name = "web", "quoted key" = 1, nested = { deep = [] } }
            h = {
              one : 1,
              two = 2
              three = 3,
            }
            "#,
        );
        assert_eq!(
            got,
            want(
                r#"{"a":1,"b":-2.5,"c":true,"d":null,"e":"plain","f":["x",2,false],
                "g":{"name":"web","quoted key":1,"nested":{"deep":[]}},
                "h":{"one":1,"two":2,"three":3}}"#
            )
        );
    }

    #[test]
    fn any_other_expression_becomes_an_interpolation() {
        let got = json(
            r#"
            ref    = aws_instance.web.id
            call   = cidrsubnet(var.cidr, 8, 1)
            cond   = var.prod ? 3 : 1
            index  = var.zones[0]
            splat  = aws_instance.web[*].id
            sum    = 1 + 2
            list   = [var.a, "b-${var.c}", upper("x")]
            map    = { id = aws_vpc.main.id, n = 1 }
            concat = concat(["a"], var.more)
            each   = [for s in var.list : upper(s) if s != ""]
            pairs  = { for k, v in var.m : k => v }
            "#,
        );
        assert_eq!(
            got,
            want(
                r#"{"ref":"${aws_instance.web.id}","call":"${cidrsubnet(var.cidr, 8, 1)}",
                "cond":"${var.prod ? 3 : 1}","index":"${var.zones[0]}",
                "splat":"${aws_instance.web[*].id}","sum":"${1 + 2}",
                "list":["${var.a}","b-${var.c}","${upper(\"x\")}"],
                "map":{"id":"${aws_vpc.main.id}","n":1},
                "concat":"${concat([\"a\"], var.more)}",
                "each":"${[for s in var.list : upper(s) if s != \"\"]}",
                "pairs":"${{ for k, v in var.m : k => v }}"}"#
            )
        );
    }

    #[test]
    fn an_expression_over_several_lines_is_folded_onto_one() {
        let got = json(
            "x = merge(\n  var.tags, # the shared ones\n  {\n    Name = \"web\"\n    Env  = var.env\n  },\n)\ny = 1\n",
        );
        assert_eq!(
            got,
            want(r#"{"x":"${merge( var.tags, { Name = \"web\", Env = var.env }, )}","y":1}"#)
        );
    }

    #[test]
    fn strings_decode_their_escapes_and_keep_their_templates() {
        let got = json(
            r#"
            a = "line\nbreak \"quoted\" tab\t é"
            b = "web-${var.env}-${lookup(var.m, "k", "d")}"
            c = "$${literal} and %%{literal}"
            d = "%{ if var.x }yes%{ endif }"
            "#,
        );
        assert_eq!(
            got,
            want(
                r#"{"a":"line\nbreak \"quoted\" tab\t é",
                "b":"web-${var.env}-${lookup(var.m, \"k\", \"d\")}",
                "c":"$${literal} and %%{literal}",
                "d":"%{ if var.x }yes%{ endif }"}"#
            )
        );
        let body = parse(
            "t.tf",
            "d = \"%{ if var.x }yes%{ endif }\"\ne = \"%%{no}\"\n",
        )
        .unwrap();
        assert!(body.attributes[0].expr.has_directive());
        assert!(!body.attributes[1].expr.has_directive());
    }

    #[test]
    fn heredocs_keep_their_text_and_lose_their_margin_when_asked() {
        let got = json(
            "a = <<EOT\n  kept\n    as is ${var.x}\nEOT\nb = <<-EOT\n    one\n      two\n    EOT\nc = 1\n",
        );
        assert_eq!(
            got,
            want(r#"{"a":"  kept\n    as is ${var.x}\n","b":"one\n  two\n","c":1}"#)
        );
    }

    #[test]
    fn blocks_nest_by_type_and_labels_and_repeat_as_lists() {
        let got = json(
            r#"
            resource "aws_security_group" "web" {
              name = "web"
              ingress {
                from_port = 80
              }
              ingress {
                from_port = 443
              }
              lifecycle {
                create_before_destroy = true
              }
              depends_on = [aws_vpc.main, module.net]
              provisioner "local-exec" { command = "echo hi" }
            }
            resource "aws_security_group" "db" {}
            locals { a = 1 }
            locals { b = 2 }
            "#,
        );
        assert_eq!(
            got,
            want(
                r#"{"resource":{"aws_security_group":{
                  "web":{"name":"web","ingress":[{"from_port":80},{"from_port":443}],
                         "lifecycle":{"create_before_destroy":true},
                         "depends_on":["aws_vpc.main","module.net"],
                         "provisioner":{"local-exec":{"command":"echo hi"}}},
                  "db":{}}},
                "locals":[{"a":1},{"b":2}]}"#
            )
        );
    }

    #[test]
    fn comments_are_skipped_and_lines_are_recorded() {
        let body = parse(
            "t.tf",
            "# one\n// two\n/* three\n   four */\nvariable \"x\" {\n  default = 1 # trailing\n}\n",
        )
        .unwrap();
        assert_eq!(body.blocks[0].line, 5);
        assert_eq!(body.blocks[0].labels, ["x"]);
        assert_eq!(body.blocks[0].body.attributes[0].line, 6);
        assert_eq!(
            body.blocks[0].body.attributes[0].expr,
            Expr::Literal(Value::Int(1))
        );
    }

    #[test]
    fn a_syntax_error_names_the_file_and_line() {
        for (text, line) in [
            ("a = 1\nresource \"x\" \"y\" {\n  b = 2\n", 2),
            ("a = \"never closed\nb = 1\n", 1),
            ("a = [1, 2\nb = 1\n", 1),
            ("\n\n= 3\n", 3),
            ("a = <<EOT\nnever closed\n", 1),
            ("a =\n", 1),
        ] {
            let err = parse("bad.tf", text).unwrap_err();
            assert_eq!(
                (err.file.as_str(), err.line),
                ("bad.tf", line),
                "{text:?}: {err}"
            );
        }
    }
}
