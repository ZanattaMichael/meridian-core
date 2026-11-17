use std::collections::BTreeMap;
use std::path::Path;

use crate::api::VersionTable;
use crate::error::{Error, ErrorList, Position};
use crate::node::{self, Node, NodeKind};
use crate::schema;
use crate::types::*;
use meridian_value::Value;
use meridian_yaml::scalar::Scalar;

/// Reads `path` and parses every document it contains. The file extension is
/// not trusted: the content decides between JSON and YAML.
pub fn parse_file(path: impl AsRef<Path>) -> Result<Vec<Document>, ErrorList> {
    let path = path.as_ref();
    let name = path.to_string_lossy();
    match std::fs::read(path) {
        Ok(data) => parse(&name, &data),
        Err(e) => Err(Error::new(
            Position::document(&name, 0),
            "",
            format!("cannot read file: {e}"),
        )
        .into()),
    }
}

/// Parses a single- or multi-document byte stream. `name` is used only in
/// diagnostics. A YAML stream may hold several `---`-separated documents; a
/// JSON stream may be one object or a top-level array of objects.
pub fn parse(name: &str, data: &[u8]) -> Result<Vec<Document>, ErrorList> {
    let text = match std::str::from_utf8(data) {
        Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text),
        Err(e) => {
            let (line, column) = line_column(data, e.valid_up_to());
            let pos = Position {
                file: name.to_owned(),
                doc_idx: 0,
                line,
                column,
            };
            return Err(Error::new(pos, "", "input is not valid UTF-8").into());
        }
    };

    let (roots, json) = load_roots(name, text, true).map_err(|e| unquoted_hint(e, text))?;
    if roots.is_empty() {
        return Err(Error::new(Position::document(name, 0), "", "no documents found").into());
    }

    let mut errs = ErrorList::new();
    let mut docs = Vec::with_capacity(roots.len());
    for (doc_idx, root) in roots.iter().enumerate() {
        let mut d = Decoder {
            file: name,
            doc_idx,
            json,
            errs: ErrorList::new(),
        };
        let doc = d.document(root);
        match doc {
            Some(doc) if d.errs.is_empty() => docs.push(doc),
            _ => errs.extend(d.errs),
        }
    }
    errs.finish(docs)
}

/// Parses a free-form YAML or JSON document, such as a hierarchy data layer,
/// into a [`Value`]. An empty document is [`Value::Null`]. The stream must hold
/// a single document.
pub fn parse_value(name: &str, data: &[u8]) -> Result<Value, ErrorList> {
    let text = match std::str::from_utf8(data) {
        Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text),
        Err(e) => {
            let (line, column) = line_column(data, e.valid_up_to());
            let pos = Position {
                file: name.to_owned(),
                doc_idx: 0,
                line,
                column,
            };
            return Err(Error::new(pos, "", "input is not valid UTF-8").into());
        }
    };
    let (mut roots, json) = load_roots(name, text, false)?;
    let mut d = Decoder {
        file: name,
        doc_idx: 0,
        json,
        errs: ErrorList::new(),
    };
    if roots.len() > 1 {
        d.doc_idx = 1;
        d.error(&roots[1], "", "expected a single document, found another");
        d.doc_idx = 0;
    }
    let value = match roots.first_mut() {
        Some(root) => d.value(root, ""),
        None => Value::Null,
    };
    d.errs.finish(value)
}

/// Parses every document in a YAML or JSON stream into a free-form
/// [`Value`], without reading any of them as a Meridian document.
///
/// It is what lets a tool ask whether two texts say the same thing: a
/// formatter compares the values before and after its change. A stream with
/// no documents is an empty list.
pub fn parse_stream(name: &str, data: &[u8]) -> Result<Vec<Value>, ErrorList> {
    let text = match std::str::from_utf8(data) {
        Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text),
        Err(e) => {
            let (line, column) = line_column(data, e.valid_up_to());
            let pos = Position {
                file: name.to_owned(),
                doc_idx: 0,
                line,
                column,
            };
            return Err(Error::new(pos, "", "input is not valid UTF-8").into());
        }
    };
    let (roots, json) = load_roots(name, text, true)?;
    let mut errs = ErrorList::new();
    let mut values = Vec::with_capacity(roots.len());
    for (doc_idx, root) in roots.iter().enumerate() {
        let mut d = Decoder {
            file: name,
            doc_idx,
            json,
            errs: ErrorList::new(),
        };
        values.push(d.value(root, ""));
        errs.extend(d.errs);
    }
    errs.finish(values)
}

/// YAML reads `: ` and ` #` inside an unquoted value as its own syntax, and
/// an expression often holds one (`a ? b : c`). The YAML error alone does
/// not say that, so a line that holds an interpolation gets the way out.
fn unquoted_hint(mut e: Error, text: &str) -> Error {
    let line = e.pos.line.checked_sub(1).and_then(|i| text.lines().nth(i));
    if line.is_some_and(meridian_expr::has_interpolation) {
        e.msg.push_str(
            "; this line holds an interpolation, and YAML reads a colon followed by a space \
             inside an unquoted value as its own: put the whole value in double quotes",
        );
    }
    e
}

fn json_error(name: &str, e: &serde_json::Error) -> Error {
    let pos = Position {
        file: name.to_owned(),
        doc_idx: 0,
        line: e.line(),
        column: e.column(),
    };
    Error::new(pos, "", e.to_string())
}

/// Loads the documents in `text`, and says whether they were read as JSON.
///
/// Text whose first significant byte opens a JSON value is read as JSON. If
/// that fails it is tried as YAML, because YAML flow style (`{name: web}`)
/// opens the same way; when both fail the JSON error is the one reported,
/// since JSON is what the text looked like. With `array_is_stream`, a
/// top-level JSON array is a stream of documents instead of one document.
fn load_roots(name: &str, text: &str, array_is_stream: bool) -> Result<(Vec<Node>, bool), Error> {
    let first = text
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .as_bytes()
        .first();
    if !matches!(first, Some(b'{' | b'[')) {
        return Ok((node::load_yaml(name, text)?, false));
    }
    let parsed = serde_json::from_str::<serde_json::Value>(text);
    // JSON is held to the limit YAML is held to, so a document is not too
    // deep in one spelling and fine in the other.
    if parsed.as_ref().is_ok_and(node::json_too_deep) {
        return Err(Error::new(
            Position::document(name, 0),
            "",
            "document is nested too deeply",
        ));
    }
    match parsed {
        Ok(serde_json::Value::Array(items)) if array_is_stream => {
            Ok((items.into_iter().map(node::from_json).collect(), true))
        }
        Ok(value) => Ok((vec![node::from_json(value)], true)),
        Err(json_err) => match node::load_yaml(name, text) {
            Ok(roots) => Ok((roots, false)),
            Err(_) => Err(json_error(name, &json_err)),
        },
    }
}

/// Converts a byte offset into a 1-based line and column.
fn line_column(data: &[u8], offset: usize) -> (usize, usize) {
    let before = &data[..offset.min(data.len())];
    let line = 1 + before.iter().filter(|b| **b == b'\n').count();
    let column = 1 + before.iter().rev().take_while(|b| **b != b'\n').count();
    (line, column)
}

fn join(path: &str, field: &str) -> String {
    if path.is_empty() {
        field.to_owned()
    } else {
        format!("{path}.{field}")
    }
}

/// The fields of one mapping, already checked against what its type allows.
struct Fields<'n>(Vec<(&'n str, &'n Node)>);

impl<'n> Fields<'n> {
    fn get(&self, key: &str) -> Option<&'n Node> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

struct Decoder<'a> {
    file: &'a str,
    doc_idx: usize,
    /// JSON is typed, so a number where a string belongs is an error. YAML is
    /// not, and `name: 123` is the string "123".
    json: bool,
    errs: ErrorList,
}

impl Decoder<'_> {
    fn at(&self, node: &Node) -> Position {
        Position {
            file: self.file.to_owned(),
            doc_idx: self.doc_idx,
            line: node.line,
            column: node.column,
        }
    }

    /// The node's own position, or `fallback` when the source format has none.
    fn at_or(&self, node: &Node, fallback: &Position) -> Position {
        if node.line == 0 {
            fallback.clone()
        } else {
            self.at(node)
        }
    }

    fn error(&mut self, node: &Node, path: &str, msg: impl Into<String>) {
        let pos = self.at(node);
        self.errs.push(Error::new(pos, path, msg));
    }

    /// Reads a mapping as the fields of a type. An unrecognised field is an
    /// error, not data silently dropped on the floor.
    fn fields<'n>(&mut self, node: Option<&'n Node>, path: &str, known: &[&str]) -> Fields<'n> {
        let mut out: Vec<(&str, &Node)> = Vec::new();
        let Some(node) = node.filter(|n| !n.is_null()) else {
            return Fields(out);
        };
        let NodeKind::Map(entries) = &node.kind else {
            let what = if path.is_empty() {
                "the document"
            } else {
                path
            };
            self.error(
                node,
                "",
                format!("{what} must be a mapping, found {}", node.describe()),
            );
            return Fields(out);
        };
        for (key, value) in entries {
            let NodeKind::Scalar { raw, .. } = &key.kind else {
                self.error(
                    key,
                    path,
                    format!("a field name must be a string, found {}", key.describe()),
                );
                continue;
            };
            if !known.contains(&raw.as_str()) {
                self.error(key, path, format!("unknown field {raw:?}"));
                continue;
            }
            if let Some((_, first)) = out.iter().find(|(k, _)| k == raw) {
                let first_line = first.line;
                self.error(
                    key,
                    &join(path, raw),
                    format!("field is already set at line {first_line}"),
                );
                continue;
            }
            out.push((raw, value));
        }
        Fields(out)
    }

    fn string(&mut self, node: Option<&Node>, path: &str) -> String {
        let Some(node) = node else {
            return String::new();
        };
        match &node.kind {
            NodeKind::Scalar {
                resolved: Scalar::Null,
                ..
            } => String::new(),
            NodeKind::Scalar { raw, resolved } => {
                if self.json && *resolved != Scalar::Str {
                    self.error(
                        node,
                        path,
                        format!("expected a string, found {}", node.describe()),
                    );
                    return String::new();
                }
                raw.clone()
            }
            _ => {
                self.error(
                    node,
                    path,
                    format!("expected a string, found {}", node.describe()),
                );
                String::new()
            }
        }
    }

    fn list<'n>(&mut self, node: Option<&'n Node>, path: &str) -> &'n [Node] {
        match node.map(|n| (n, &n.kind)) {
            Some((_, NodeKind::Seq(items))) => items,
            Some((n, _)) if !n.is_null() => {
                self.error(n, path, format!("expected a list, found {}", n.describe()));
                &[]
            }
            _ => &[],
        }
    }

    fn document(&mut self, root: &Node) -> Option<Document> {
        let pos = self.at(root);
        if !matches!(root.kind, NodeKind::Map(_)) && !root.is_null() {
            self.error(
                root,
                "",
                format!("the document must be a mapping, found {}", root.describe()),
            );
            return None;
        }

        // Route on the header first, so a document Meridian cannot safely
        // interpret is rejected before any of its body is decoded.
        let api_version = self.string(root.child("apiVersion"), "apiVersion");
        let kind = self.string(root.child("kind"), "kind");
        let unsupported = VersionTable::builtin().check(&api_version).err();
        let kind = match (api_version.as_str(), kind.as_str()) {
            ("", _) => Err(("apiVersion", "missing required field".to_owned())),
            (_, _) if unsupported.is_some() => Err((
                "apiVersion",
                unsupported.map(|e| e.to_string()).unwrap_or_default(),
            )),
            (_, "") => Err(("kind", "missing required field".to_owned())),
            (_, other) => schema::kind_named(other).ok_or_else(|| {
                (
                    "kind",
                    format!(
                        "unknown kind {other:?} (expected \"Infrastructure\" or \"ResourceSet\")"
                    ),
                )
            }),
        };
        let kind = match kind {
            Ok(kind) => kind,
            Err((path, msg)) => {
                self.errs.push(Error::new(pos, path, msg));
                return None;
            }
        };

        let top = self.fields(Some(root), "", &schema::document(kind).names());
        let metadata = self.metadata(top.get("metadata"));
        Some(match kind {
            Kind::Infrastructure => {
                let f = self.fields(
                    top.get("spec"),
                    "spec",
                    &schema::INFRASTRUCTURE_SPEC.names(),
                );
                let spec = InfrastructureSpec {
                    target: self.string(f.get("target"), "spec.target"),
                    target_version: self.string(f.get("targetVersion"), "spec.targetVersion"),
                    when: self.string(f.get("when"), "spec.when"),
                    variables: self.value_map(f.get("variables"), "spec.variables"),
                    conditions: self.conditions(f.get("conditions")),
                    secrets: self.secrets(f.get("secrets"), &pos),
                    imports: self.imports(f.get("imports"), &pos),
                    resources: self.resources(f.get("resources"), &pos),
                    outputs: self.outputs(f.get("outputs"), &pos),
                };
                Document::Infrastructure(Infrastructure {
                    api_version,
                    metadata,
                    spec,
                    pos,
                })
            }
            Kind::ResourceSet => {
                let f = self.fields(top.get("spec"), "spec", &schema::RESOURCE_SET_SPEC.names());
                let spec = ResourceSetSpec {
                    target: self.string(f.get("target"), "spec.target"),
                    target_version: self.string(f.get("targetVersion"), "spec.targetVersion"),
                    when: self.string(f.get("when"), "spec.when"),
                    variables: self.value_map(f.get("variables"), "spec.variables"),
                    conditions: self.conditions(f.get("conditions")),
                    secrets: self.secrets(f.get("secrets"), &pos),
                    target_host: self.target_host(f.get("targetHost"), &pos),
                    imports: self.imports(f.get("imports"), &pos),
                    resources: self.resources(f.get("resources"), &pos),
                };
                Document::ResourceSet(ResourceSet {
                    api_version,
                    metadata,
                    spec,
                    pos,
                })
            }
        })
    }

    fn metadata(&mut self, node: Option<&Node>) -> Metadata {
        let f = self.fields(node, "metadata", &schema::METADATA.names());
        let mut labels = BTreeMap::new();
        for (key, value) in self.entries(f.get("labels"), "metadata.labels") {
            let value = self.string(Some(value), &join("metadata.labels", key));
            labels.insert(key.to_owned(), value);
        }
        Metadata {
            name: self.string(f.get("name"), "metadata.name"),
            labels,
        }
    }

    /// `spec.conditions`: each name with its expression, as text.
    fn conditions(&mut self, node: Option<&Node>) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (key, value) in self.entries(node, "spec.conditions") {
            let text = self.string(Some(value), &join("spec.conditions", key));
            out.insert(key.to_owned(), text);
        }
        out
    }

    /// The entries of a free-form mapping, with a repeated key reported.
    fn entries<'n>(&mut self, node: Option<&'n Node>, path: &str) -> Vec<(&'n str, &'n Node)> {
        let mut out: Vec<(&str, &Node)> = Vec::new();
        let Some(node) = node.filter(|n| !n.is_null()) else {
            return out;
        };
        let NodeKind::Map(entries) = &node.kind else {
            self.error(
                node,
                path,
                format!("expected a mapping, found {}", node.describe()),
            );
            return out;
        };
        for (key, value) in entries {
            let NodeKind::Scalar { raw, .. } = &key.kind else {
                self.error(
                    key,
                    path,
                    format!("a key must be a scalar, found {}", key.describe()),
                );
                continue;
            };
            if let Some((_, first)) = out.iter().find(|(k, _)| k == raw) {
                let first_line = first.line;
                self.error(
                    key,
                    &join(path, raw),
                    format!("key is already defined at line {first_line}"),
                );
                continue;
            }
            out.push((raw, value));
        }
        out
    }

    fn value(&mut self, node: &Node, path: &str) -> Value {
        match &node.kind {
            NodeKind::Scalar { raw, resolved } => match *resolved {
                Scalar::Null => Value::Null,
                Scalar::Bool(b) => Value::Bool(b),
                Scalar::Int(i) => Value::Int(i),
                Scalar::Uint(u) => Value::Uint(u),
                Scalar::Float(f) => Value::Float(f),
                Scalar::Str => Value::String(raw.clone()),
            },
            NodeKind::Seq(items) => Value::List(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| self.value(item, &format!("{path}[{i}]")))
                    .collect(),
            ),
            NodeKind::Map(_) => Value::Map(self.value_map(Some(node), path)),
        }
    }

    fn value_map(&mut self, node: Option<&Node>, path: &str) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        for (key, value) in self.entries(node, path) {
            let value = self.value(value, &join(path, key));
            map.insert(key.to_owned(), value);
        }
        map
    }

    fn resources(&mut self, node: Option<&Node>, doc_pos: &Position) -> Vec<Resource> {
        let mut out = Vec::new();
        for (index, item) in self.list(node, "spec.resources").iter().enumerate() {
            let path = format!("spec.resources[{index}]");
            let f = self.fields(Some(item), &path, &schema::RESOURCE.names());
            let pos = self.at_or(item, doc_pos);

            let mut depends_on = Vec::new();
            let deps_path = join(&path, "dependsOn");
            for (j, dep) in self.list(f.get("dependsOn"), &deps_path).iter().enumerate() {
                let p = format!("{deps_path}[{j}]");
                let d = self.fields(Some(dep), &p, &schema::DEPENDENCY.names());
                depends_on.push(Dependency {
                    resource: self.string(d.get("resource"), &join(&p, "resource")),
                    if_missing: IfMissing::from_text(
                        &self.string(d.get("ifMissing"), &join(&p, "ifMissing")),
                    ),
                    when: self.string(d.get("when"), &join(&p, "when")),
                    pos: self.at_or(dep, &pos),
                });
            }

            let mut notifies = Vec::new();
            let notifies_path = join(&path, "notifies");
            for (j, n) in self
                .list(f.get("notifies"), &notifies_path)
                .iter()
                .enumerate()
            {
                let p = format!("{notifies_path}[{j}]");
                let d = self.fields(Some(n), &p, &schema::NOTIFICATION.names());
                notifies.push(Notification {
                    resource: self.string(d.get("resource"), &join(&p, "resource")),
                    on: self.string(d.get("on"), &join(&p, "on")),
                    action: self.string(d.get("action"), &join(&p, "action")),
                    if_missing: IfMissing::from_text(
                        &self.string(d.get("ifMissing"), &join(&p, "ifMissing")),
                    ),
                    when: self.string(d.get("when"), &join(&p, "when")),
                    pos: self.at_or(n, &pos),
                });
            }

            out.push(Resource {
                id: self.string(f.get("id"), &join(&path, "id")),
                r#type: self.string(f.get("type"), &join(&path, "type")),
                state: self.string(f.get("state"), &join(&path, "state")),
                params: self.value_map(f.get("params"), &join(&path, "params")),
                depends_on,
                notifies,
                when: self.string(f.get("when"), &join(&path, "when")),
                runtime_when: self.string(f.get("runtimeWhen"), &join(&path, "runtimeWhen")),
                index,
                pos,
            });
        }
        out
    }

    fn outputs(&mut self, node: Option<&Node>, doc_pos: &Position) -> Vec<Output> {
        let mut out = Vec::new();
        for (i, item) in self.list(node, "spec.outputs").iter().enumerate() {
            let path = format!("spec.outputs[{i}]");
            let f = self.fields(Some(item), &path, &schema::OUTPUT.names());
            out.push(Output {
                name: self.string(f.get("name"), &join(&path, "name")),
                value: self.string(f.get("value"), &join(&path, "value")),
                when: self.string(f.get("when"), &join(&path, "when")),
                pos: self.at_or(item, doc_pos),
            });
        }
        out
    }

    fn imports(&mut self, node: Option<&Node>, doc_pos: &Position) -> Vec<Import> {
        let mut out = Vec::new();
        for (i, item) in self.list(node, "spec.imports").iter().enumerate() {
            let path = format!("spec.imports[{i}]");
            let f = self.fields(Some(item), &path, &schema::IMPORT.names());
            out.push(Import {
                name: self.string(f.get("name"), &join(&path, "name")),
                from: self.string(f.get("from"), &join(&path, "from")),
                r#ref: self.string(f.get("ref"), &join(&path, "ref")),
                file: self.string(f.get("file"), &join(&path, "file")),
                output: self.string(f.get("output"), &join(&path, "output")),
                sensitive: self.flag(f.get("sensitive"), &join(&path, "sensitive")),
                pos: self.at_or(item, doc_pos),
            });
        }
        out
    }

    /// `spec.secrets`: where each secret comes from. Never a value.
    fn secrets(&mut self, node: Option<&Node>, doc_pos: &Position) -> Vec<Secret> {
        let mut out = Vec::new();
        for (i, item) in self.list(node, "spec.secrets").iter().enumerate() {
            let path = format!("spec.secrets[{i}]");
            let f = self.fields(Some(item), &path, &schema::SECRET.names());
            let first = self.secret_source(&f, &path);
            let fallback_path = join(&path, "fallback");
            let mut fallback = Vec::new();
            for (j, entry) in self
                .list(f.get("fallback"), &fallback_path)
                .iter()
                .enumerate()
            {
                let path = format!("{fallback_path}[{j}]");
                let f = self.fields(Some(entry), &path, &schema::SECRET_SOURCE.names());
                fallback.push(self.secret_source(&f, &path));
            }
            out.push(Secret {
                name: self.string(f.get("name"), &join(&path, "name")),
                from: first.from,
                key: first.key,
                path: first.path,
                command: first.command,
                service: first.service,
                account: first.account,
                attributes: first.attributes,
                optional: self.flag(f.get("optional"), &join(&path, "optional")),
                allow_wide_mode: first.allow_wide_mode,
                keep_trailing_newline: first.keep_trailing_newline,
                fallback,
                pos: self.at_or(item, doc_pos),
            });
        }
        out
    }

    /// One place a secret comes from: the fields of a `spec.secrets` entry
    /// or of one of its `fallback` entries.
    fn secret_source(&mut self, f: &Fields<'_>, path: &str) -> meridian_secret::Source {
        let command_path = join(path, "command");
        let command = self
            .list(f.get("command"), &command_path)
            .iter()
            .enumerate()
            .map(|(j, arg)| self.string(Some(arg), &format!("{command_path}[{j}]")))
            .collect();
        let attributes_path = join(path, "attributes");
        let mut attributes = BTreeMap::new();
        for (key, value) in self.entries(f.get("attributes"), &attributes_path) {
            let value = self.string(Some(value), &join(&attributes_path, key));
            attributes.insert(key.to_owned(), value);
        }
        meridian_secret::Source {
            from: self.string(f.get("from"), &join(path, "from")),
            key: self.string(f.get("key"), &join(path, "key")),
            path: self.string(f.get("path"), &join(path, "path")),
            command,
            service: self.string(f.get("service"), &join(path, "service")),
            account: self.string(f.get("account"), &join(path, "account")),
            attributes,
            allow_wide_mode: self.flag(f.get("allowWideMode"), &join(path, "allowWideMode")),
            keep_trailing_newline: self.flag(
                f.get("keepTrailingNewline"),
                &join(path, "keepTrailingNewline"),
            ),
        }
    }

    /// A yes or a no; left out, it is no.
    fn flag(&mut self, node: Option<&Node>, path: &str) -> bool {
        let Some(node) = node.filter(|n| !n.is_null()) else {
            return false;
        };
        match &node.kind {
            NodeKind::Scalar {
                resolved: Scalar::Bool(b),
                ..
            } => *b,
            _ => {
                self.error(
                    node,
                    path,
                    format!("expected true or false, found {}", node.describe()),
                );
                false
            }
        }
    }

    fn target_host(&mut self, node: Option<&Node>, doc_pos: &Position) -> Option<TargetHost> {
        let node = node.filter(|n| !n.is_null())?;
        let path = "spec.targetHost";
        let f = self.fields(Some(node), path, &schema::TARGET_HOST.names());
        Some(TargetHost {
            from: self.string(f.get("from"), &join(path, "from")),
            r#ref: self.string(f.get("ref"), &join(path, "ref")),
            output: self.string(f.get("output"), &join(path, "output")),
            host: self.string(f.get("host"), &join(path, "host")),
            pos: self.at_or(node, doc_pos),
        })
    }
}
