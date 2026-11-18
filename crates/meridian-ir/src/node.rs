//! A positioned document tree, the common shape YAML and JSON are both loaded
//! into before anything is decoded into a typed document.
//!
//! Decoding from a tree rather than straight from the text is what lets every
//! diagnostic, including a wrong type or an unknown field, point at the line
//! and column it came from.

use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle, ScanError, Span};

use crate::error::{Error, Position};
use meridian_yaml::scalar::{resolve_plain, Scalar};

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub kind: NodeKind,
    /// 1-based; zero when unknown, which is always the case for JSON.
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum NodeKind {
    Scalar { raw: String, resolved: Scalar },
    Seq(Vec<Node>),
    Map(Vec<(Node, Node)>),
}

impl Node {
    pub fn is_null(&self) -> bool {
        matches!(
            self.kind,
            NodeKind::Scalar {
                resolved: Scalar::Null,
                ..
            }
        )
    }

    /// The word a diagnostic uses for this node's shape.
    pub fn describe(&self) -> &'static str {
        match &self.kind {
            NodeKind::Seq(_) => "a list",
            NodeKind::Map(_) => "a mapping",
            NodeKind::Scalar { resolved, .. } => match resolved {
                Scalar::Null => "null",
                Scalar::Bool(_) => "a boolean",
                Scalar::Int(_) | Scalar::Uint(_) | Scalar::Float(_) => "a number",
                Scalar::Str => "a string",
            },
        }
    }

    /// The value for `key` in a mapping node, if this is one and has it.
    pub fn child(&self, key: &str) -> Option<&Node> {
        match &self.kind {
            NodeKind::Map(entries) => entries
                .iter()
                .find(|(k, _)| matches!(&k.kind, NodeKind::Scalar { raw, .. } if raw == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Aliases are expanded by copying, so a small document can describe a very
/// large tree. Past this many nodes the document is refused.
const MAX_NODES: usize = 1_000_000;
/// Nesting deeper than this is refused rather than risking the stack. No
/// real document comes near it, and the limit has to hold on the smallest
/// stack the reader runs on: a thread with 1 MiB, in an unoptimised build,
/// overflowed at 128 levels when the limit was 256.
const MAX_DEPTH: usize = 64;

/// Loads every document in a YAML stream. A syntax error ends the stream: the
/// documents before it are discarded and the error is returned alone, located
/// at the document it occurred in.
pub(crate) fn load_yaml(file: &str, text: &str) -> Result<Vec<Node>, Error> {
    let mut loader = YamlLoader {
        parser: Parser::new_from_str(text),
        file,
        doc_idx: 0,
        anchors: HashMap::new(),
        nodes: 0,
    };
    let mut docs = Vec::new();
    loop {
        match loader.next()? {
            (Event::StreamStart | Event::Nothing, _) => {}
            (Event::StreamEnd, _) => return Ok(docs),
            (Event::DocumentStart(_), _) => {
                loader.anchors.clear();
                let (event, span) = loader.next()?;
                let root = loader.node(event, span, 0)?;
                docs.push(root);
                loader.doc_idx += 1;
            }
            _ => {}
        }
    }
}

struct YamlLoader<'a> {
    parser: Parser<'a, saphyr_parser::StrInput<'a>>,
    file: &'a str,
    doc_idx: usize,
    anchors: HashMap<usize, Node>,
    nodes: usize,
}

impl<'a> YamlLoader<'a> {
    fn next(&mut self) -> Result<(Event<'a>, Span), Error> {
        match self.parser.next_event() {
            Some(Ok(pair)) => Ok(pair),
            Some(Err(e)) => Err(self.scan_error(&e)),
            None => Ok((Event::StreamEnd, Span::default())),
        }
    }

    fn scan_error(&self, e: &ScanError) -> Error {
        let m = e.marker();
        Error::new(self.pos(m.line(), m.col() + 1), "", e.info().to_owned())
    }

    fn pos(&self, line: usize, column: usize) -> Position {
        Position {
            file: self.file.to_owned(),
            doc_idx: self.doc_idx,
            line,
            column,
        }
    }

    fn err(&self, node: &Node, msg: impl Into<String>) -> Error {
        Error::new(self.pos(node.line, node.column), "", msg)
    }

    /// `!!str` is the one tag a document may carry: it makes a value text,
    /// which quoting does as well. Every other tag is refused rather than
    /// ignored, because an ignored `!!int "12"` or `!!binary` would read as
    /// something other than what the author wrote.
    fn check_tag(&self, tag: &saphyr_parser::Tag, at: (usize, usize)) -> Result<(), Error> {
        if tag.is_yaml_core_schema() && tag.suffix == "str" {
            return Ok(());
        }
        let written = if tag.is_yaml_core_schema() {
            format!("!!{}", tag.suffix)
        } else {
            format!("{}{}", tag.handle, tag.suffix)
        };
        Err(Error::new(
            self.pos(at.0, at.1),
            "",
            format!(
                "the tag {written} is not supported; only !!str is. Write the value in its \
                 plain form, or quote it for text"
            ),
        ))
    }

    fn count(&mut self, n: usize, at: (usize, usize)) -> Result<(), Error> {
        self.nodes += n;
        if self.nodes > MAX_NODES {
            return Err(Error::new(
                self.pos(at.0, at.1),
                "",
                "document is too large (aliases are expanded by copying; this one expands past the limit)",
            ));
        }
        Ok(())
    }

    fn node(&mut self, event: Event<'a>, span: Span, depth: usize) -> Result<Node, Error> {
        let (line, column) = (span.start.line(), span.start.col() + 1);
        if depth > MAX_DEPTH {
            return Err(Error::new(
                self.pos(line, column),
                "",
                "document is nested too deeply",
            ));
        }
        let (node, anchor) = match event {
            Event::Scalar(value, style, anchor, tag) => {
                self.count(1, (line, column))?;
                if let Some(t) = &tag {
                    self.check_tag(t, (line, column))?;
                }
                let resolved = match (&tag, style) {
                    // An explicit !!str, or any quoting, makes it a string.
                    (Some(_), _) => Scalar::Str,
                    (_, ScalarStyle::Plain) => resolve_plain(&value),
                    _ => Scalar::Str,
                };
                let kind = NodeKind::Scalar {
                    raw: value.into_owned(),
                    resolved,
                };
                (Node { kind, line, column }, anchor)
            }
            Event::SequenceStart(anchor, tag) => {
                self.count(1, (line, column))?;
                if let Some(t) = &tag {
                    self.check_tag(t, (line, column))?;
                }
                let mut items = Vec::new();
                loop {
                    let (event, span) = self.next()?;
                    if matches!(event, Event::SequenceEnd) {
                        break;
                    }
                    items.push(self.node(event, span, depth + 1)?);
                }
                (
                    Node {
                        kind: NodeKind::Seq(items),
                        line,
                        column,
                    },
                    anchor,
                )
            }
            Event::MappingStart(anchor, tag) => {
                self.count(1, (line, column))?;
                if let Some(t) = &tag {
                    self.check_tag(t, (line, column))?;
                }
                let mut entries = Vec::new();
                loop {
                    let (event, span) = self.next()?;
                    if matches!(event, Event::MappingEnd) {
                        break;
                    }
                    // Whether a key is the merge key has to be decided from the
                    // event: once it is a node, `<<` and `"<<"` look the same.
                    let is_merge = matches!(
                        &event,
                        Event::Scalar(v, ScalarStyle::Plain, _, None) if v == "<<"
                    );
                    let key = self.node(event, span, depth + 1)?;
                    let (event, span) = self.next()?;
                    let value = self.node(event, span, depth + 1)?;
                    entries.push((is_merge, key, value));
                }
                let entries = self.merge(entries)?;
                (
                    Node {
                        kind: NodeKind::Map(entries),
                        line,
                        column,
                    },
                    anchor,
                )
            }
            Event::Alias(id) => {
                let Some(target) = self.anchors.get(&id) else {
                    return Err(Error::new(
                        self.pos(line, column),
                        "",
                        "alias refers to an unknown anchor",
                    ));
                };
                let copy = target.clone();
                self.count(size(&copy), (line, column))?;
                return Ok(copy);
            }
            other => {
                return Err(Error::new(
                    self.pos(line, column),
                    "",
                    format!("unexpected {other:?} where a value was expected"),
                ));
            }
        };
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        Ok(node)
    }

    /// Applies YAML merge keys (`<<: *defaults`). A key written in the mapping
    /// itself always wins over a merged one, wherever the merge key appears,
    /// and when several mappings are merged the earlier one wins.
    fn merge(&mut self, entries: Vec<(bool, Node, Node)>) -> Result<Vec<(Node, Node)>, Error> {
        let mut own = Vec::new();
        let mut merged: Vec<(Node, Node)> = Vec::new();
        for (is_merge, key, value) in entries {
            if !is_merge {
                own.push((key, value));
                continue;
            }
            let sources = match value.kind {
                NodeKind::Map(_) => vec![value],
                NodeKind::Seq(items) => items,
                NodeKind::Scalar { .. } => {
                    return Err(self.err(
                        &value,
                        "a merge key (<<) needs a mapping, or a list of mappings, to merge",
                    ));
                }
            };
            for source in sources {
                let NodeKind::Map(source_entries) = source.kind else {
                    return Err(self.err(&source, "a merge key (<<) can only merge mappings"));
                };
                for (k, v) in source_entries {
                    if !merged.iter().any(|(seen, _)| same_key(seen, &k)) {
                        merged.push((k, v));
                    }
                }
            }
        }
        merged.retain(|(k, _)| !own.iter().any(|(mine, _)| same_key(mine, k)));
        own.extend(merged);
        Ok(own)
    }
}

fn same_key(a: &Node, b: &Node) -> bool {
    match (&a.kind, &b.kind) {
        (NodeKind::Scalar { raw: x, .. }, NodeKind::Scalar { raw: y, .. }) => x == y,
        _ => false,
    }
}

fn size(node: &Node) -> usize {
    1 + match &node.kind {
        NodeKind::Scalar { .. } => 0,
        NodeKind::Seq(items) => items.iter().map(size).sum(),
        NodeKind::Map(entries) => entries.iter().map(|(k, v)| size(k) + size(v)).sum(),
    }
}

/// Whether a parsed JSON value nests past [`MAX_DEPTH`]. It walks with a list
/// of its own, not by recursion, since the point is to protect the stack.
pub(crate) fn json_too_deep(root: &serde_json::Value) -> bool {
    let mut pending = vec![(root, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return true;
        }
        match value {
            serde_json::Value::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            serde_json::Value::Object(map) => {
                pending.extend(map.values().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    false
}

/// Converts a parsed JSON value into the same tree. JSON carries no positions
/// past the syntax stage, so every node's line is zero.
pub(crate) fn from_json(value: serde_json::Value) -> Node {
    let scalar = |raw: String, resolved| Node {
        kind: NodeKind::Scalar { raw, resolved },
        line: 0,
        column: 0,
    };
    match value {
        serde_json::Value::Null => scalar("null".into(), Scalar::Null),
        serde_json::Value::Bool(b) => scalar(b.to_string(), Scalar::Bool(b)),
        serde_json::Value::Number(n) => {
            let resolved = if let Some(i) = n.as_i64() {
                Scalar::Int(i)
            } else if let Some(u) = n.as_u64() {
                Scalar::Uint(u)
            } else {
                Scalar::Float(n.as_f64().unwrap_or(f64::NAN))
            };
            scalar(n.to_string(), resolved)
        }
        serde_json::Value::String(s) => scalar(s, Scalar::Str),
        serde_json::Value::Array(items) => Node {
            kind: NodeKind::Seq(items.into_iter().map(from_json).collect()),
            line: 0,
            column: 0,
        },
        serde_json::Value::Object(map) => Node {
            kind: NodeKind::Map(
                map.into_iter()
                    .map(|(k, v)| (scalar(k, Scalar::Str), from_json(v)))
                    .collect(),
            ),
            line: 0,
            column: 0,
        },
    }
}
