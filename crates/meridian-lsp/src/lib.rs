//! The Meridian language server.
//!
//! It is a thin layer. Every answer comes from `meridian-tooling`, which in
//! turn asks the compiler and the loaded target plugins; this crate turns
//! those answers into the Language Server Protocol and keeps track of which
//! files are open. Any editor that speaks the protocol can use it: start
//! `meridian lsp` and talk to it over standard input and output.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types as lsp;
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::Url;
use meridian_plugin_host::{Registry, StartOptions};
use meridian_tooling::analysis::{self, CompletionKind, SymbolKind};
use meridian_tooling::catalog::Catalog;
use meridian_tooling::check::{self, Diagnostic, Env, Severity, Source, Workspace};
use meridian_tooling::syntax::{Pos, Span};
use meridian_tooling::{fmt, lint};

/// Why the server stopped abnormally.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The command an editor sends to load the plugins again, after one was
/// installed or rebuilt.
pub const RELOAD_PLUGINS: &str = "meridian.reloadPlugins";

/// Files larger than this are not read from disk into the workspace; no
/// hand-written document is this big.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// How many files a workspace scan reads before it stops looking.
const MAX_FILES: usize = 2000;

/// Options a client may pass as `initializationOptions`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Options {
    /// Directories to load target plugins from, ahead of the default.
    plugin_dirs: Vec<PathBuf>,
    /// Whether lints are reported alongside the compiler's diagnostics.
    lint: Option<bool>,
    /// Whether `fmt` writes fields in the language's order.
    sort_fields: Option<bool>,
}

/// Loads every plugin found in `dirs`. A directory that cannot be read is
/// reported and skipped: an editor is still useful with fewer targets.
pub fn load_plugins(dirs: &[PathBuf]) -> (Registry, Vec<String>) {
    let mut registry = Registry::new();
    let mut problems = Vec::new();
    for dir in dirs {
        if let Err(e) = registry.discover(&dir.to_string_lossy(), &StartOptions::default()) {
            problems.push(e.to_string());
        }
    }
    (registry, problems)
}

/// Serves the protocol over standard input and output until the client
/// asks to exit. Plugins are loaded from `plugin_dirs`.
pub fn run_stdio(plugin_dirs: Vec<PathBuf>) -> Result<(), Error> {
    let (connection, io_threads) = Connection::stdio();
    serve(&connection, Plugins::Discover(plugin_dirs))?;
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Where the server's plugins come from.
pub enum Plugins {
    /// Find plugin binaries in these directories, and in any the client
    /// names in its initialization options.
    Discover(Vec<PathBuf>),
    /// Use exactly this registry. This is how tests serve compiled-in
    /// plugins.
    Loaded(Registry),
}

/// Serves one client over `connection` until it asks to exit.
pub fn serve(connection: &Connection, plugins: Plugins) -> Result<(), Error> {
    let (id, params) = connection.initialize_start()?;
    let params: lsp::InitializeParams = serde_json::from_value(params)?;
    let options: Options = params
        .initialization_options
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let result = lsp::InitializeResult {
        capabilities: capabilities(),
        server_info: Some(lsp::ServerInfo {
            name: "meridian".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    let mut roots: Vec<PathBuf> = params
        .workspace_folders
        .unwrap_or_default()
        .iter()
        .filter_map(|f| f.uri.to_file_path().ok())
        .collect();
    #[allow(deprecated)]
    if roots.is_empty() {
        roots.extend(params.root_uri.and_then(|u| u.to_file_path().ok()));
    }

    let (dirs, registry, problems) = match plugins {
        Plugins::Loaded(registry) => (Vec::new(), registry, Vec::new()),
        Plugins::Discover(mut dirs) => {
            // The client's directories come first, so a project's own
            // plugins win over the ones installed beside the binary.
            let mut all = options.plugin_dirs.clone();
            all.append(&mut dirs);
            let (registry, problems) = load_plugins(&all);
            (all, registry, problems)
        }
    };

    let mut server = Server {
        connection,
        catalog: Catalog::from_registry(&registry),
        registry,
        plugin_dirs: dirs,
        open: BTreeMap::new(),
        disk: BTreeMap::new(),
        roots,
        published: BTreeSet::new(),
        lint: options.lint.unwrap_or(true),
        sort_fields: options.sort_fields.unwrap_or(true),
        workspace: Workspace::default(),
        stale: true,
    };
    for problem in problems {
        server.show(lsp::MessageType::WARNING, &problem);
    }
    if server.catalog.is_empty() {
        server.show(
            lsp::MessageType::INFO,
            "Meridian found no target plugins, so documents are checked against the \
             language only. Set the plugin directory in the extension's settings, or put \
             the meridian-target-* binaries beside the meridian binary.",
        );
    }
    server.scan();
    server.run()?;
    let Server { registry, .. } = server;
    // A plugin that will not exit is not worth failing the editor over.
    let _ = registry.close();
    Ok(())
}

fn capabilities() -> lsp::ServerCapabilities {
    lsp::ServerCapabilities {
        text_document_sync: Some(lsp::TextDocumentSyncCapability::Options(
            lsp::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(lsp::TextDocumentSyncKind::FULL),
                save: Some(lsp::TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        completion_provider: Some(lsp::CompletionOptions {
            // A space after a colon or a dash, and the `{` and `.` of a
            // `${id.param}` reference.
            trigger_characters: Some(vec![
                " ".to_owned(),
                ":".to_owned(),
                "{".to_owned(),
                ".".to_owned(),
            ]),
            ..Default::default()
        }),
        hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
        definition_provider: Some(lsp::OneOf::Left(true)),
        references_provider: Some(lsp::OneOf::Left(true)),
        rename_provider: Some(lsp::OneOf::Right(lsp::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_symbol_provider: Some(lsp::OneOf::Left(true)),
        document_formatting_provider: Some(lsp::OneOf::Left(true)),
        code_action_provider: Some(lsp::CodeActionProviderCapability::Options(
            lsp::CodeActionOptions {
                code_action_kinds: Some(vec![lsp::CodeActionKind::QUICKFIX]),
                ..Default::default()
            },
        )),
        execute_command_provider: Some(lsp::ExecuteCommandOptions {
            commands: vec![RELOAD_PLUGINS.to_owned()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct Server<'a> {
    connection: &'a Connection,
    registry: Registry,
    catalog: Catalog,
    plugin_dirs: Vec<PathBuf>,
    /// The text of every open file, by the name diagnostics use for it.
    open: BTreeMap<String, String>,
    /// Meridian files found in the workspace folders that are not open.
    disk: BTreeMap<String, String>,
    roots: Vec<PathBuf>,
    /// Files the client is currently showing diagnostics for.
    published: BTreeSet<String>,
    lint: bool,
    sort_fields: bool,
    workspace: Workspace,
    /// Whether `workspace` is behind the text.
    stale: bool,
}

/// The name a file goes by inside the server: its path when it has one, so
/// messages that mention a file read naturally, and its URI otherwise.
fn name_of(uri: &Url) -> String {
    match uri.to_file_path() {
        Ok(path) if uri.scheme() == "file" => path.to_string_lossy().into_owned(),
        _ => uri.to_string(),
    }
}

fn uri_of(name: &str) -> Option<Url> {
    if name.contains("://") || name.starts_with("untitled:") {
        Url::parse(name).ok()
    } else {
        Url::from_file_path(name).ok()
    }
}

/// Converts a column in bytes to one in UTF-16 code units, which is what
/// the protocol counts in.
fn to_utf16(line: &str, col: usize) -> u32 {
    let mut end = col.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].encode_utf16().count() as u32
}

fn from_utf16(line: &str, col: u32) -> usize {
    let mut units = 0;
    for (i, c) in line.char_indices() {
        if units >= col as usize {
            return i;
        }
        units += c.len_utf16();
    }
    line.len()
}

impl Server<'_> {
    fn show(&self, kind: lsp::MessageType, message: &str) {
        self.notify::<lsp::notification::ShowMessage>(lsp::ShowMessageParams {
            typ: kind,
            message: message.to_owned(),
        });
    }

    fn notify<N: lsp::notification::Notification>(&self, params: N::Params) {
        let _ = self
            .connection
            .sender
            .send(Message::Notification(Notification::new(
                N::METHOD.to_owned(),
                params,
            )));
    }

    fn respond<T: serde::Serialize>(&self, id: RequestId, result: T) {
        let _ = self
            .connection
            .sender
            .send(Message::Response(Response::new_ok(id, result)));
    }

    /// Finds the Meridian documents in the workspace folders, so a
    /// reference to a document in a file that is not open still resolves.
    fn scan(&mut self) {
        fn visit(dir: &Path, found: &mut BTreeMap<String, String>, budget: &mut usize) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                if *budget == 0 {
                    return;
                }
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    // Build output, dependencies and state are not sources.
                    if name.starts_with('.') || matches!(&*name, "node_modules" | "target") {
                        continue;
                    }
                    visit(&path, found, budget);
                } else if kind.is_file()
                    && matches!(
                        path.extension().and_then(|e| e.to_str()),
                        Some("yaml" | "yml" | "json")
                    )
                {
                    *budget -= 1;
                    let small = entry.metadata().is_ok_and(|m| m.len() <= MAX_FILE_BYTES);
                    if !small {
                        continue;
                    }
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if check::is_meridian(&text) {
                            found.insert(path.to_string_lossy().into_owned(), text);
                        }
                    }
                }
            }
        }
        let mut found = BTreeMap::new();
        let mut budget = MAX_FILES;
        for root in &self.roots {
            visit(root, &mut found, &mut budget);
        }
        self.disk = found;
        self.stale = true;
    }

    /// The workspace as it stands: open files as typed, the rest from disk.
    fn workspace(&mut self) -> &Workspace {
        if self.stale {
            let mut sources: Vec<Source> = self
                .open
                .iter()
                .map(|(name, text)| Source {
                    name: name.clone(),
                    text: text.clone(),
                })
                .collect();
            for (name, text) in &self.disk {
                if !self.open.contains_key(name) {
                    sources.push(Source {
                        name: name.clone(),
                        text: text.clone(),
                    });
                }
            }
            self.workspace = Workspace::new(&sources);
            self.stale = false;
        }
        &self.workspace
    }

    fn line<'t>(&'t self, name: &str, line: usize) -> &'t str {
        self.workspace
            .file(name)
            .and_then(|f| f.file.lines.get(line))
            .map_or("", String::as_str)
    }

    fn range(&self, name: &str, span: Span) -> lsp::Range {
        let point = |pos: Pos| lsp::Position {
            line: pos.line as u32,
            character: to_utf16(self.line(name, pos.line), pos.col),
        };
        lsp::Range {
            start: point(span.start),
            end: point(span.end),
        }
    }

    fn pos(&self, name: &str, position: lsp::Position) -> Pos {
        let line = position.line as usize;
        Pos::new(line, from_utf16(self.line(name, line), position.character))
    }

    fn diagnostics(&mut self) -> Vec<Diagnostic> {
        self.workspace();
        let env = Env {
            registry: Some(&self.registry),
            catalog: &self.catalog,
            data: None,
        };
        let mut found = check::check(&self.workspace, &env);
        if self.lint {
            found.extend(lint::lint(&self.workspace, &self.catalog));
        }
        found
    }

    /// Sends every open file its diagnostics, and clears a file that was
    /// closed while it had some.
    fn publish(&mut self) {
        let found = self.diagnostics();
        let mut by_file: BTreeMap<String, Vec<lsp::Diagnostic>> = BTreeMap::new();
        for d in &found {
            // Only what the client has open: marking files it never opened
            // would fill its problems list with other people's work.
            if !self.open.contains_key(&d.file) {
                continue;
            }
            by_file
                .entry(d.file.clone())
                .or_default()
                .push(self.to_lsp(d));
        }
        // Every open file hears back after every change, with an empty
        // list when it is clean, so the client never shows a stale mark.
        let mut names: BTreeSet<String> = self.open.keys().cloned().collect();
        names.extend(self.published.iter().cloned());
        for name in names {
            let Some(uri) = uri_of(&name) else { continue };
            let diagnostics = by_file.remove(&name).unwrap_or_default();
            if diagnostics.is_empty() {
                self.published.remove(&name);
            } else {
                self.published.insert(name.clone());
            }
            self.notify::<lsp::notification::PublishDiagnostics>(lsp::PublishDiagnosticsParams {
                uri,
                diagnostics,
                version: None,
            });
        }
    }

    fn to_lsp(&self, d: &Diagnostic) -> lsp::Diagnostic {
        lsp::Diagnostic {
            range: self.range(&d.file, d.span),
            severity: Some(match d.severity {
                Severity::Error => lsp::DiagnosticSeverity::ERROR,
                Severity::Warning => lsp::DiagnosticSeverity::WARNING,
                Severity::Info => lsp::DiagnosticSeverity::INFORMATION,
            }),
            code: Some(lsp::NumberOrString::String(d.code.clone())),
            source: Some(d.source.clone()),
            message: d.message.clone(),
            ..Default::default()
        }
    }

    fn run(&mut self) -> Result<(), Error> {
        while let Ok(message) = self.connection.receiver.recv() {
            let mut changed = false;
            let mut next = Some(message);
            // Everything already waiting is handled before diagnostics are
            // worked out again, so fast typing costs one check, not one per
            // keystroke.
            while let Some(message) = next {
                match message {
                    Message::Request(request) => {
                        if self.connection.handle_shutdown(&request)? {
                            return Ok(());
                        }
                        changed |= self.request(request);
                    }
                    Message::Notification(notification) => {
                        changed |= self.notification(notification)
                    }
                    Message::Response(_) => {}
                }
                next = self.connection.receiver.try_recv().ok();
            }
            if changed {
                self.publish();
            }
        }
        Ok(())
    }

    /// Handles a notification. Returns whether diagnostics may have changed.
    fn notification(&mut self, n: Notification) -> bool {
        fn params<N: lsp::notification::Notification>(n: Notification) -> Option<N::Params> {
            serde_json::from_value(n.params).ok()
        }
        match n.method.as_str() {
            lsp::notification::DidOpenTextDocument::METHOD => {
                let Some(p) = params::<lsp::notification::DidOpenTextDocument>(n) else {
                    return false;
                };
                self.open
                    .insert(name_of(&p.text_document.uri), p.text_document.text);
            }
            lsp::notification::DidChangeTextDocument::METHOD => {
                let Some(p) = params::<lsp::notification::DidChangeTextDocument>(n) else {
                    return false;
                };
                // Full sync: the last change is the whole text.
                let Some(change) = p.content_changes.into_iter().last() else {
                    return false;
                };
                self.open.insert(name_of(&p.text_document.uri), change.text);
            }
            lsp::notification::DidCloseTextDocument::METHOD => {
                let Some(p) = params::<lsp::notification::DidCloseTextDocument>(n) else {
                    return false;
                };
                self.open.remove(&name_of(&p.text_document.uri));
                // What is on disk may differ from what was open.
                self.scan();
            }
            lsp::notification::DidSaveTextDocument::METHOD
            | lsp::notification::DidChangeWatchedFiles::METHOD => self.scan(),
            lsp::notification::DidChangeConfiguration::METHOD => {
                let Some(p) = params::<lsp::notification::DidChangeConfiguration>(n) else {
                    return false;
                };
                let settings = p.settings.get("meridian").cloned().unwrap_or(p.settings);
                if let Ok(options) = serde_json::from_value::<Options>(settings) {
                    self.lint = options.lint.unwrap_or(self.lint);
                    self.sort_fields = options.sort_fields.unwrap_or(self.sort_fields);
                }
            }
            _ => return false,
        }
        self.stale = true;
        true
    }

    /// Handles a request. Returns whether diagnostics may have changed.
    fn request(&mut self, request: Request) -> bool {
        let id = request.id.clone();
        macro_rules! params {
            ($kind:ty) => {
                match serde_json::from_value::<<$kind as lsp::request::Request>::Params>(
                    request.params,
                ) {
                    Ok(params) => params,
                    Err(e) => {
                        let _ = self
                            .connection
                            .sender
                            .send(Message::Response(Response::new_err(
                                id,
                                ErrorCode::InvalidParams as i32,
                                e.to_string(),
                            )));
                        return false;
                    }
                }
            };
        }
        self.workspace();
        match request.method.as_str() {
            lsp::request::Completion::METHOD => {
                let p = params!(lsp::request::Completion);
                let name = name_of(&p.text_document_position.text_document.uri);
                let pos = self.pos(&name, p.text_document_position.position);
                let items: Vec<lsp::CompletionItem> =
                    analysis::complete(&self.workspace, &self.catalog, &name, pos)
                        .into_iter()
                        .enumerate()
                        .map(|(i, c)| self.completion(&name, i, c))
                        .collect();
                self.respond(id, lsp::CompletionResponse::Array(items));
            }
            lsp::request::HoverRequest::METHOD => {
                let p = params!(lsp::request::HoverRequest);
                let name = name_of(&p.text_document_position_params.text_document.uri);
                let pos = self.pos(&name, p.text_document_position_params.position);
                let hover = analysis::hover(&self.workspace, &self.catalog, &name, pos).map(|h| {
                    lsp::Hover {
                        contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                            kind: lsp::MarkupKind::Markdown,
                            value: h.markdown,
                        }),
                        range: Some(self.range(&name, h.span)),
                    }
                });
                self.respond(id, hover);
            }
            lsp::request::GotoDefinition::METHOD => {
                let p = params!(lsp::request::GotoDefinition);
                let name = name_of(&p.text_document_position_params.text_document.uri);
                let pos = self.pos(&name, p.text_document_position_params.position);
                let location = analysis::definition(&self.workspace, &self.catalog, &name, pos)
                    .and_then(|l| self.location(&l));
                self.respond(id, location.map(lsp::GotoDefinitionResponse::Scalar));
            }
            lsp::request::References::METHOD => {
                let p = params!(lsp::request::References);
                let name = name_of(&p.text_document_position.text_document.uri);
                let pos = self.pos(&name, p.text_document_position.position);
                let locations: Vec<lsp::Location> =
                    analysis::references(&self.workspace, &self.catalog, &name, pos)
                        .iter()
                        .filter_map(|l| self.location(l))
                        .collect();
                self.respond(id, locations);
            }
            lsp::request::PrepareRenameRequest::METHOD => {
                let p = params!(lsp::request::PrepareRenameRequest);
                let name = name_of(&p.text_document.uri);
                let pos = self.pos(&name, p.position);
                let found = analysis::references(&self.workspace, &self.catalog, &name, pos);
                // The occurrence under the cursor is what gets renamed.
                let range = found
                    .iter()
                    .find(|l| l.span.start.line == pos.line && l.span.contains(pos))
                    .map(|l| lsp::PrepareRenameResponse::Range(self.range(&name, l.span)));
                self.respond(id, range);
            }
            lsp::request::Rename::METHOD => {
                let p = params!(lsp::request::Rename);
                let uri = p.text_document_position.text_document.uri;
                let name = name_of(&uri);
                let pos = self.pos(&name, p.text_document_position.position);
                let edits: Vec<lsp::TextEdit> =
                    analysis::references(&self.workspace, &self.catalog, &name, pos)
                        .iter()
                        .map(|l| lsp::TextEdit {
                            range: self.range(&name, l.span),
                            new_text: p.new_name.clone(),
                        })
                        .collect();
                let edit = (!edits.is_empty()).then(|| lsp::WorkspaceEdit {
                    changes: Some(HashMap::from([(uri, edits)])),
                    ..Default::default()
                });
                self.respond(id, edit);
            }
            lsp::request::DocumentSymbolRequest::METHOD => {
                let p = params!(lsp::request::DocumentSymbolRequest);
                let name = name_of(&p.text_document.uri);
                let symbols: Vec<lsp::DocumentSymbol> = self
                    .workspace
                    .file(&name)
                    .filter(|f| f.meridian)
                    .map(analysis::symbols)
                    .unwrap_or_default()
                    .iter()
                    .map(|s| self.symbol(&name, s))
                    .collect();
                self.respond(id, lsp::DocumentSymbolResponse::Nested(symbols));
            }
            lsp::request::Formatting::METHOD => {
                let p = params!(lsp::request::Formatting);
                let name = name_of(&p.text_document.uri);
                let edits = self.format(&name);
                self.respond(id, edits);
            }
            lsp::request::CodeActionRequest::METHOD => {
                let p = params!(lsp::request::CodeActionRequest);
                let uri = p.text_document.uri;
                let name = name_of(&uri);
                let wanted = p.range;
                let actions: Vec<lsp::CodeActionOrCommand> = self
                    .diagnostics()
                    .iter()
                    .filter(|d| d.file == name)
                    .filter_map(|d| {
                        let fix = d.fix.as_ref()?;
                        let range = self.range(&name, d.span);
                        let overlaps = range.start <= wanted.end && wanted.start <= range.end;
                        overlaps.then(|| {
                            lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
                                title: fix.title.clone(),
                                kind: Some(lsp::CodeActionKind::QUICKFIX),
                                diagnostics: Some(vec![self.to_lsp(d)]),
                                edit: Some(lsp::WorkspaceEdit {
                                    changes: Some(HashMap::from([(
                                        uri.clone(),
                                        vec![lsp::TextEdit {
                                            range: self.range(&name, fix.span),
                                            new_text: fix.text.clone(),
                                        }],
                                    )])),
                                    ..Default::default()
                                }),
                                is_preferred: Some(true),
                                ..Default::default()
                            })
                        })
                    })
                    .collect();
                self.respond(id, actions);
            }
            lsp::request::ExecuteCommand::METHOD => {
                let p = params!(lsp::request::ExecuteCommand);
                if p.command == RELOAD_PLUGINS && !self.plugin_dirs.is_empty() {
                    let (registry, problems) = load_plugins(&self.plugin_dirs);
                    let old = std::mem::replace(&mut self.registry, registry);
                    let _ = old.close();
                    self.catalog = Catalog::from_registry(&self.registry);
                    for problem in problems {
                        self.show(lsp::MessageType::WARNING, &problem);
                    }
                    self.show(
                        lsp::MessageType::INFO,
                        &format!(
                            "Meridian loaded {} target plugins.",
                            self.catalog.names().len()
                        ),
                    );
                }
                self.respond(id, serde_json::Value::Null);
                return true;
            }
            _ => {
                let _ = self
                    .connection
                    .sender
                    .send(Message::Response(Response::new_err(
                        id,
                        ErrorCode::MethodNotFound as i32,
                        format!("unsupported request {}", request.method),
                    )));
            }
        }
        false
    }

    fn location(&self, l: &analysis::Location) -> Option<lsp::Location> {
        Some(lsp::Location {
            uri: uri_of(&l.file)?,
            range: self.range(&l.file, l.span),
        })
    }

    fn completion(&self, name: &str, index: usize, c: analysis::Completion) -> lsp::CompletionItem {
        lsp::CompletionItem {
            label: c.label.clone(),
            kind: Some(match c.kind {
                CompletionKind::Field => lsp::CompletionItemKind::FIELD,
                CompletionKind::Value => lsp::CompletionItemKind::ENUM_MEMBER,
                CompletionKind::Reference => lsp::CompletionItemKind::REFERENCE,
                CompletionKind::Type => lsp::CompletionItemKind::CLASS,
                CompletionKind::Snippet => lsp::CompletionItemKind::SNIPPET,
            }),
            detail: (!c.detail.is_empty()).then_some(c.detail),
            documentation: (!c.doc.is_empty()).then_some(lsp::Documentation::MarkupContent(
                lsp::MarkupContent {
                    kind: lsp::MarkupKind::Markdown,
                    value: c.doc,
                },
            )),
            // The order the language lists things in is the order to offer
            // them in: `id` before `type` before `state`.
            sort_text: Some(format!("{index:04}")),
            filter_text: Some(c.label),
            insert_text_format: Some(if c.snippet {
                lsp::InsertTextFormat::SNIPPET
            } else {
                lsp::InsertTextFormat::PLAIN_TEXT
            }),
            text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
                range: self.range(name, c.span),
                new_text: c.insert,
            })),
            ..Default::default()
        }
    }

    #[allow(deprecated)]
    fn symbol(&self, name: &str, s: &analysis::Symbol) -> lsp::DocumentSymbol {
        lsp::DocumentSymbol {
            name: if s.name.is_empty() {
                "(unnamed)".to_owned()
            } else {
                s.name.clone()
            },
            detail: (!s.detail.is_empty()).then(|| s.detail.clone()),
            kind: match s.kind {
                SymbolKind::Document => lsp::SymbolKind::MODULE,
                SymbolKind::Resource => lsp::SymbolKind::OBJECT,
                SymbolKind::Output => lsp::SymbolKind::PROPERTY,
            },
            tags: None,
            deprecated: None,
            range: self.range(name, s.span),
            selection_range: self.range(name, s.selection),
            children: Some(s.children.iter().map(|c| self.symbol(name, c)).collect()),
        }
    }

    /// Formats a file as one edit over the whole text, or nothing when it
    /// is already formatted. A file the formatter refuses is left alone and
    /// the reason shown.
    fn format(&self, name: &str) -> Option<Vec<lsp::TextEdit>> {
        let text = self.open.get(name)?;
        if !check::is_meridian(text) {
            return None;
        }
        let options = fmt::Options {
            sort_fields: self.sort_fields,
        };
        match fmt::format(name, text, options) {
            Ok(formatted) if formatted == *text => None,
            Ok(formatted) => {
                let lines = text.split('\n').count() as u32;
                Some(vec![lsp::TextEdit {
                    range: lsp::Range {
                        start: lsp::Position::new(0, 0),
                        end: lsp::Position::new(lines, 0),
                    },
                    new_text: formatted,
                }])
            }
            Err(e) => {
                self.show(lsp::MessageType::WARNING, &e.to_string());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_convert_between_bytes_and_utf16() {
        let line = "name: caf\u{e9} \u{1f600} x";
        // `é` is two bytes and one unit; the emoji is four bytes, two units.
        assert_eq!(to_utf16(line, 11), 10);
        assert_eq!(from_utf16(line, 10), 11);
        assert_eq!(to_utf16(line, 16), 13);
        assert_eq!(from_utf16(line, 13), 16);
        assert_eq!(from_utf16(line, 99), line.len());
        assert_eq!(to_utf16(line, 99), 15);
    }

    #[test]
    fn a_file_is_named_by_its_path_and_found_again() {
        // A Windows path has a drive, and a file URI without one is no path.
        let (text, path) = if cfg!(windows) {
            ("file:///C:/work/web.yaml", "C:\\work\\web.yaml")
        } else {
            ("file:///work/web.yaml", "/work/web.yaml")
        };
        let uri = Url::parse(text).unwrap();
        assert_eq!(name_of(&uri), path);
        assert_eq!(uri_of(path), Some(uri));
        let untitled = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(uri_of(&name_of(&untitled)), Some(untitled));
    }
}
