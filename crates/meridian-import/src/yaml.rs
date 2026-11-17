//! Reading YAML and JSON the way the tool whose file it is reads them.
//!
//! An importer needs two things a plain value does not give it. Key order,
//! because an Ansible task's module is whichever key is not a keyword, and a
//! report should follow the file. And line numbers, so a note can say where
//! in the source the thing it is about was written.
//!
//! It also needs the right scalar rules. Meridian's own documents follow
//! YAML 1.2, where only `true` and `false` are booleans. Ansible and
//! Kubernetes read YAML 1.1, where `yes`, `no`, `on` and `off` are booleans
//! too, and a playbook that says `enabled: yes` means `true`. [`Rules`]
//! chooses.

use std::collections::{BTreeMap, HashMap};

use meridian_sdk::{ImportError, Value};
use meridian_yaml::scalar::{resolve_plain, Scalar};
use saphyr_parser::{Event, Parser, ScalarStyle, Span};

/// Which scalar rules a file is read with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rules {
    /// YAML 1.2: only `true` and `false` are booleans.
    Yaml12,
    /// YAML 1.1: `yes`, `no`, `on`, `off` and `y`, `n` are booleans as well,
    /// in any of the three capitalisations that version allows.
    Yaml11,
}

/// One node of a document, with the line it was written on.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: Kind,
    /// 1-based.
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    Seq(Vec<Node>),
    /// In the order written. A key that is not a string is kept as its text.
    Map(Vec<(String, Node)>),
}

impl Node {
    /// The node as a free-form value. Key order is lost: a value's maps are
    /// sorted.
    pub fn value(&self) -> Value {
        match &self.kind {
            Kind::Null => Value::Null,
            Kind::Bool(b) => Value::Bool(*b),
            Kind::Int(i) => Value::Int(*i),
            Kind::Uint(u) => Value::Uint(*u),
            Kind::Float(f) => Value::Float(*f),
            Kind::Str(s) => Value::String(s.clone()),
            Kind::Seq(items) => Value::List(items.iter().map(Node::value).collect()),
            Kind::Map(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.value()))
                    .collect::<BTreeMap<_, _>>(),
            ),
        }
    }

    pub fn is_null(&self) -> bool {
        self.kind == Kind::Null
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> Option<&[Node]> {
        match &self.kind {
            Kind::Seq(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(String, Node)]> {
        match &self.kind {
            Kind::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// The value for `key`, when this is a mapping that has it.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// A scalar as the text a person would write for it; `None` for a list or
    /// a mapping.
    pub fn scalar_text(&self) -> Option<String> {
        Some(match &self.kind {
            Kind::Null => String::new(),
            Kind::Bool(b) => b.to_string(),
            Kind::Int(i) => i.to_string(),
            Kind::Uint(u) => u.to_string(),
            Kind::Float(f) => f.to_string(),
            Kind::Str(s) => s.clone(),
            Kind::Seq(_) | Kind::Map(_) => return None,
        })
    }

    /// The word a message uses for this node's shape.
    pub fn describe(&self) -> &'static str {
        match &self.kind {
            Kind::Null => "nothing",
            Kind::Bool(_) => "a boolean",
            Kind::Int(_) | Kind::Uint(_) | Kind::Float(_) => "a number",
            Kind::Str(_) => "a string",
            Kind::Seq(_) => "a list",
            Kind::Map(_) => "a mapping",
        }
    }
}

/// Aliases are expanded by copying, so a small file can describe a very large
/// tree. Past this many nodes the file is refused.
const MAX_NODES: usize = 1_000_000;
/// Nesting deeper than this is refused instead of risking the stack.
const MAX_DEPTH: usize = 256;

/// Reads every document of a YAML stream. JSON is YAML, so a JSON file reads
/// the same way. An empty document is left out.
pub fn read(file: &str, text: &str, rules: Rules) -> Result<Vec<Node>, ImportError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader {
        parser: Parser::new_from_str(text),
        file,
        rules,
        anchors: HashMap::new(),
        nodes: 0,
    };
    let mut documents = Vec::new();
    loop {
        match reader.next()? {
            (Event::StreamEnd, _) => return Ok(documents),
            (Event::DocumentStart(_), _) => {
                reader.anchors.clear();
                let (event, span) = reader.next()?;
                if matches!(event, Event::DocumentEnd) {
                    continue;
                }
                let root = reader.node(event, span, 0)?;
                if !root.is_null() {
                    documents.push(root);
                }
            }
            _ => {}
        }
    }
}

struct Reader<'a> {
    parser: Parser<'a, saphyr_parser::StrInput<'a>>,
    file: &'a str,
    rules: Rules,
    anchors: HashMap<usize, Node>,
    nodes: usize,
}

impl<'a> Reader<'a> {
    fn next(&mut self) -> Result<(Event<'a>, Span), ImportError> {
        match self.parser.next_event() {
            Some(Ok(pair)) => Ok(pair),
            Some(Err(e)) => Err(ImportError::at(
                self.file,
                e.marker().line(),
                e.info().to_owned(),
            )),
            None => Ok((Event::StreamEnd, Span::default())),
        }
    }

    fn count(&mut self, n: usize, line: usize) -> Result<(), ImportError> {
        self.nodes += n;
        if self.nodes > MAX_NODES {
            return Err(ImportError::at(
                self.file,
                line,
                "the file is too large (aliases are expanded by copying; this one expands past \
                 the limit)",
            ));
        }
        Ok(())
    }

    fn scalar(&self, raw: &str, plain: bool) -> Kind {
        if !plain {
            return Kind::Str(raw.to_owned());
        }
        if self.rules == Rules::Yaml11 {
            match raw {
                "yes" | "Yes" | "YES" | "on" | "On" | "ON" | "y" | "Y" => return Kind::Bool(true),
                "no" | "No" | "NO" | "off" | "Off" | "OFF" | "n" | "N" => return Kind::Bool(false),
                _ => {}
            }
        }
        match resolve_plain(raw) {
            Scalar::Null => Kind::Null,
            Scalar::Bool(b) => Kind::Bool(b),
            Scalar::Int(i) => Kind::Int(i),
            Scalar::Uint(u) => Kind::Uint(u),
            Scalar::Float(f) => Kind::Float(f),
            Scalar::Str => Kind::Str(raw.to_owned()),
        }
    }

    fn node(&mut self, event: Event<'a>, span: Span, depth: usize) -> Result<Node, ImportError> {
        let line = span.start.line();
        if depth > MAX_DEPTH {
            return Err(ImportError::at(
                self.file,
                line,
                "the file is nested too deeply",
            ));
        }
        let (node, anchor) = match event {
            Event::Scalar(value, style, anchor, tag) => {
                self.count(1, line)?;
                let is_str_tag =
                    matches!(&tag, Some(t) if t.is_yaml_core_schema() && t.suffix == "str");
                let plain = style == ScalarStyle::Plain && !is_str_tag;
                let kind = self.scalar(&value, plain);
                (Node { kind, line }, anchor)
            }
            Event::SequenceStart(anchor, _) => {
                self.count(1, line)?;
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
                        kind: Kind::Seq(items),
                        line,
                    },
                    anchor,
                )
            }
            Event::MappingStart(anchor, _) => {
                self.count(1, line)?;
                let mut own: Vec<(String, Node)> = Vec::new();
                let mut merged: Vec<(String, Node)> = Vec::new();
                loop {
                    let (event, span) = self.next()?;
                    if matches!(event, Event::MappingEnd) {
                        break;
                    }
                    let is_merge = matches!(
                        &event,
                        Event::Scalar(v, ScalarStyle::Plain, _, None) if v == "<<"
                    );
                    let key_line = span.start.line();
                    // A key is its text, whatever it would have resolved to:
                    // `80: http` and `"80": http` name the same thing.
                    let key = match &event {
                        Event::Scalar(v, ..) => v.to_string(),
                        _ => String::new(),
                    };
                    let key_node = self.node(event, span, depth + 1)?;
                    let (event, span) = self.next()?;
                    let value = self.node(event, span, depth + 1)?;
                    if is_merge {
                        let sources = match value.kind {
                            Kind::Map(_) => vec![value],
                            Kind::Seq(items) => items,
                            _ => {
                                return Err(ImportError::at(
                                    self.file,
                                    key_line,
                                    "a merge key (<<) needs a mapping, or a list of mappings",
                                ))
                            }
                        };
                        for source in sources {
                            let Kind::Map(entries) = source.kind else {
                                return Err(ImportError::at(
                                    self.file,
                                    key_line,
                                    "a merge key (<<) can only merge mappings",
                                ));
                            };
                            for (k, v) in entries {
                                if !merged.iter().any(|(seen, _)| *seen == k) {
                                    merged.push((k, v));
                                }
                            }
                        }
                        continue;
                    }
                    if matches!(key_node.kind, Kind::Seq(_) | Kind::Map(_)) {
                        return Err(ImportError::at(
                            self.file,
                            key_line,
                            "a mapping key must be a scalar",
                        ));
                    }
                    // A repeated key keeps its last value, as the tools do.
                    match own.iter_mut().find(|(k, _)| *k == key) {
                        Some(slot) => slot.1 = value,
                        None => own.push((key, value)),
                    }
                }
                merged.retain(|(k, _)| !own.iter().any(|(mine, _)| mine == k));
                own.extend(merged);
                (
                    Node {
                        kind: Kind::Map(own),
                        line,
                    },
                    anchor,
                )
            }
            Event::Alias(id) => {
                let Some(target) = self.anchors.get(&id) else {
                    return Err(ImportError::at(
                        self.file,
                        line,
                        "an alias refers to an unknown anchor",
                    ));
                };
                let copy = target.clone();
                self.count(size(&copy), line)?;
                return Ok(copy);
            }
            other => {
                return Err(ImportError::at(
                    self.file,
                    line,
                    format!("unexpected {other:?} where a value was expected"),
                ))
            }
        };
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        Ok(node)
    }
}

fn size(node: &Node) -> usize {
    1 + match &node.kind {
        Kind::Seq(items) => items.iter().map(size).sum(),
        Kind::Map(entries) => entries.iter().map(|(_, v)| 1 + size(v)).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_order_and_lines_are_kept() {
        let docs = read("f.yml", "b: 1\na:\n  - x\n  - y\n", Rules::Yaml12).unwrap();
        assert_eq!(docs.len(), 1);
        let entries = docs[0].as_map().unwrap();
        assert_eq!(entries[0].0, "b");
        assert_eq!(entries[1].0, "a");
        assert_eq!(entries[1].1.as_seq().unwrap()[1].line, 4);
    }

    #[test]
    fn yes_is_a_boolean_only_under_the_older_rules() {
        let text = "enabled: yes\nquoted: \"yes\"\nreal: true\n";
        let new = read("f.yml", text, Rules::Yaml12).unwrap();
        assert_eq!(new[0].get("enabled").unwrap().as_str(), Some("yes"));
        let old = read("f.yml", text, Rules::Yaml11).unwrap();
        assert_eq!(old[0].get("enabled").unwrap().kind, Kind::Bool(true));
        assert_eq!(old[0].get("quoted").unwrap().as_str(), Some("yes"));
        assert_eq!(old[0].get("real").unwrap().kind, Kind::Bool(true));
    }

    #[test]
    fn a_leading_zero_is_octal_and_a_quoted_one_is_text() {
        // Which is why an unquoted `mode: 0644` means what its author
        // expects, in a playbook and in a manifest.
        let text = "mode: 0644\nquoted: \"0644\"\nzero: 0\n";
        let read = read("f.yml", text, Rules::Yaml11).unwrap();
        assert_eq!(read[0].get("mode").unwrap().kind, Kind::Int(420));
        assert_eq!(
            read[0].get("quoted").unwrap().kind,
            Kind::Str("0644".to_owned())
        );
        assert_eq!(read[0].get("zero").unwrap().kind, Kind::Int(0));
    }

    #[test]
    fn a_stream_holds_several_documents_and_empty_ones_are_skipped() {
        let docs = read("f.yml", "---\na: 1\n---\n---\nb: 2\n", Rules::Yaml12).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[1].line, 5);
    }

    #[test]
    fn anchors_and_merge_keys_are_expanded() {
        let text = "base: &b\n  image: nginx\n  restart: always\nweb:\n  <<: *b\n  image: caddy\n";
        let docs = read("f.yml", text, Rules::Yaml12).unwrap();
        let web = docs[0].get("web").unwrap();
        assert_eq!(web.get("image").unwrap().as_str(), Some("caddy"));
        assert_eq!(web.get("restart").unwrap().as_str(), Some("always"));
    }

    #[test]
    fn json_reads_as_yaml_and_a_syntax_error_names_its_line() {
        let docs = read("f.json", "{\"a\": [1, 2.5, null]}", Rules::Yaml12).unwrap();
        assert_eq!(
            docs[0].value(),
            Value::Map(BTreeMap::from([(
                "a".to_owned(),
                Value::List(vec![Value::Int(1), Value::Float(2.5), Value::Null])
            )]))
        );
        let err = read("f.yml", "a: 1\nb: [1, 2\nc: 3\n", Rules::Yaml12).unwrap_err();
        assert_eq!(err.file, "f.yml");
        assert!(err.line >= 2, "{err}");
    }
}
