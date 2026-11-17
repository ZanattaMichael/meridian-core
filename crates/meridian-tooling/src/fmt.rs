//! `meridian fmt`: one canonical layout for a Meridian document.
//!
//! The formatter changes how a document is laid out and never what it says.
//! It indents by two spaces, writes a mapping's fields in the order the
//! language defines them, removes trailing spaces and surplus blank lines,
//! and leaves every value, quote and comment as the author wrote it.
//!
//! Two checks stand behind that promise. Before returning, the formatted
//! text is parsed again and compared, value for value, with the original;
//! and its comments are counted against the original's. If either differs
//! the formatter refuses, and the file is left as it was. A refusal is a bug
//! in the formatter, never something for the author to fix.

use std::fmt;

use meridian_ir::schema::{self, Shape, Type};

use crate::syntax::{self, indent_of, line_kind, Doc, LineKind, Node, NodeKind};

/// How to format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Write fields in the language's own order (`id`, `type`, `state`,
    /// `params`, ...). Fields the language does not define, such as a
    /// resource's params, always keep the author's order.
    pub sort_fields: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { sort_fields: true }
    }
}

/// Why a file was not formatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The text is not valid YAML. The compiler's own message says where.
    Syntax(String),
    /// Formatting would have changed what the document says, or lost a
    /// comment. The file is left alone.
    Unsafe(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(why) => write!(f, "not formatted, the file does not parse: {why}"),
            Error::Unsafe(why) => write!(
                f,
                "not formatted: {why}. The file is unchanged. This is a bug in the \
                 formatter; please report it with the file"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Formats a file. Returns the text unchanged when it is JSON, which has one
/// layout already and is left to JSON tools.
pub fn format(name: &str, text: &str, options: Options) -> Result<String, Error> {
    let before = meridian_ir::parse_stream(name, text.as_bytes())
        .map_err(|errors| Error::Syntax(errors.to_string()))?;
    let file = syntax::read(text);
    if file.json {
        return Ok(text.to_owned());
    }

    let mut writer = Writer {
        lines: &file.lines,
        out: Vec::new(),
        options,
        prefix: None,
    };
    for doc in &file.docs {
        writer.doc(doc);
    }
    let formatted = writer.finish();

    // The promise: same values, same comments.
    let after = meridian_ir::parse_stream(name, formatted.as_bytes()).map_err(|errors| {
        Error::Unsafe(format!("the formatted text would not parse ({errors})"))
    })?;
    if before != after {
        return Err(Error::Unsafe(
            "the formatted text would not mean the same as the original".to_owned(),
        ));
    }
    if comments(&file.lines) != comments(&syntax::read(&formatted).lines) {
        return Err(Error::Unsafe(
            "the formatted text would not keep every comment".to_owned(),
        ));
    }
    Ok(formatted)
}

/// Every whole-line comment, sorted, so two texts can be compared for
/// having the same ones wherever they sit.
fn comments(lines: &[String]) -> Vec<&str> {
    let mut found: Vec<&str> = lines
        .iter()
        .filter(|l| line_kind(l) == LineKind::Comment)
        .map(|l| l.trim())
        .collect();
    found.sort_unstable();
    found
}

const STEP: usize = 2;

struct Writer<'a> {
    lines: &'a [String],
    out: Vec<String>,
    options: Options,
    /// Text the next content line starts with in place of its indentation:
    /// the `- ` of a list item whose first field sits on the dash's line.
    prefix: Option<String>,
}

impl Writer<'_> {
    fn finish(mut self) -> String {
        while self.out.last().is_some_and(String::is_empty) {
            self.out.pop();
        }
        let mut text = self.out.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }

    fn blank(&mut self) {
        // One blank line at most, and none at the top of the file or
        // directly under a document marker.
        match self.out.last() {
            None => {}
            Some(last) if last.is_empty() || last == "---" => {}
            Some(_) => self.out.push(String::new()),
        }
    }

    /// Writes one content line at `indent`, or after the pending prefix.
    fn line(&mut self, indent: usize, text: &str) {
        let start = match self.prefix.take() {
            Some(prefix) => prefix,
            None => " ".repeat(indent),
        };
        self.out.push(format!("{start}{}", text.trim_end()));
    }

    /// Writes comment and blank lines at `indent`.
    fn trivia(&mut self, lines: &[usize], indent: usize) {
        for &l in lines {
            let text = &self.lines[l];
            match line_kind(text) {
                LineKind::Blank => self.blank(),
                // A comment never takes a pending prefix: it goes above the
                // line the prefix belongs to.
                _ => self
                    .out
                    .push(format!("{}{}", " ".repeat(indent), text.trim())),
            }
        }
    }

    fn doc(&mut self, doc: &Doc) {
        if let Some(marker) = doc.marker {
            // Comments above the marker belong to this document.
            let above: Vec<usize> = (doc.first_line..marker).collect();
            self.trivia(&above, 0);
            self.out.push(self.lines[marker].trim_end().to_owned());
        }
        let Some(root) = &doc.root else {
            self.trivia(&doc.leading, 0);
            return;
        };
        self.trivia(&doc.leading, 0);
        let shape = root
            .text("kind")
            .and_then(schema::kind_named)
            .map(schema::document);
        self.node(root, 0, shape);
    }

    fn node(&mut self, node: &Node, indent: usize, shape: Option<&'static Shape>) {
        match &node.kind {
            NodeKind::Map(entries) => {
                let mut order: Vec<usize> = (0..entries.len()).collect();
                if let (true, Some(shape)) = (self.options.sort_fields, shape) {
                    let names = shape.names();
                    let rank = |i: &usize| {
                        names
                            .iter()
                            .position(|n| *n == entries[*i].key)
                            .unwrap_or(names.len())
                    };
                    // Stable, so fields the language does not define keep
                    // the author's order after the ones it does.
                    order.sort_by_key(rank);
                }
                for i in order {
                    let entry = &entries[i];
                    // A comment above a field that moves up onto a dash's
                    // line is written above the dash, at the dash's indent.
                    let trivia_indent = match &self.prefix {
                        Some(prefix) => indent_of(prefix),
                        None => indent,
                    };
                    self.trivia(&entry.leading, trivia_indent);
                    let source = &self.lines[entry.line];
                    let key = &source[entry.key_span.start.col..entry.key_span.end.col];
                    let Some(colon) = entry.colon else {
                        self.line(indent, key);
                        continue;
                    };
                    let field = shape.and_then(|s| s.field(&entry.key));
                    let child = field.and_then(|f| match &f.ty {
                        Type::Object(next) | Type::List(next) => Some(*next),
                        _ => None,
                    });
                    match &entry.value {
                        None => {
                            let tail = source[colon + 1..].trim();
                            self.line(indent, &join(&format!("{key}:"), tail));
                        }
                        Some(value) if value.line == entry.line => {
                            let tail = &source[value.indent..];
                            self.line(indent, &format!("{key}: {tail}"));
                            self.continuation(value, entry.key_span.start.col, indent);
                        }
                        Some(value) => {
                            let tail = source[colon + 1..].trim();
                            self.line(indent, &join(&format!("{key}:"), tail));
                            self.node(value, indent + STEP, child);
                        }
                    }
                }
            }
            NodeKind::Seq(items) => {
                for item in items {
                    let trivia_indent = match &self.prefix {
                        Some(prefix) => indent_of(prefix),
                        None => indent,
                    };
                    self.trivia(&item.leading, trivia_indent);
                    let source = &self.lines[item.line];
                    match &item.value {
                        None => {
                            let tail = source[item.dash_col + 1..].trim();
                            self.line(indent, &join("-", tail));
                        }
                        Some(value) if value.line == item.line => {
                            // The item's first line shares the dash's line.
                            let start = match self.prefix.take() {
                                Some(prefix) => prefix,
                                None => " ".repeat(indent),
                            };
                            self.prefix = Some(format!("{start}- "));
                            self.node(value, indent + STEP, shape);
                        }
                        Some(value) => {
                            let tail = source[item.dash_col + 1..].trim();
                            self.line(indent, &join("-", tail));
                            self.node(value, indent + STEP, shape);
                        }
                    }
                }
            }
            NodeKind::Scalar(_) => {
                let source = &self.lines[node.line];
                self.line(indent, &source[node.indent..]);
                // A scalar that is a node of its own continues relative to
                // where it started.
                self.continuation(node, node.indent, indent);
            }
        }
        self.trivia(&node.trailing, indent);
    }

    /// Writes the lines a multi-line scalar runs on over. The least
    /// indented of them goes one step in from the line that opened the
    /// scalar and the rest keep their distance from it, so text inside a
    /// block keeps its own indentation.
    fn continuation(&mut self, value: &Node, old: usize, new: usize) {
        let body = value.line + 1..value.end_line;
        let header = &self.lines[value.line][value.indent..];
        // `|2` fixes the body's indent relative to its parent, so such a
        // body may only move as far as the parent does.
        let explicit = matches!(value.scalar().map(|s| s.style), Some(syntax::Style::Block))
            && header
                .split_whitespace()
                .next()
                .is_some_and(|h| h.chars().any(|c| c.is_ascii_digit()));
        let least = body
            .clone()
            .map(|l| self.lines[l].as_str())
            .filter(|l| !l.trim().is_empty())
            .map(indent_of)
            .min()
            .unwrap_or(0);
        for l in body {
            let text = self.lines[l].trim_end();
            if text.is_empty() {
                self.out.push(String::new());
                continue;
            }
            let indent = if explicit {
                (indent_of(text) + new).saturating_sub(old)
            } else {
                indent_of(text) - least + new + STEP
            };
            self.out
                .push(format!("{}{}", " ".repeat(indent), text.trim_start()));
        }
    }
}

fn join(head: &str, tail: &str) -> String {
    if tail.is_empty() {
        head.to_owned()
    } else {
        format!("{head} {tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(text: &str) -> String {
        let out = format("test.yaml", text, Options::default()).expect("formats");
        let again = format("test.yaml", &out, Options::default()).expect("formats twice");
        assert_eq!(out, again, "formatting is not stable");
        out
    }

    #[test]
    fn layout_is_normalised_and_fields_take_the_language_order() {
        let messy = "\
kind: ResourceSet
apiVersion: meridian/v1


spec:
    resources:
    -   type: package
        # the name other resources use
        id: nginx_pkg
        params: {name: nginx}
    -   dependsOn:
          -   resource: nginx_pkg
        params:
            path: /etc/nginx.conf
            content: |
                server {
                  listen 80;
                }
            mode: \"0644\"   # quoted, so it is not a number
        type: file
        id: nginx_conf
    target: ansible
    targetHost: {from: inline, host: web01}
metadata:
    name:   web
";
        let want = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web

spec:
  target: ansible
  targetHost: {from: inline, host: web01}
  resources:
    # the name other resources use
    - id: nginx_pkg
      type: package
      params: {name: nginx}
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx.conf
        content: |
          server {
            listen 80;
          }
        mode: \"0644\"   # quoted, so it is not a number
      dependsOn:
        - resource: nginx_pkg
";
        assert_eq!(fmt(messy), want);
    }

    #[test]
    fn params_and_unknown_documents_keep_their_order() {
        let text = "zebra: 1\napple: 2\n";
        assert_eq!(fmt(text), text);
        let doc = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: x
spec:
  target: ansible
  resources:
    - id: a
      type: user
      params:
        uid: 1
        name: z
        home: /h
";
        assert_eq!(fmt(doc), doc);
    }

    #[test]
    fn sorting_can_be_turned_off() {
        let text = "kind: ResourceSet\napiVersion: meridian/v1\n";
        let kept = format("t", text, Options { sort_fields: false }).unwrap();
        assert_eq!(kept, text);
    }

    #[test]
    fn several_documents_and_their_comments_survive() {
        let text = "\
# The network.
---
kind: Infrastructure
apiVersion: meridian/v1
metadata: {name: net}
spec:
  outputs:
  - name: ip
    value: ${vm.ip}
  target: terraform
  resources:
  - id: vm
    type: aws_instance
    params:
      tags:
          Name: web
      # trailing note
---

# The host.
apiVersion: meridian/v1
kind: ResourceSet
";
        let want = "\
# The network.
---
apiVersion: meridian/v1
kind: Infrastructure
metadata: {name: net}
spec:
  target: terraform
  resources:
    - id: vm
      type: aws_instance
      params:
        tags:
          Name: web
        # trailing note
  outputs:
    - name: ip
      value: ${vm.ip}
---
# The host.
apiVersion: meridian/v1
kind: ResourceSet
";
        assert_eq!(fmt(text), want);
    }

    #[test]
    fn text_that_does_not_parse_is_refused_and_json_is_left_alone() {
        let err = format("bad.yaml", "a: [1, 2\nb: 3\n", Options::default()).unwrap_err();
        assert!(matches!(err, Error::Syntax(_)), "{err}");
        let json = "{\"apiVersion\": \"meridian/v1\",   \"kind\": \"ResourceSet\"}\n";
        assert_eq!(format("x.json", json, Options::default()).unwrap(), json);
    }

    #[test]
    fn multi_line_scalars_keep_their_meaning() {
        let text = "\
a:
    b: >-
        folded
          more
    c: \"double
      quoted\"
    d: plain
      continued
    e: [1,
      2]
    f: |+
        keep

    g: 1
";
        let out = fmt(text);
        assert!(out.contains("\n  b: >-\n    folded\n      more\n"), "{out}");
        assert!(out.contains("\n  g: 1\n"), "{out}");
        assert!(out.contains("\n  f: |+\n    keep\n\n  g: 1"), "{out}");

        // An explicit indentation indicator pins the body to its parent.
        let pinned = "a:\n    b: |2\n        two more\n";
        assert_eq!(fmt(pinned), "a:\n  b: |2\n      two more\n");
    }

    #[test]
    fn an_empty_file_stays_empty() {
        assert_eq!(fmt(""), "");
        assert_eq!(fmt("# only a comment\n"), "# only a comment\n");
    }
}
