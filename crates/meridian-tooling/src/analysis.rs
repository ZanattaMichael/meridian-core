//! What the language server answers with: completion, hover, definitions,
//! references and the outline.
//!
//! Nothing about the language or any target is written here. Field names,
//! their order and their documentation come from [`meridian_ir::schema`],
//! the tables the parser itself reads; resource types, params, states and
//! actions come from what each loaded plugin declares. A field added to the
//! language, or a plugin added to the plugin directory, shows up in every
//! answer without a change to this module.

use meridian_ir::schema::{self, Field, Role, Shape, Type};
use meridian_ir::Kind;
use meridian_sdk::{ParamKind, ParamSchema, ResourceSchema};

use crate::catalog::{pattern_parts, Catalog, PatternPart, Target};
use crate::check::{Parsed, Workspace};
use crate::syntax::{indent_of, parse_path, Cursor, Node, Place, Pos, Seg, Span};

/// What a completion inserts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Field,
    Value,
    Reference,
    Type,
    Snippet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    /// A few words shown beside the label.
    pub detail: String,
    /// Markdown shown when the item is selected.
    pub doc: String,
    /// The text to insert, in place of `span`.
    pub insert: String,
    /// Whether `insert` uses snippet placeholders such as `$1`.
    pub snippet: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub span: Span,
    pub markdown: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub file: String,
    pub span: Span,
}

/// What kind of thing an outline entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Document,
    Resource,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub detail: String,
    pub kind: SymbolKind,
    /// The whole of what the symbol covers.
    pub span: Span,
    /// The name itself.
    pub selection: Span,
    pub children: Vec<Symbol>,
}

/// Where a path leads in the language.
#[derive(Debug, Default)]
struct Walk {
    /// The mapping the path ends inside, when it is one the language defines.
    shape: Option<&'static Shape>,
    /// The last field of the language the path went through.
    field: Option<&'static Field>,
    /// What follows after the path leaves the language, as inside `params`.
    beyond: Vec<Seg>,
    /// Whether the path left the language through a field it does not have.
    lost: bool,
}

fn walk(root: &'static Shape, path: &[Seg]) -> Walk {
    let mut out = Walk {
        shape: Some(root),
        ..Walk::default()
    };
    let mut list: Option<&'static Shape> = None;
    let mut free = false;
    for seg in path {
        if free {
            out.beyond.push(seg.clone());
            continue;
        }
        match seg {
            Seg::Key(key) => {
                let Some(field) = out.shape.and_then(|s| s.field(key)) else {
                    out.shape = None;
                    out.lost = true;
                    free = true;
                    continue;
                };
                out.field = Some(field);
                out.shape = None;
                list = None;
                match &field.ty {
                    Type::Object(next) => out.shape = Some(next),
                    Type::List(next) => list = Some(next),
                    Type::Params | Type::StringMap => free = true,
                    Type::String | Type::Enum(_) | Type::Bool | Type::StringList => {}
                }
            }
            Seg::Index(_) => {
                out.shape = list.take();
                if out.shape.is_none() {
                    out.lost = true;
                    free = true;
                }
            }
        }
    }
    out
}

/// One resource as the text declares it, parsed or not.
struct Declared<'a> {
    id: &'a str,
    r#type: &'a str,
    node: &'a Node,
    id_span: Option<Span>,
    line: usize,
}

/// The document the cursor is in.
struct Scope<'a> {
    file: &'a Parsed,
    root: Option<&'a Node>,
    kind: Option<Kind>,
    shape: &'static Shape,
    target_name: &'a str,
    target: Option<&'a Target>,
}

impl<'a> Scope<'a> {
    fn new(file: &'a Parsed, doc: usize, catalog: &'a Catalog) -> Scope<'a> {
        let root = file.file.docs.get(doc).and_then(|d| d.root.as_ref());
        let kind = root
            .and_then(|r| r.text("kind"))
            .and_then(schema::kind_named);
        let target_name = root
            .and_then(|r| r.get(&parse_path("spec.target")))
            .and_then(Node::scalar)
            .map_or("", |s| s.text.as_str());
        Scope {
            file,
            root,
            kind,
            // Until a kind is written the header fields are the same for
            // every kind, so any shape serves.
            shape: schema::document(kind.unwrap_or(Kind::ResourceSet)),
            target_name,
            target: catalog.target(target_name),
        }
    }

    fn resources(&self) -> Vec<Declared<'a>> {
        let Some(list) = self.root.and_then(|r| r.get(&parse_path("spec.resources"))) else {
            return Vec::new();
        };
        list.items()
            .iter()
            .filter_map(|item| {
                let node = item.value.as_ref()?;
                Some(Declared {
                    id: node.text("id").unwrap_or(""),
                    r#type: node.text("type").unwrap_or(""),
                    node,
                    id_span: node.child("id").and_then(Node::scalar).map(|s| s.span),
                    line: item.line,
                })
            })
            .collect()
    }

    fn declared(&self, id: &str) -> Option<Declared<'a>> {
        self.resources().into_iter().find(|r| r.id == id)
    }

    /// The resource a path is inside, when it is inside one.
    fn resource_at(&self, path: &[Seg]) -> Option<&'a Node> {
        match path {
            [Seg::Key(spec), Seg::Key(resources), Seg::Index(_), ..]
                if spec == "spec" && resources == "resources" =>
            {
                self.root?.get(&path[..3])
            }
            _ => None,
        }
    }

    fn schema_of(&self, type_name: &str) -> Option<&'a ResourceSchema> {
        self.target?.resource(type_name)
    }
}

fn code(text: &str) -> String {
    format!("`{text}`")
}

fn type_words(ty: &Type) -> &'static str {
    match ty {
        Type::String => "string",
        Type::Enum(_) => "one of a fixed set",
        Type::Object(_) => "mapping",
        Type::List(_) => "list",
        Type::StringMap => "mapping of names to strings",
        Type::Params => "mapping",
        Type::Bool => "true or false",
        Type::StringList => "list of strings",
    }
}

fn field_markdown(path: &str, field: &Field) -> String {
    let mut out = format!(
        "**{}** · {} · {}\n\n{}",
        code(path),
        if field.required {
            "required"
        } else {
            "optional"
        },
        type_words(&field.ty),
        field.doc
    );
    if let Type::Enum(choices) = &field.ty {
        out.push('\n');
        for choice in *choices {
            out.push_str(&format!("\n- {}: {}", code(choice.value), choice.doc));
        }
    }
    out
}

fn param_markdown(target: &str, type_name: &str, param: &meridian_sdk::ParamSchema) -> String {
    let mut out = format!(
        "**{}** · {} · {}\n\nParam of {} on target {}.",
        code(&param.name),
        if param.required {
            "required"
        } else {
            "optional"
        },
        param.kind.as_str(),
        code(type_name),
        code(target)
    );
    if !param.description.is_empty() {
        out.push_str(&format!("\n\n{}", param.description));
    }
    if !param.values.is_empty() {
        let values: Vec<String> = param.values.iter().map(|v| code(v)).collect();
        out.push_str(&format!("\n\nAccepts: {}", values.join(", ")));
    }
    out
}

fn type_markdown(target: &Target, schema: &ResourceSchema) -> String {
    let mut out = format!(
        "**{}** · resource type of target {}",
        code(&schema.r#type),
        code(&target.name)
    );
    if !schema.description.is_empty() {
        out.push_str(&format!("\n\n{}", schema.description));
    }
    if !schema.params.is_empty() {
        out.push_str("\n\n| Param | Value | |\n|---|---|---|");
        for p in &schema.params {
            out.push_str(&format!(
                "\n| {} | {} | {}{} |",
                code(&p.name),
                p.kind.as_str(),
                if p.required { "**required.** " } else { "" },
                p.description.replace('|', "\\|")
            ));
        }
    }
    if schema.open {
        out.push_str(
            "\n\nParams are passed through to the tool, so any param the tool takes is accepted.",
        );
    }
    if !schema.states.is_empty() {
        let states: Vec<String> = schema
            .states
            .iter()
            .map(|s| {
                if *s == schema.default_state {
                    format!("{} (default)", code(s))
                } else {
                    code(s)
                }
            })
            .collect();
        out.push_str(&format!("\n\nStates: {}", states.join(", ")));
    } else if !schema.open {
        out.push_str("\n\nTakes no state.");
    }
    if !schema.actions.is_empty() {
        let actions: Vec<String> = schema.actions.iter().map(|a| code(a)).collect();
        out.push_str(&format!("\n\nCan be notified to: {}", actions.join(", ")));
    }
    out
}

fn target_markdown(target: &Target) -> String {
    let mut out = format!(
        "**{}** · {} target\n\nCan: {}",
        code(&target.name),
        target.category,
        target.can().join(", ")
    );
    if !target.versions.is_empty() {
        let versions: Vec<String> = target.versions.iter().map(|v| code(v)).collect();
        out.push_str(&format!("\n\nTool versions: {}", versions.join(", ")));
    }
    if target.knows_types() {
        let types: Vec<String> = target.types.iter().map(|t| code(&t.r#type)).collect();
        out.push_str(&format!("\n\nResource types: {}", types.join(", ")));
    }
    out
}

fn resource_markdown(r: &Declared<'_>) -> String {
    let mut out = format!("**{}** · resource of type {}", code(r.id), code(r.r#type));
    if let Some(state) = r.node.text("state") {
        out.push_str(&format!(" · state {}", code(state)));
    }
    let params: Vec<String> = r
        .node
        .child("params")
        .map(|p| {
            p.entries()
                .iter()
                .filter_map(|e| {
                    let value = e.value.as_ref()?.scalar()?;
                    (value.style != crate::syntax::Style::Block)
                        .then(|| format!("- {}: {}", code(&e.key), code(&value.text)))
                })
                .collect()
        })
        .unwrap_or_default();
    if !params.is_empty() {
        out.push_str(&format!("\n\n{}", params.join("\n")));
    }
    out.push_str(&format!("\n\nDeclared on line {}.", r.line + 1));
    out
}

/// A `${...}` reference the cursor is inside: the text between `${` and the
/// cursor, and where it starts.
fn open_reference(line: &str, col: usize) -> Option<(usize, &str)> {
    let before = line.get(..col)?;
    let open = before.rfind("${")?;
    // `$${` is a literal `${`, not a reference.
    if open > 0 && before.as_bytes()[open - 1] == b'$' {
        return None;
    }
    let inside = &before[open + 2..];
    if inside.contains('}') {
        return None;
    }
    Some((open + 2, inside))
}

/// The `${...}` reference that covers `col`, whole: its text and span.
fn reference_at(line: &str, line_no: usize, col: usize) -> Option<(&str, Span)> {
    let mut from = 0;
    while let Some(found) = line[from..].find("${") {
        let open = from + found;
        let close = open + line[open..].find('}')?;
        let escaped = open > 0 && line.as_bytes()[open - 1] == b'$';
        if !escaped && open <= col && col <= close {
            return Some((
                &line[open + 2..close],
                Span::on_line(line_no, open + 2, close),
            ));
        }
        from = close + 1;
    }
    None
}

/// The leading name of a reference: `web_vm` of `web_vm.public_ip`.
fn head(expr: &str) -> &str {
    let end = expr
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(expr.len());
    &expr[..end]
}

struct Completer<'a> {
    scope: Scope<'a>,
    workspace: &'a Workspace,
    catalog: &'a Catalog,
    cursor: Cursor,
    /// Spaces to add to the lines a multi-line insert continues on, for a
    /// key that sits after a list dash.
    hang: usize,
    out: Vec<Completion>,
}

impl Completer<'_> {
    fn push(&mut self, label: &str, kind: CompletionKind, detail: &str, doc: &str, insert: &str) {
        self.out.push(Completion {
            label: label.to_owned(),
            kind,
            detail: detail.to_owned(),
            doc: doc.to_owned(),
            insert: insert.to_owned(),
            snippet: false,
            span: self.cursor.span,
        });
    }

    fn snippet(
        &mut self,
        label: &str,
        kind: CompletionKind,
        detail: &str,
        doc: &str,
        insert: &str,
    ) {
        // An editor indents continuation lines to the current line's own
        // indentation; a key after a dash sits further in than that.
        let pad = " ".repeat(self.hang);
        let insert = insert.replace('\n', &format!("\n{pad}"));
        self.push(label, kind, detail, doc, &insert);
        if let Some(last) = self.out.last_mut() {
            last.snippet = true;
        }
    }

    fn value(&mut self, label: &str, detail: &str, doc: &str) {
        self.push(label, CompletionKind::Value, detail, doc, label);
    }

    /// The fields of a shape that the mapping does not have yet.
    fn fields(&mut self, shape: &'static Shape, path: &str, present: &[String], bare: bool) {
        for field in shape.fields {
            if present.iter().any(|k| k == field.name) && self.cursor.word != field.name {
                continue;
            }
            let full = if path.is_empty() {
                field.name.to_owned()
            } else {
                format!("{path}.{}", field.name)
            };
            let doc = field_markdown(&full, field);
            let detail = if field.required { "required" } else { "" };
            if bare {
                self.push(field.name, CompletionKind::Field, detail, &doc, field.name);
                continue;
            }
            let insert = match &field.ty {
                Type::String => format!("{}: $0", field.name),
                Type::Enum(choices) if choices.len() == 1 => {
                    format!("{}: {}", field.name, choices[0].value)
                }
                Type::Enum(choices) => {
                    let values: Vec<&str> = choices.iter().map(|c| c.value).collect();
                    format!("{}: ${{1|{}|}}", field.name, values.join(","))
                }
                Type::Object(_) | Type::Params | Type::StringMap => {
                    format!("{}:\n  $0", field.name)
                }
                Type::List(item) => {
                    let first = item.fields.first().map_or("", |f| f.name);
                    format!("{}:\n  - {first}: $0", field.name)
                }
                Type::Bool => format!("{}: ${{1|true,false|}}", field.name),
                Type::StringList => format!("{}:\n  - $0", field.name),
            };
            self.snippet(field.name, CompletionKind::Field, detail, &doc, &insert);
        }
    }

    fn keys(&mut self) {
        let path = self.cursor.path.clone();
        let w = walk(self.scope.shape, &path);
        let node = self.scope.root.and_then(|r| r.get(&path));
        let present: Vec<String> = node
            .map(|n| n.entries().iter().map(|e| e.key.clone()).collect())
            .unwrap_or_default();
        // A key that already has its colon is being renamed, not started.
        let bare = self
            .scope
            .file
            .file
            .lines
            .get(self.cursor.span.start.line)
            .is_some_and(|l| {
                l[self.cursor.span.end.col.min(l.len())..]
                    .trim_start()
                    .starts_with(':')
            });

        if let (Some(shape), true) = (w.shape, w.beyond.is_empty()) {
            let shown = crate::syntax::show_path(&path);
            // Index steps are noise in a field's name.
            let shown = strip_indexes(&shown);
            if path.is_empty() && present.is_empty() {
                self.documents();
            }
            if shape.name == schema::RESOURCE.name && present.is_empty() {
                self.resource_snippets();
            }
            // With no kind written, `spec` could be either kind's; offer
            // what both have rather than guess.
            if self.scope.kind.is_none() && shape.name == schema::RESOURCE_SET_SPEC.name {
                self.fields(shape, &shown, &present, bare);
                let extra: Vec<&Field> = schema::INFRASTRUCTURE_SPEC
                    .fields
                    .iter()
                    .filter(|f| shape.field(f.name).is_none())
                    .collect();
                for field in extra {
                    let doc = field_markdown(&format!("{shown}.{}", field.name), field);
                    self.push(
                        field.name,
                        CompletionKind::Field,
                        "Infrastructure only",
                        &doc,
                        &format!("{}: ", field.name),
                    );
                }
                return;
            }
            self.fields(shape, &shown, &present, bare);
            return;
        }
        if w.field.map(|f| f.role) == Some(Role::ResourceParams) && w.beyond.is_empty() {
            self.params(&path, &present, bare);
        }
    }

    /// The params the resource's type takes that are not set yet.
    fn params(&mut self, path: &[Seg], present: &[String], bare: bool) {
        let Some(resource) = self.scope.resource_at(path) else {
            return;
        };
        let type_name = resource.text("type").unwrap_or("");
        let Some(schema) = self.scope.schema_of(type_name) else {
            return;
        };
        let target = self.scope.target_name.to_owned();
        // What must be given is offered first; the rest keep the order the
        // plugin lists them in.
        let mut params: Vec<&ParamSchema> = schema.params.iter().collect();
        params.sort_by_key(|p| !p.required);
        for param in params {
            if present.contains(&param.name) && self.cursor.word != param.name {
                continue;
            }
            let doc = param_markdown(&target, type_name, param);
            let detail = if param.required {
                format!("required · {}", param.kind.as_str())
            } else {
                param.kind.as_str().to_owned()
            };
            if bare {
                self.push(
                    &param.name,
                    CompletionKind::Field,
                    &detail,
                    &doc,
                    &param.name,
                );
                continue;
            }
            let insert = match param.kind {
                ParamKind::Bool => format!("{}: ${{1|true,false|}}", param.name),
                ParamKind::List => format!("{}:\n  - $0", param.name),
                ParamKind::Map => format!("{}:\n  $0", param.name),
                // Quoted, so a mode is never read as a number.
                _ if param.name == "mode" => format!("{}: \"${{1:0644}}\"", param.name),
                _ if !param.values.is_empty() => {
                    format!("{}: ${{1|{}|}}", param.name, param.values.join(","))
                }
                _ => format!("{}: $0", param.name),
            };
            self.snippet(&param.name, CompletionKind::Field, &detail, &doc, &insert);
        }
    }

    /// Whole documents, offered in an empty file.
    fn documents(&mut self) {
        let target = |kind: Kind| {
            self.catalog
                .targets()
                .find(|t| t.refuses_kind(kind).is_none())
                .map_or("target", |t| t.name.as_str())
                .to_owned()
        };
        let api = schema::API_VERSIONS[0].value;
        let set = format!(
            "apiVersion: {api}\nkind: ResourceSet\nmetadata:\n  name: ${{1:name}}\nspec:\n  \
             target: ${{2:{}}}\n  targetHost:\n    from: inline\n    host: ${{3:host.example.com}}\n  \
             resources:\n    - id: ${{4:id}}\n      type: $0",
            target(Kind::ResourceSet)
        );
        let infra = format!(
            "apiVersion: {api}\nkind: Infrastructure\nmetadata:\n  name: ${{1:name}}\nspec:\n  \
             target: ${{2:{}}}\n  resources:\n    - id: ${{3:id}}\n      type: $0",
            target(Kind::Infrastructure)
        );
        self.snippet(
            "ResourceSet document",
            CompletionKind::Snippet,
            "a new document",
            schema::KINDS[1].doc,
            &set,
        );
        self.snippet(
            "Infrastructure document",
            CompletionKind::Snippet,
            "a new document",
            schema::KINDS[0].doc,
            &infra,
        );
    }

    /// A whole resource of each type the target describes, with its
    /// required params laid out.
    fn resource_snippets(&mut self) {
        let Some(target) = self.scope.target else {
            return;
        };
        let described: Vec<&ResourceSchema> = target
            .types
            .iter()
            .filter(|t| !t.is_pattern() && !t.params.is_empty())
            .collect();
        for t in described {
            let mut insert = format!("id: ${{1:{}_id}}\ntype: {}", t.r#type, t.r#type);
            let required: Vec<&str> = t
                .params
                .iter()
                .filter(|p| p.required)
                .map(|p| p.name.as_str())
                .collect();
            if !required.is_empty() {
                insert.push_str("\nparams:");
                for (i, name) in required.iter().enumerate() {
                    insert.push_str(&format!("\n  {name}: ${}", i + 2));
                }
            }
            let doc = type_markdown(target, t);
            self.snippet(
                &format!("{} resource", t.r#type),
                CompletionKind::Snippet,
                &target.name,
                &doc,
                &insert,
            );
        }
    }

    fn resource_ids(&mut self, except: Option<&str>) {
        for r in self.scope.resources() {
            if r.id.is_empty() || Some(r.id) == except {
                continue;
            }
            let doc = resource_markdown(&r);
            self.push(r.id, CompletionKind::Reference, r.r#type, &doc, r.id);
        }
    }

    fn values(&mut self) {
        let path = self.cursor.path.clone();
        let w = walk(self.scope.shape, &path);
        let Some(field) = w.field else { return };
        if w.lost {
            return;
        }
        let resource = self.scope.resource_at(&path);

        if !w.beyond.is_empty() {
            // A value inside params.
            if field.role != Role::ResourceParams || w.beyond.len() != 1 {
                return;
            }
            let Seg::Key(name) = &w.beyond[0] else { return };
            let type_name = resource.and_then(|r| r.text("type")).unwrap_or("");
            let Some(param) = self
                .scope
                .schema_of(type_name)
                .and_then(|s| s.find_param(name))
            else {
                return;
            };
            if param.kind == ParamKind::Bool {
                self.value("true", "", "");
                self.value("false", "", "");
            }
            for v in &param.values {
                self.value(v, "", "");
            }
            return;
        }

        if let Type::Enum(choices) = &field.ty {
            for choice in *choices {
                self.value(choice.value, "", choice.doc);
            }
            return;
        }
        match field.role {
            Role::Target => {
                let targets: Vec<&Target> = self.catalog.targets().collect();
                for t in targets {
                    // A target that cannot take this kind of document is
                    // left out, not offered and then refused.
                    if self.scope.kind.is_some_and(|k| t.refuses_kind(k).is_some()) {
                        continue;
                    }
                    self.value(&t.name, t.category.as_str(), &target_markdown(t));
                }
            }
            Role::TargetVersion => {
                let versions = self
                    .scope
                    .target
                    .map(|t| t.versions.clone())
                    .unwrap_or_default();
                for v in versions {
                    self.value(&v, self.scope.target_name, "");
                }
            }
            Role::ResourceType => {
                let Some(target) = self.scope.target else {
                    return;
                };
                for t in &target.types {
                    let doc = type_markdown(target, t);
                    if !t.is_pattern() {
                        self.push(
                            &t.r#type,
                            CompletionKind::Type,
                            &target.name,
                            &doc,
                            &t.r#type,
                        );
                        continue;
                    }
                    // A pattern becomes a snippet with a stop per placeholder.
                    let mut insert = String::new();
                    let mut stop = 0;
                    for part in pattern_parts(&t.r#type) {
                        match part {
                            PatternPart::Literal(text) => insert.push_str(text),
                            PatternPart::Placeholder(name) => {
                                stop += 1;
                                insert.push_str(&format!("${{{stop}:{name}}}"));
                            }
                        }
                    }
                    self.snippet(&t.r#type, CompletionKind::Type, "pattern", &doc, &insert);
                }
            }
            Role::ResourceState => {
                let type_name = resource.and_then(|r| r.text("type")).unwrap_or("");
                let Some(schema) = self.scope.schema_of(type_name) else {
                    return;
                };
                for state in &schema.states {
                    let detail = if *state == schema.default_state {
                        "default"
                    } else {
                        ""
                    };
                    self.value(state, detail, "");
                }
            }
            Role::ResourceRef => {
                let own = resource.and_then(|r| r.text("id")).map(str::to_owned);
                self.resource_ids(own.as_deref());
            }
            Role::NotifyAction => {
                // The actions of whatever this notify names.
                let notified = self
                    .scope
                    .root
                    .and_then(|r| r.get(&path[..path.len() - 1]))
                    .and_then(|n| n.text("resource"))
                    .and_then(|id| self.scope.declared(id))
                    .map(|r| r.r#type.to_owned());
                let Some(schema) = notified.as_deref().and_then(|t| self.scope.schema_of(t)) else {
                    return;
                };
                for action in &schema.actions {
                    self.value(action, &schema.r#type, "");
                }
            }
            Role::InfrastructureRef => {
                for file in &self.workspace.files {
                    for d in &file.docs {
                        if d.doc.kind() == Kind::Infrastructure {
                            let detail = format!("target {}", d.doc.target());
                            self.push(
                                d.doc.name(),
                                CompletionKind::Reference,
                                &detail,
                                &format!("Infrastructure document in `{}`.", file.name),
                                d.doc.name(),
                            );
                        }
                    }
                }
            }
            Role::OutputRef => {
                let named = self
                    .scope
                    .root
                    .and_then(|r| r.get(&path[..path.len() - 1]))
                    .and_then(|n| n.text("ref"))
                    .unwrap_or("");
                let from = self.scope.file.name.clone();
                let found = self.workspace.infrastructure(&from, named);
                if let Some(infra) = found.and_then(|(_, d)| d.doc.as_infrastructure()) {
                    for o in &infra.spec.outputs {
                        self.push(
                            &o.name,
                            CompletionKind::Reference,
                            named,
                            &format!("Output of {}: {}", code(named), code(&o.value)),
                            &o.name,
                        );
                    }
                }
            }
            Role::Condition => {
                self.snippet(
                    "data.",
                    CompletionKind::Snippet,
                    "a value given with --data or --set",
                    "Reads a value from the data, as in \
                     `data.env == 'prod'`. Give them with `--data` or `--set`.",
                    "data.${1:key} == '${2:value}'",
                );
                self.snippet(
                    "defined()",
                    CompletionKind::Snippet,
                    "whether a value was given",
                    "True when the named value was given with `--data`.",
                    "defined(data.${1:key})",
                );
                // What the document itself declares for a condition to read.
                let spec = self.scope.root.and_then(|r| r.child("spec"));
                let mut declared: Vec<(String, &str)> = Vec::new();
                for (prefix, field, what) in [
                    ("cond", "conditions", "a named condition of this document"),
                    ("var", "variables", "a variable of this document"),
                ] {
                    for entry in spec
                        .and_then(|s| s.child(field))
                        .map_or(&[][..], Node::entries)
                    {
                        declared.push((format!("{prefix}.{}", entry.key), what));
                    }
                }
                for item in spec
                    .and_then(|s| s.child("secrets"))
                    .map_or(&[][..], Node::items)
                {
                    if let Some(name) = item.value.as_ref().and_then(|v| v.text("name")) {
                        declared.push((format!("secret.{name}"), "a secret of this document"));
                    }
                }
                for (label, what) in declared {
                    self.value(&label, what, "");
                }
            }
            _ => {}
        }
    }

    /// Inside `${`: resource ids, then the params of the one named.
    fn reference(&mut self, start: usize, inside: &str) -> bool {
        let line = self.cursor.span.start.line;
        let col = start + inside.len();
        match inside.rfind('.') {
            None => {
                self.cursor.span = Span::on_line(line, start, col);
                self.resource_ids(None);
            }
            Some(dot) => {
                self.cursor.span = Span::on_line(line, start + dot + 1, col);
                let Some(r) = self.scope.declared(head(inside)) else {
                    return true;
                };
                let keys: Vec<(String, String)> = r
                    .node
                    .child("params")
                    .map(|p| {
                        p.entries()
                            .iter()
                            .map(|e| {
                                let value = e
                                    .value
                                    .as_ref()
                                    .and_then(Node::scalar)
                                    .map_or(String::new(), |s| s.text.clone());
                                (e.key.clone(), value)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for (key, value) in keys {
                    self.push(&key, CompletionKind::Reference, &value, "", &key);
                }
            }
        }
        true
    }
}

fn strip_indexes(path: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in path.chars() {
        match c {
            '[' => skip = true,
            ']' => skip = false,
            _ if !skip => out.push(c),
            _ => {}
        }
    }
    out
}

/// What can be typed at `pos` in the file called `name`.
pub fn complete(workspace: &Workspace, catalog: &Catalog, name: &str, pos: Pos) -> Vec<Completion> {
    let Some(file) = workspace.file(name) else {
        return Vec::new();
    };
    let Some(mut cursor) = file.file.cursor(pos) else {
        return Vec::new();
    };
    let line = file.file.lines.get(pos.line).map_or("", String::as_str);
    let scope = Scope::new(file, cursor.doc, catalog);

    // A lone word under a field that holds a mapping is the start of a key
    // inside it, not the field's value.
    if cursor.place == Place::Value {
        let w = walk(scope.shape, &cursor.path);
        let holds_mapping = matches!(
            w.field.map(|f| &f.ty),
            Some(Type::Object(_) | Type::Params | Type::StringMap)
        ) && w.beyond.is_empty();
        let item = matches!(cursor.path.last(), Some(Seg::Index(_))) && w.shape.is_some();
        let own_line = scope
            .root
            .and_then(|r| r.get(&cursor.path))
            .is_some_and(|n| n.scalar().is_some())
            && !line[..cursor.span.start.col.min(line.len())].contains(':');
        if (holds_mapping || item) && (own_line || cursor.word.is_empty()) && !line.contains(": ") {
            cursor.place = Place::Key;
        }
    }

    let hang = cursor.span.start.col.saturating_sub(indent_of(line));
    let mut completer = Completer {
        scope,
        workspace,
        catalog,
        cursor,
        hang,
        out: Vec::new(),
    };
    if let Some((start, inside)) = open_reference(line, pos.col) {
        completer.reference(start, inside);
        return completer.out;
    }
    match completer.cursor.place {
        Place::Key => completer.keys(),
        Place::Value => completer.values(),
    }
    completer.out
}

/// What the thing at `pos` refers to.
enum Referent<'a> {
    Resource(Declared<'a>),
    Infrastructure(String),
    Output { infra: String, name: String },
}

/// Finds the reference under the cursor: a `dependsOn` or `notifies` target,
/// a `${...}` reference, or a `targetHost` ref or output.
fn referent<'a>(scope: &Scope<'a>, cursor: &Cursor, pos: Pos) -> Option<(Referent<'a>, Span)> {
    let line = scope.file.file.lines.get(pos.line)?;
    if let Some((expr, span)) = reference_at(line, pos.line, pos.col) {
        let name = head(expr);
        let r = scope.declared(name)?;
        let span = Span::on_line(span.start.line, span.start.col, span.start.col + name.len());
        return Some((Referent::Resource(r), span));
    }
    if cursor.place != Place::Value {
        return None;
    }
    let w = walk(scope.shape, &cursor.path);
    let field = w.field.filter(|_| w.beyond.is_empty() && !w.lost)?;
    let parent = scope.root?.get(&cursor.path[..cursor.path.len() - 1]);
    match field.role {
        Role::ResourceRef => {
            let r = scope.declared(&cursor.word)?;
            Some((Referent::Resource(r), cursor.span))
        }
        Role::InfrastructureRef => {
            Some((Referent::Infrastructure(cursor.word.clone()), cursor.span))
        }
        Role::OutputRef => Some((
            Referent::Output {
                infra: parent?.text("ref")?.to_owned(),
                name: cursor.word.clone(),
            },
            cursor.span,
        )),
        _ => None,
    }
}

/// Where an Infrastructure document, or one of its outputs, is declared.
fn infrastructure(
    workspace: &Workspace,
    from: &str,
    name: &str,
    output: Option<&str>,
) -> Option<Location> {
    let (file, d) = workspace.infrastructure(from, name)?;
    let path = match output {
        None => "metadata.name".to_owned(),
        Some(output) => {
            let infra = d.doc.as_infrastructure()?;
            let index = infra.spec.outputs.iter().position(|o| o.name == output)?;
            format!("spec.outputs[{index}].name")
        }
    };
    let located = file.file.locate(d.index, &parse_path(&path))?;
    Some(Location {
        file: file.name.clone(),
        span: located.span(&file.file),
    })
}

/// Documentation for whatever is at `pos`.
pub fn hover(workspace: &Workspace, catalog: &Catalog, name: &str, pos: Pos) -> Option<Hover> {
    let file = workspace.file(name)?;
    let cursor = file.file.cursor(pos)?;
    let scope = Scope::new(file, cursor.doc, catalog);

    if let Some((referent, span)) = referent(&scope, &cursor, pos) {
        let markdown = match referent {
            Referent::Resource(r) => resource_markdown(&r),
            Referent::Infrastructure(infra) => {
                let doc = workspace
                    .infrastructure(name, &infra)?
                    .1
                    .doc
                    .as_infrastructure()?;
                let outputs: Vec<String> = doc.spec.outputs.iter().map(|o| code(&o.name)).collect();
                format!(
                    "**{}** · Infrastructure document · target {}\n\nOutputs: {}",
                    code(&infra),
                    code(&doc.spec.target),
                    if outputs.is_empty() {
                        "none".to_owned()
                    } else {
                        outputs.join(", ")
                    }
                )
            }
            Referent::Output {
                infra,
                name: output,
            } => {
                let doc = workspace
                    .infrastructure(name, &infra)?
                    .1
                    .doc
                    .as_infrastructure()?;
                let output = doc.output(&output)?;
                format!(
                    "**{}** · output of {}\n\nValue: {}",
                    code(&output.name),
                    code(&infra),
                    code(&output.value)
                )
            }
        };
        return Some(Hover { span, markdown });
    }

    let w = walk(scope.shape, &cursor.path);
    let shown = strip_indexes(&crate::syntax::show_path(&cursor.path));
    match cursor.place {
        Place::Key => {
            if w.beyond.is_empty() {
                if let Some(field) = w.shape.and_then(|s| s.field(&cursor.word)) {
                    let path = if shown.is_empty() {
                        field.name.to_owned()
                    } else {
                        format!("{shown}.{}", field.name)
                    };
                    return Some(Hover {
                        span: cursor.span,
                        markdown: field_markdown(&path, field),
                    });
                }
            }
            if w.field.map(|f| f.role) == Some(Role::ResourceParams) && w.beyond.is_empty() {
                let type_name = scope.resource_at(&cursor.path)?.text("type")?;
                let param = scope.schema_of(type_name)?.find_param(&cursor.word)?;
                return Some(Hover {
                    span: cursor.span,
                    markdown: param_markdown(scope.target_name, type_name, param),
                });
            }
            None
        }
        Place::Value => {
            let field = w.field.filter(|_| w.beyond.is_empty() && !w.lost)?;
            let markdown = match (&field.ty, field.role) {
                (Type::Enum(choices), _) => {
                    let choice = choices.iter().find(|c| c.value == cursor.word)?;
                    format!("**{}**\n\n{}", code(choice.value), choice.doc)
                }
                (_, Role::Target) => target_markdown(catalog.target(&cursor.word)?),
                (_, Role::ResourceType) => {
                    type_markdown(scope.target?, scope.schema_of(&cursor.word)?)
                }
                (_, Role::ResourceState) => {
                    let type_name = scope.resource_at(&cursor.path)?.text("type")?;
                    let schema = scope.schema_of(type_name)?;
                    if !schema.states.contains(&cursor.word) {
                        return None;
                    }
                    format!(
                        "**{}** · state of {}{}",
                        code(&cursor.word),
                        code(type_name),
                        if cursor.word == schema.default_state {
                            " · the default"
                        } else {
                            ""
                        }
                    )
                }
                _ => return None,
            };
            Some(Hover {
                span: cursor.span,
                markdown,
            })
        }
    }
}

/// Where the thing at `pos` is declared.
pub fn definition(
    workspace: &Workspace,
    catalog: &Catalog,
    name: &str,
    pos: Pos,
) -> Option<Location> {
    let file = workspace.file(name)?;
    let cursor = file.file.cursor(pos)?;
    let scope = Scope::new(file, cursor.doc, catalog);
    let (referent, _) = referent(&scope, &cursor, pos)?;
    match referent {
        Referent::Resource(r) => Some(Location {
            file: name.to_owned(),
            span: r.id_span?,
        }),
        Referent::Infrastructure(infra) => infrastructure(workspace, name, &infra, None),
        Referent::Output {
            infra,
            name: output,
        } => infrastructure(workspace, name, &infra, Some(&output)),
    }
}

/// Every place the resource at `pos` is named: its declaration, the
/// `dependsOn` and `notifies` entries that name it, and `${...}` references.
/// Empty when `pos` is not on a resource's id or a reference to one.
pub fn references(workspace: &Workspace, catalog: &Catalog, name: &str, pos: Pos) -> Vec<Location> {
    let Some(file) = workspace.file(name) else {
        return Vec::new();
    };
    let Some(cursor) = file.file.cursor(pos) else {
        return Vec::new();
    };
    let scope = Scope::new(file, cursor.doc, catalog);
    let id = match referent(&scope, &cursor, pos) {
        Some((Referent::Resource(r), _)) => r.id.to_owned(),
        Some(_) => return Vec::new(),
        None => {
            let w = walk(scope.shape, &cursor.path);
            let on_id = cursor.place == Place::Value
                && w.field.map(|f| f.role) == Some(Role::ResourceId)
                && w.beyond.is_empty();
            if !on_id || cursor.word.is_empty() {
                return Vec::new();
            }
            cursor.word.clone()
        }
    };

    let mut spans = Vec::new();
    for r in scope.resources() {
        if r.id == id {
            spans.extend(r.id_span);
        }
        for list in ["dependsOn", "notifies"] {
            for item in r.node.child(list).map_or(&[][..], Node::items) {
                let named = item
                    .value
                    .as_ref()
                    .and_then(|v| v.child("resource"))
                    .and_then(Node::scalar);
                if let Some(s) = named.filter(|s| s.text == id) {
                    spans.push(s.span);
                }
            }
        }
    }
    // `${id.` and `${id}` anywhere in the document.
    if let Some(doc) = file.file.docs.get(cursor.doc) {
        let lines = file.file.lines.iter().enumerate();
        for (line_no, line) in lines.take(doc.end_line).skip(doc.first_line) {
            let mut from = 0;
            while let Some(found) = line[from..].find("${") {
                let start = from + found + 2;
                let escaped = start >= 3 && line.as_bytes()[start - 3] == b'$';
                if !escaped && head(&line[start..]) == id {
                    spans.push(Span::on_line(line_no, start, start + id.len()));
                }
                from = start;
            }
        }
    }
    spans.sort_by_key(|s| s.start);
    spans.dedup();
    spans
        .into_iter()
        .map(|span| Location {
            file: name.to_owned(),
            span,
        })
        .collect()
}

/// The outline of a file: its documents, and their resources and outputs.
pub fn symbols(file: &Parsed) -> Vec<Symbol> {
    let mut out = Vec::new();
    for doc in &file.file.docs {
        let Some(root) = &doc.root else { continue };
        let Some(kind) = root.text("kind") else {
            continue;
        };
        let whole = |node: &Node| Span {
            start: Pos::new(node.line, node.indent),
            end: Pos::new(
                node.end_line.saturating_sub(1).max(node.line),
                file.file
                    .line_end(node.end_line.saturating_sub(1).max(node.line)),
            ),
        };
        let name_node = root
            .get(&parse_path("metadata.name"))
            .and_then(Node::scalar);
        let mut children = Vec::new();
        let mut listed = |path: &str, key: &str, detail_key: &str, kind: SymbolKind| {
            for item in root.get(&parse_path(path)).map_or(&[][..], Node::items) {
                let Some(node) = &item.value else { continue };
                let Some(name) = node.child(key).and_then(Node::scalar) else {
                    continue;
                };
                children.push(Symbol {
                    name: name.text.clone(),
                    detail: node.text(detail_key).unwrap_or("").to_owned(),
                    kind,
                    span: whole(node),
                    selection: name.span,
                    children: Vec::new(),
                });
            }
        };
        listed("spec.resources", "id", "type", SymbolKind::Resource);
        listed("spec.outputs", "name", "value", SymbolKind::Output);
        let span = whole(root);
        out.push(Symbol {
            name: name_node.map_or("(unnamed)".to_owned(), |s| s.text.clone()),
            detail: kind.to_owned(),
            kind: SymbolKind::Document,
            span,
            selection: name_node.map_or(Span::on_line(root.line, 0, 0), |s| s.span),
            children,
        });
    }
    out
}
