//! Diagnostics: what is wrong with a set of documents, and exactly where.
//!
//! Every error reported here is the compiler's. The documents are parsed by
//! the compiler's parser, validated by its validator, and compiled by the
//! target plugin the document names; this module only gathers what they say
//! and finds the text each message is about. The one thing it adds is
//! reading a plugin's declared schema, so that every resource with a param
//! its target does not take is marked at once, where a compile stops at the
//! first.

use std::collections::BTreeMap;

use meridian_ast::ResourceGraph;
use meridian_ir::{Bundle, Document, Resolution, Value};
use meridian_orchestrator::version_fits;
use meridian_plugin_host::{to_sdk, EmitError, Registry};
use meridian_sdk::Data;

use crate::catalog::Catalog;
use crate::syntax::{self, parse_path, File, NodeKind, Pos, Seg, Span};

/// How serious a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// A replacement that would make a diagnostic go away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub title: String,
    pub span: Span,
    pub text: String,
}

/// One problem, located.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The source's name, as given to [`Workspace::new`].
    pub file: String,
    pub span: Span,
    pub severity: Severity,
    /// A stable name for the problem: a compiler rule such as
    /// `unsupported-param`, a lint rule, or `syntax`.
    pub code: String,
    pub message: String,
    /// Who says so: `meridian` for the compiler and validator, a target's
    /// name for a plugin's refusal or warning, `meridian-lint` for a lint.
    pub source: String,
    pub fix: Option<Fix>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}: {} [{}]",
            self.file,
            self.span.start.line + 1,
            self.span.start.col + 1,
            self.severity.as_str(),
            self.message,
            self.code
        )
    }
}

/// One file to analyse: a name for diagnostics and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub text: String,
}

/// One document that parsed, and where it is.
#[derive(Clone, Debug)]
pub struct ParsedDoc {
    /// The index of the document within its file's tree.
    pub index: usize,
    pub doc: Document,
}

/// One file, read both ways: as a positioned tree and by the compiler.
#[derive(Clone, Debug)]
pub struct Parsed {
    pub name: String,
    pub file: File,
    /// Whether the file holds Meridian documents at all. Other YAML is left
    /// alone.
    pub meridian: bool,
    pub docs: Vec<ParsedDoc>,
    /// What the parser refused.
    pub errors: Vec<Diagnostic>,
}

/// Whether text looks like it holds a Meridian document: some line gives an
/// `apiVersion` in the `meridian/` group.
pub fn is_meridian(text: &str) -> bool {
    let value_of = |line: &str, key: &str| -> Option<String> {
        let line = line.trim_start_matches(['{', ' ', '\t', '"', '\'']);
        line.strip_prefix(key).map(|rest| {
            rest.trim_start_matches(['"', '\'', ' ', ':', '\t'])
                .to_owned()
        })
    };
    let versioned = text
        .lines()
        .any(|line| value_of(line, "apiVersion").is_some_and(|v| v.starts_with("meridian/")));
    // A module's manifest (`kind: Module`) carries the same apiVersion and
    // is read by `meridian module`, not compiled as a document.
    let manifest = text.lines().any(|line| {
        !line.starts_with([' ', '\t'])
            && value_of(line, "kind")
                .is_some_and(|v| v.trim_end_matches(['"', '\'']).trim() == "Module")
    });
    versioned && !manifest
}

impl Parsed {
    pub fn new(source: &Source) -> Parsed {
        let file = syntax::read(&source.text);
        let meridian = is_meridian(&source.text);
        let mut parsed = Parsed {
            name: source.name.clone(),
            file,
            meridian,
            docs: Vec::new(),
            errors: Vec::new(),
        };
        if !meridian {
            return parsed;
        }
        if parsed.file.json {
            match meridian_ir::parse(&source.name, source.text.as_bytes()) {
                Ok(docs) => {
                    parsed.docs = docs
                        .into_iter()
                        .map(|doc| ParsedDoc { index: 0, doc })
                        .collect();
                }
                Err(errors) => {
                    let syntax =
                        meridian_ir::parse_stream(&source.name, source.text.as_bytes()).is_err();
                    for e in &errors {
                        let d = parsed.diagnose(e, 0, syntax);
                        parsed.errors.push(d);
                    }
                }
            }
            return parsed;
        }
        // Each document is parsed on its own, padded with blank lines so its
        // positions are the file's. One document's mistake then costs only
        // that document, where parsing the whole stream would lose them all.
        for index in 0..parsed.file.docs.len() {
            let doc = &parsed.file.docs[index];
            let Some(root) = &doc.root else {
                continue;
            };
            let mut text = "\n".repeat(root.line);
            for line in &parsed.file.lines[root.line..doc.end_line] {
                text.push_str(line);
                text.push('\n');
            }
            match meridian_ir::parse(&source.name, text.as_bytes()) {
                Ok(docs) => parsed
                    .docs
                    .extend(docs.into_iter().map(|doc| ParsedDoc { index, doc })),
                Err(errors) => {
                    // Text that is not YAML at all is a syntax error; text
                    // that is YAML but not a Meridian document is not.
                    let syntax = meridian_ir::parse_stream(&source.name, text.as_bytes()).is_err();
                    for e in &errors {
                        let d = parsed.diagnose(e, index, syntax);
                        parsed.errors.push(d);
                    }
                }
            }
        }
        parsed
    }

    /// Turns a compiler error into a diagnostic on the text it is about.
    fn diagnose(&self, e: &meridian_ir::Error, doc: usize, syntax: bool) -> Diagnostic {
        let code = if syntax { "syntax" } else { "invalid-document" };
        Diagnostic {
            file: self.name.clone(),
            span: self.span_of(doc, &e.path, e.pos.line, e.pos.column),
            severity: Severity::Error,
            code: code.to_owned(),
            message: if e.path.is_empty() {
                e.msg.clone()
            } else {
                format!("{}: {}", e.path, e.msg)
            },
            source: "meridian".to_owned(),
            fix: None,
        }
    }

    /// Whether document `doc` holds a `${{ ... }}` interpolation anywhere.
    pub fn interpolates(&self, doc: usize) -> bool {
        self.file.docs.get(doc).is_some_and(|d| {
            self.file.lines[d.first_line..d.end_line.min(self.file.lines.len())]
                .iter()
                .any(|line| meridian_expr::has_interpolation(line))
        })
    }

    /// The text a compiler position and path are about. `line` and `column`
    /// are 1-based, zero when unknown.
    pub fn span_of(&self, doc: usize, path: &str, line: usize, column: usize) -> Span {
        let segs = parse_path(path);
        let located = if path.is_empty() {
            None
        } else {
            self.file.locate(doc, &segs)
        };
        let pos = (line > 0).then(|| Pos::new(line - 1, column.saturating_sub(1)));
        let ends_in_key = matches!(segs.last(), Some(Seg::Key(_)));
        if let (Some(l), Some(p)) = (&located, pos) {
            // The path names a field on the very line the position is on:
            // the field is the more exact of the two.
            if l.complete && ends_in_key && l.line == p.line {
                return l.span(&self.file);
            }
        }
        if let Some(key) = pos.and_then(|p| self.key_at(p)) {
            return key;
        }
        if let Some(l) = &located {
            if l.complete {
                return l.span(&self.file);
            }
        }
        if let Some(p) = pos {
            let end = self.file.line_end(p.line).max(p.col + 1);
            return Span::on_line(p.line, p.col, end);
        }
        match located {
            Some(l) => l.span(&self.file),
            None => self.first_line(doc),
        }
    }

    /// The first line of a document, for a problem with no better place.
    pub fn first_line(&self, doc: usize) -> Span {
        let line = self
            .file
            .docs
            .get(doc)
            .and_then(|d| d.root.as_ref())
            .map_or(0, |r| r.line);
        Span::on_line(line, 0, self.file.line_end(line).max(1))
    }

    /// The key that starts exactly at `pos`, if one does.
    fn key_at(&self, pos: Pos) -> Option<Span> {
        fn find(node: &syntax::Node, pos: Pos) -> Option<Span> {
            match &node.kind {
                NodeKind::Map(entries) => entries.iter().find_map(|e| {
                    if e.key_span.start == pos {
                        return Some(e.key_span);
                    }
                    e.value.as_ref().and_then(|v| find(v, pos))
                }),
                NodeKind::Seq(items) => items
                    .iter()
                    .find_map(|i| i.value.as_ref().and_then(|v| find(v, pos))),
                NodeKind::Scalar(_) => None,
            }
        }
        self.file
            .docs
            .iter()
            .filter_map(|d| d.root.as_ref())
            .find_map(|root| find(root, pos))
    }

    /// Where something inside a resource is: `sub` is a path within the
    /// resource, such as `params.name`. Falls back to the resource's id.
    pub fn resource_span(&self, doc: usize, id: &str, sub: &str) -> Span {
        let index = self.resource_index(doc, id);
        let Some(index) = index else {
            return self.first_line(doc);
        };
        let base = format!("spec.resources[{index}]");
        if !sub.is_empty() {
            if let Some(found) = self
                .file
                .locate(doc, &parse_path(&format!("{base}.{sub}")))
                .filter(|l| l.complete)
            {
                // A whole mapping is marked at its key, a scalar at its text.
                return found.span(&self.file);
            }
        }
        match self.file.locate(doc, &parse_path(&format!("{base}.id"))) {
            Some(found) => found.span(&self.file),
            None => self.first_line(doc),
        }
    }

    /// The position of the resource called `id` in `spec.resources`.
    pub fn resource_index(&self, doc: usize, id: &str) -> Option<usize> {
        let root = self.file.docs.get(doc)?.root.as_ref()?;
        let resources = root.get(&parse_path("spec.resources"))?;
        resources.items().iter().position(|item| {
            item.value
                .as_ref()
                .and_then(|v| v.text("id"))
                .is_some_and(|text| text == id)
        })
    }
}

/// Every file under analysis.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub files: Vec<Parsed>,
}

impl Workspace {
    pub fn new(sources: &[Source]) -> Workspace {
        Workspace {
            files: sources.iter().map(Parsed::new).collect(),
        }
    }

    pub fn file(&self, name: &str) -> Option<&Parsed> {
        self.files.iter().find(|f| f.name == name)
    }

    /// The documents one file is checked with: its own, and the
    /// Infrastructure documents of other files that it may refer to.
    ///
    /// Files in a folder are not all one workflow. Two examples may both
    /// name a document `web`, and that is only a mistake if they are run
    /// together, which nothing here can know. So a file's own documents are
    /// checked against each other, and other files only lend the documents
    /// a `targetHost.ref` could name: one per name, the nearest file first.
    pub fn bundle_for(&self, name: &str) -> Bundle {
        let mut documents: Vec<Document> = Vec::new();
        let Some(own) = self.file(name) else {
            return Bundle::new(documents);
        };
        documents.extend(own.docs.iter().map(|d| d.doc.clone()));
        let mut taken: Vec<String> = documents
            .iter()
            .filter(|d| d.kind() == meridian_ir::Kind::Infrastructure)
            .map(|d| d.name().to_owned())
            .collect();
        for (_, doc) in self.lenders(name) {
            if !taken.iter().any(|t| t == doc.doc.name()) {
                taken.push(doc.doc.name().to_owned());
                documents.push(doc.doc.clone());
            }
        }
        Bundle::new(documents)
    }

    /// The Infrastructure documents of every file but `name`, those in the
    /// same directory first.
    fn lenders(&self, name: &str) -> Vec<(&Parsed, &ParsedDoc)> {
        let dir = |n: &str| {
            n.rfind(['/', '\\'])
                .map_or(String::new(), |i| n[..i].to_owned())
        };
        let here = dir(name);
        let mut others: Vec<&Parsed> = self.files.iter().filter(|f| f.name != name).collect();
        others.sort_by_key(|f| (dir(&f.name) != here, f.name.clone()));
        others
            .into_iter()
            .flat_map(|f| f.docs.iter().map(move |d| (f, d)))
            .filter(|(_, d)| d.doc.kind() == meridian_ir::Kind::Infrastructure)
            .collect()
    }

    /// The Infrastructure document a reference in `from` names: one in the
    /// file itself, or else the nearest one in another file.
    pub fn infrastructure(&self, from: &str, name: &str) -> Option<(&Parsed, &ParsedDoc)> {
        let own = self.file(from).into_iter().flat_map(|f| {
            f.docs
                .iter()
                .filter(|d| d.doc.kind() == meridian_ir::Kind::Infrastructure)
                .map(move |d| (f, d))
        });
        own.chain(self.lenders(from))
            .find(|(_, d)| d.doc.name() == name)
    }
}

/// What the checks may use.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    /// The loaded plugins, to compile each document with. Without them only
    /// the language itself is checked.
    pub registry: Option<&'a Registry>,
    pub catalog: &'a Catalog,
    /// The values `when` conditions read. Without them every resource is
    /// checked as if its condition held, which is what an editor wants: a
    /// resource that only exists in production is still worth checking.
    pub data: Option<&'a Data>,
}

/// Checks a workspace: parse errors, validation errors, and what each
/// document's target says when asked to compile it. Sorted by file and
/// position.
pub fn check(workspace: &Workspace, env: &Env<'_>) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for file in &workspace.files {
        out.extend(file.errors.iter().cloned());
    }

    for file in &workspace.files {
        // Validation, with the documents of other files that this one may
        // refer to, so a reference between files is checked.
        let bundle = workspace.bundle_for(&file.name);
        let mut invalid: Vec<usize> = Vec::new();
        if let Err(errors) = bundle.validate() {
            for e in &errors {
                // What is wrong with a lent document is its own file's to
                // report.
                if e.pos.file != file.name {
                    continue;
                }
                let doc = if e.pos.line > 0 {
                    file.file.doc_at(e.pos.line - 1).unwrap_or(0)
                } else {
                    0
                };
                invalid.push(doc);
                out.push(Diagnostic {
                    file: file.name.clone(),
                    span: file.span_of(doc, &e.path, e.pos.line, e.pos.column),
                    severity: Severity::Error,
                    code: "invalid-document".to_owned(),
                    message: if e.path.is_empty() {
                        e.msg.clone()
                    } else {
                        format!("{}: {}", e.path, e.msg)
                    },
                    source: "meridian".to_owned(),
                    fix: None,
                });
            }
        }

        for parsed in &file.docs {
            // What the target says about each resource is worth hearing
            // even when the document is invalid for another reason; only
            // compiling it has to wait until it is valid.
            let from_schema = schema_problems(file, parsed, env.catalog);
            let from_target = if invalid.contains(&parsed.index) {
                Vec::new()
            } else {
                compile(file, parsed, &bundle, env)
            };
            // A refusal the schema already explained, resource by resource,
            // is not repeated.
            for d in from_target {
                let repeated = from_schema.iter().any(|s| {
                    s.code == d.code
                        && (s.span.start.line == d.span.start.line
                            || same_resource(file, parsed.index, s, &d))
                });
                if !repeated {
                    out.push(d);
                }
            }
            out.extend(from_schema);
        }
    }

    out.sort_by(|a, b| {
        (&a.file, a.span.start, a.severity, &a.code).cmp(&(
            &b.file,
            b.span.start,
            b.severity,
            &b.code,
        ))
    });
    out.dedup();
    out
}

/// Whether two diagnostics fall inside the same resource.
fn same_resource(file: &Parsed, doc: usize, a: &Diagnostic, b: &Diagnostic) -> bool {
    let Some(resources) = file
        .file
        .docs
        .get(doc)
        .and_then(|d| d.root.as_ref())
        .and_then(|r| r.get(&parse_path("spec.resources")))
    else {
        return false;
    };
    let item_of = |line: usize| {
        resources
            .items()
            .iter()
            .position(|i| i.line <= line && line < i.end_line)
    };
    let (a, b) = (item_of(a.span.start.line), item_of(b.span.start.line));
    a.is_some() && a == b
}

fn problem(file: &Parsed, span: Span, code: &str, source: &str, message: String) -> Diagnostic {
    Diagnostic {
        file: file.name.clone(),
        span,
        severity: Severity::Error,
        code: code.to_owned(),
        message,
        source: source.to_owned(),
        fix: None,
    }
}

fn list(names: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let names: Vec<String> = names.into_iter().map(|n| n.as_ref().to_owned()).collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

/// Checks each resource against what its target's plugin declares: the
/// type, its params, its state, and the actions it can be notified with.
fn schema_problems(file: &Parsed, parsed: &ParsedDoc, catalog: &Catalog) -> Vec<Diagnostic> {
    let doc = &parsed.doc;
    let mut out = Vec::new();
    // A target that is worked out from data is not known here.
    let Some(target) = catalog.target(doc.target()) else {
        return out;
    };
    // A document of the wrong kind for its target is one problem, not one
    // per resource.
    if !target.knows_types() || target.refuses_kind(doc.kind()).is_some() {
        return out;
    }
    let source = target.name.as_str();
    let types: BTreeMap<&str, &str> = doc
        .resources()
        .iter()
        .map(|r| (r.id.as_str(), r.r#type.as_str()))
        .collect();
    for r in doc.resources() {
        let at = |sub: &str| file.resource_span(parsed.index, &r.id, sub);
        let Some(schema) = target.resource(&r.r#type) else {
            out.push(problem(
                file,
                at("type"),
                "unsupported-resource-type",
                source,
                format!(
                    "target {:?} has no resource type {:?} (it has: {})",
                    target.name,
                    r.r#type,
                    list(target.types.iter().map(|t| &t.r#type))
                ),
            ));
            continue;
        };
        if !schema.open {
            for name in r.params.keys() {
                if schema.find_param(name).is_none() {
                    out.push(problem(
                        file,
                        key_span(file, parsed.index, &r.id, name),
                        "unsupported-param",
                        source,
                        format!(
                            "resource type {:?} does not take param {:?} on target {:?} (it takes: {})",
                            r.r#type,
                            name,
                            target.name,
                            list(schema.params.iter().map(|p| &p.name))
                        ),
                    ));
                }
            }
            // A state worked out from data is judged once it is known.
            if !r.state.is_empty()
                && !schema.states.contains(&r.state)
                && !meridian_expr::has_interpolation(&r.state)
            {
                let message = if schema.states.is_empty() {
                    format!(
                        "resource type {:?} takes no state on target {:?}",
                        r.r#type, target.name
                    )
                } else {
                    format!(
                        "resource type {:?} does not take state {:?} on target {:?} (it takes: {})",
                        r.r#type,
                        r.state,
                        target.name,
                        list(&schema.states)
                    )
                };
                out.push(problem(
                    file,
                    at("state"),
                    "unsupported-state",
                    source,
                    message,
                ));
            }
        }
        for param in schema.params.iter().filter(|p| p.required) {
            if !r.params.contains_key(&param.name) {
                out.push(problem(
                    file,
                    at("params"),
                    "missing-param",
                    source,
                    format!(
                        "resource type {:?} requires param {:?} on target {:?}",
                        r.r#type, param.name, target.name
                    ),
                ));
            }
        }
        for (name, value) in &r.params {
            let Some(param) = schema.find_param(name) else {
                continue;
            };
            let Value::String(text) = value else { continue };
            if !param.values.is_empty() && !param.values.contains(text) && !text.contains("${") {
                out.push(problem(
                    file,
                    at(&format!("params.{name}")),
                    "unsupported-value",
                    source,
                    format!(
                        "param {:?} does not take {:?} (it takes: {})",
                        name,
                        text,
                        list(&param.values)
                    ),
                ));
            }
        }
        for (j, n) in r.notifies.iter().enumerate() {
            let Some(notified) = types.get(n.resource.as_str()) else {
                continue;
            };
            let Some(schema) = target.resource(notified).filter(|s| !s.open) else {
                continue;
            };
            if n.action.is_empty() || schema.actions.contains(&n.action) {
                continue;
            }
            let message = if schema.actions.is_empty() {
                format!(
                    "resource type {:?} cannot be notified on target {:?}",
                    notified, target.name
                )
            } else {
                format!(
                    "resource type {:?} cannot perform {:?} when notified on target {:?} (it can: {})",
                    notified,
                    n.action,
                    target.name,
                    list(&schema.actions)
                )
            };
            out.push(problem(
                file,
                at(&format!("notifies[{j}].action")),
                "unsupported-action",
                source,
                message,
            ));
        }
    }
    out
}

/// The span of a param's key, so an unwanted param is marked at its name.
fn key_span(file: &Parsed, doc: usize, id: &str, param: &str) -> Span {
    let found = file.resource_index(doc, id).and_then(|index| {
        file.file
            .locate(
                doc,
                &[
                    Seg::Key("spec".into()),
                    Seg::Key("resources".into()),
                    Seg::Index(index),
                    Seg::Key("params".into()),
                    Seg::Key(param.to_owned()),
                ],
            )
            .filter(|l| l.complete)
            .and_then(|l| l.key)
    });
    found.unwrap_or_else(|| file.resource_span(doc, id, "params"))
}

/// Asks the document's target to compile it and reports what it says. This
/// follows the orchestrator's own compile step: same target lookup, same
/// kind and version rules, same tree, same `emit`.
/// The document with every `when` taken off, so that everything in it is
/// checked: what an editor shows when it has no data to decide them with.
fn without_conditions(doc: &Document) -> Document {
    let mut doc = doc.clone();
    let (when, resources, outputs) = match &mut doc {
        Document::Infrastructure(d) => (
            &mut d.spec.when,
            &mut d.spec.resources,
            Some(&mut d.spec.outputs),
        ),
        Document::ResourceSet(d) => (&mut d.spec.when, &mut d.spec.resources, None),
    };
    when.clear();
    for r in resources {
        r.when.clear();
        for d in &mut r.depends_on {
            d.when.clear();
        }
        for n in &mut r.notifies {
            n.when.clear();
        }
    }
    for o in outputs.into_iter().flatten() {
        o.when.clear();
    }
    doc
}

fn compile(file: &Parsed, parsed: &ParsedDoc, bundle: &Bundle, env: &Env<'_>) -> Vec<Diagnostic> {
    let index = parsed.index;
    let mut out = Vec::new();
    let at = |path: &str| file.span_of(index, path, 0, 0);
    let fail = |span: Span, code: &str, message: String| {
        vec![problem(file, span, code, "meridian", message)]
    };

    // Work out the document as a run would: variables, named conditions,
    // every `when` and every interpolation. With data that is exactly what
    // `compile` does. Without it, a document that needs none is still worked
    // out in full; one that does need data is checked as if every condition
    // held, and is not handed to the plugin while it still holds
    // interpolations nobody can place, since the plugin would be judging
    // text the tool will never see.
    let empty = Data::new();
    let data = env.data.unwrap_or(&empty);
    let mut compilable = true;
    let resolved = match meridian_ir::resolve(&parsed.doc, data) {
        Ok(Resolution::Kept { document, .. }) => *document,
        Ok(Resolution::Skipped { .. }) => return out,
        Err(e) if env.data.is_some() => {
            return vec![problem(
                file,
                file.span_of(index, &e.path, e.pos.line, e.pos.column),
                "invalid-condition",
                "meridian",
                e.msg,
            )]
        }
        Err(_) => {
            compilable = !file.interpolates(index);
            without_conditions(&parsed.doc)
        }
    };
    let doc = &resolved;
    if meridian_expr::has_interpolation(doc.target())
        || meridian_expr::has_interpolation(doc.target_version())
    {
        return out;
    }

    let Some(target) = env.catalog.target(doc.target()) else {
        if env.catalog.is_empty() {
            // No plugins are loaded at all, so nothing can be said about
            // any target; staying silent beats marking every document.
            return out;
        }
        return fail(
            at("spec.target"),
            "unknown-target",
            format!(
                "no plugin provides target {:?} (loaded targets: {})",
                doc.target(),
                list(env.catalog.names())
            ),
        );
    };
    if let Some(why) = target.refuses_kind(doc.kind()) {
        return fail(at("kind"), "kind-does-not-fit-target", why);
    }
    let asked = doc.target_version();
    if !asked.is_empty() {
        if target.versions.is_empty() {
            return fail(
                at("spec.targetVersion"),
                "unsupported-target-version",
                format!(
                    "target {:?} does not say which versions of its tool it compiles for, \
                     so a targetVersion cannot be honoured; remove it",
                    target.name
                ),
            );
        }
        if !version_fits(asked, &target.versions) {
            return fail(
                at("spec.targetVersion"),
                "unsupported-target-version",
                format!(
                    "target {:?} compiles for: {}",
                    target.name,
                    list(&target.versions)
                ),
            );
        }
    }

    let Some(plugin) = env
        .registry
        .and_then(|r| r.target(doc.target()))
        .filter(|_| compilable)
    else {
        return out;
    };

    let graph = match doc {
        Document::Infrastructure(infra) => ResourceGraph::build_infrastructure(infra),
        Document::ResourceSet(rs) => {
            if rs.spec.target_host.is_none() {
                return fail(
                    at("spec"),
                    "missing-target-host",
                    format!("document {:?} declares no targetHost", rs.metadata.name),
                );
            }
            // An editor has no applied infrastructure to read a host from,
            // so the output's declaration stands in for its value.
            let host = bundle
                .resolve_target_host(rs)
                .map(|(_, output)| output.value.clone())
                .ok()
                .filter(|host| !host.is_empty())
                .unwrap_or_else(|| "host".to_owned());
            ResourceGraph::build(rs, &host)
        }
    };
    let graph = match graph {
        Ok(graph) => graph,
        Err(e) => {
            let span = if e.pos.line > 0 {
                file.span_of(index, "", e.pos.line, e.pos.column)
            } else {
                at("spec.resources")
            };
            return fail(span, "invalid-document", e.msg);
        }
    };

    match plugin.emit(&to_sdk(&graph), data) {
        Ok(emitted) => {
            for warning in emitted.warnings {
                let span = if warning.resource.is_empty() {
                    at("spec.target")
                } else {
                    file.resource_span(index, &warning.resource, "")
                };
                out.push(Diagnostic {
                    severity: Severity::Warning,
                    ..problem(file, span, "target-warning", &target.name, warning.msg)
                });
            }
        }
        Err(EmitError::Refused(e)) => {
            let sub = match e.rule.as_str() {
                "unsupported-resource-type" => "type",
                "unsupported-state" => "state",
                "missing-param" | "empty-param" => "params",
                "unsupported-runtime-condition" => "runtimeWhen",
                _ => "",
            };
            let mut span = if e.resource.is_empty() {
                at("spec.target")
            } else {
                file.resource_span(index, &e.resource, sub)
            };
            // A refusal that quotes one of the resource's params is about
            // that param.
            if let Some(r) = doc.resources().iter().find(|r| r.id == e.resource) {
                if let Some(name) = r
                    .params
                    .keys()
                    .find(|name| e.msg.contains(&format!("param {name:?}")))
                {
                    span = key_span(file, index, &r.id, name);
                }
            }
            let code = if e.rule.is_empty() {
                "refused"
            } else {
                e.rule.as_str()
            };
            out.push(problem(file, span, code, &target.name, e.msg));
        }
        Err(EmitError::Plugin(e)) => {
            out.push(problem(
                file,
                at("spec.target"),
                "plugin-failed",
                "meridian",
                e.to_string(),
            ));
        }
    }
    out
}
