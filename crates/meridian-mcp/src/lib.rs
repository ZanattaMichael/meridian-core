//! The Meridian MCP server.
//!
//! An AI model that has never seen a Meridian document can write one that
//! compiles, if it can look up what exists, draft, be told exactly what is
//! wrong and what is accepted instead, and correct. This crate is that loop
//! as tools over the [Model Context Protocol](https://modelcontextprotocol.io):
//! `meridian mcp` speaks it on standard input and output, and a client such
//! as Claude Desktop, Claude Code, Cursor or VS Code starts it.
//!
//! It is a thin layer, like the language server. Every answer comes from
//! `meridian-tooling`, which asks the compiler and the loaded target
//! plugins; this crate turns those answers into the protocol and keeps every
//! result small enough for a small model's context.
//!
//! It is read-only. `compile` emits in memory and writes nothing; `plan` is
//! off unless the server is started with `--allow-plan`, and `apply`,
//! `rollback` and `destroy` are not offered. No secret is ever fetched: a
//! document is compiled with no data, as an editor compiles it. The server
//! makes no outbound connection, and a file argument is confined to the
//! directory it was started in.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use meridian_plugin_host::Registry;
use meridian_tooling::catalog::Catalog;
use serde_json::{json, Map, Value};

mod diagnostics;
mod explain;
mod tools;

pub use diagnostics::diagnostic;

/// The examples and pages embedded at build time.
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

/// Why the server stopped abnormally.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The protocol revisions this server speaks. The first is what it answers
/// a client that asks for one it does not know.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The name the server reports itself as.
pub const SERVER_NAME: &str = "meridian";

/// How `plan` is run when it is allowed: the `meridian` binary itself, with
/// `--output json`, so the result is exactly the object `meridian plan`
/// prints and nothing is formatted a second time.
#[derive(Clone, Debug)]
pub struct PlanCommand {
    /// The program to run, normally the running `meridian` binary.
    pub program: PathBuf,
    /// Arguments placed after `plan`: the plugin options the server itself
    /// was started with.
    pub args: Vec<String>,
}

/// What the server answers from.
pub struct Server {
    /// The loaded plugins; `None` serves the language alone.
    registry: Option<Registry>,
    catalog: Catalog,
    /// The directory file arguments are confined to.
    root: PathBuf,
    /// How `plan` runs, when it is allowed at all.
    plan: Option<PlanCommand>,
    /// Problems found while starting, told to the client once.
    notes: Vec<String>,
}

impl Server {
    /// A server over `registry`, whose catalog has had any cached tool
    /// schemas read into it already, confined to `root`.
    pub fn new(registry: Registry, catalog: Catalog, root: impl Into<PathBuf>) -> Server {
        Server {
            registry: Some(registry),
            catalog,
            root: root.into(),
            plan: None,
            notes: Vec::new(),
        }
    }

    /// A server that knows the language and nothing about any target.
    pub fn language_only(root: impl Into<PathBuf>) -> Server {
        Server {
            registry: None,
            catalog: Catalog::empty(),
            root: root.into(),
            plan: None,
            notes: Vec::new(),
        }
    }

    /// Offers `plan`, run as `command` says.
    pub fn allow_plan(mut self, command: PlanCommand) -> Server {
        self.plan = Some(command);
        self
    }

    /// Something to tell the client once it is connected.
    pub fn note(mut self, note: impl Into<String>) -> Server {
        self.notes.push(note.into());
        self
    }

    /// Hands the plugins back, so the caller can close them.
    pub fn into_registry(self) -> Option<Registry> {
        self.registry
    }

    /// `path` as a file inside the root, or why it is not.
    fn confined(&self, path: &str) -> Result<PathBuf, String> {
        let root = self
            .root
            .canonicalize()
            .map_err(|e| format!("the server's directory cannot be read: {e}"))?;
        let joined = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            root.join(path)
        };
        let full = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
        if !full.starts_with(&root) {
            return Err(format!(
                "{path} is outside {}; the server only reads files under the directory it was \
                 started in",
                root.display()
            ));
        }
        Ok(full)
    }
}

/// Serves one client over standard input and output until the input ends.
/// Plugins stay loaded for the whole conversation.
pub fn run_stdio(server: Server) -> Result<Option<Registry>, Error> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = server;
    serve(&mut stdin.lock(), &mut stdout.lock(), &mut server)?;
    Ok(server.into_registry())
}

/// Serves the protocol, one JSON-RPC message per line, from `input` to
/// `output`, until the input ends. This is what [`run_stdio`] does on the
/// standard streams; tests call it on strings.
pub fn serve(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    server: &mut Server,
) -> Result<(), Error> {
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                write_message(
                    output,
                    &error_response(
                        Value::Null,
                        PARSE_ERROR,
                        &format!("the line is not JSON: {e}"),
                    ),
                )?;
                continue;
            }
        };
        // A batch is answered as a batch; an empty one is invalid.
        match message {
            Value::Array(batch) if batch.is_empty() => write_message(
                output,
                &error_response(Value::Null, INVALID_REQUEST, "an empty batch"),
            )?,
            Value::Array(batch) => {
                let replies: Vec<Value> = batch
                    .into_iter()
                    .filter_map(|m| handle(server, m))
                    .collect();
                if !replies.is_empty() {
                    write_message(output, &Value::Array(replies))?;
                }
            }
            other => {
                if let Some(reply) = handle(server, other) {
                    write_message(output, &reply)?;
                }
            }
        }
    }
}

fn write_message(output: &mut dyn Write, message: &Value) -> std::io::Result<()> {
    // One message per line, as the stdio transport requires: the text must
    // hold no newline, which `to_string` guarantees.
    let text = serde_json::to_string(message).expect("a JSON value made here renders");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Handles one message. A request gets a response; a notification, and a
/// response the client sends to nothing, get none.
fn handle(server: &mut Server, message: Value) -> Option<Value> {
    let Value::Object(message) = message else {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "a message is a JSON object",
        ));
    };
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to a request this server never sends: nothing to do.
        return id
            .filter(|_| !message.contains_key("result") && !message.contains_key("error"))
            .map(|id| error_response(id, INVALID_REQUEST, "a request names a method"));
    };
    let params = message
        .get("params")
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    if method.starts_with("notifications/") {
        // `initialized` and `cancelled` need nothing from a server that
        // answers every request at once.
        return None;
    }
    let id = id?;
    let result = match method {
        "initialize" => Ok(initialize(server, &params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::list() })),
        "tools/call" => tools::call(server, &params),
        "prompts/list" => Ok(json!({ "prompts": tools::prompts() })),
        "prompts/get" => tools::prompt(server, &params),
        "resources/list" => Ok(json!({ "resources": tools::resources() })),
        "resources/read" => tools::resource(&params),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "logging/setLevel" => Ok(json!({})),
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => ok_response(id, result),
        Err((code, message)) => error_response(id, code, &message),
    })
}

fn initialize(server: &Server, params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut instructions = String::from(
        "Meridian is one YAML language that compiles to many infrastructure tools. To write a \
         document: read the `authoring-guide` prompt, then `list_targets`, \
         `list_resource_types` and `describe_resource_type` for what exists, draft, and check \
         the draft with `validate` and `compile`. Every diagnostic names the rule, the resource \
         and the line, and says what is accepted instead. Nothing here changes a machine: \
         `compile` writes no file, and there is no `apply`.",
    );
    if server.catalog.is_empty() {
        instructions.push_str(
            " No target plugin is loaded, so documents are checked against the language only; \
             `list_targets` is empty.",
        );
    }
    for note in &server.notes {
        instructions.push(' ');
        instructions.push_str(note);
    }
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false },
            "prompts": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
        },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions,
    })
}

/// The catalog, for the tools.
impl Server {
    pub(crate) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub(crate) fn registry(&self) -> Option<&Registry> {
        self.registry.as_ref()
    }

    pub(crate) fn plan_command(&self) -> Option<&PlanCommand> {
        self.plan.as_ref()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// The result of a tool call: structured content, and the same as text for
/// a client that shows only text.
pub(crate) fn tool_result(ok: bool, body: Value) -> Value {
    let text = serde_json::to_string_pretty(&body).expect("a JSON value made here renders");
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": body,
        "isError": !ok,
    })
}

/// A tool call that could not be made: a wrong argument, or something the
/// server cannot do. The message carries its own fix, as every error here
/// does.
pub(crate) fn tool_error(message: impl Into<String>) -> Value {
    let message = message.into();
    tool_result(false, json!({ "error": { "message": message } }))
}

/// A string argument, trimmed; `None` when absent or empty.
pub(crate) fn arg_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A whole-number argument within `1..=max`, or the default.
pub(crate) fn arg_usize(args: &Value, name: &str, default: usize, max: usize) -> usize {
    args.get(name)
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, max))
        .unwrap_or(default)
}

/// A map of name to value with the entries in a fixed order.
pub(crate) fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    let map: Map<String, Value> = entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    Value::Object(map)
}

/// The targets the catalog holds, by name, for messages that list them.
pub(crate) fn target_names(catalog: &Catalog) -> Vec<&str> {
    catalog.names()
}

/// Text cut to `max` characters at a line end, with how much was left out.
pub(crate) fn cut(text: &str, max: usize) -> (String, usize) {
    if text.chars().count() <= max {
        return (text.to_owned(), 0);
    }
    let mut end = text.char_indices().nth(max).map_or(text.len(), |(i, _)| i);
    if let Some(nl) = text[..end].rfind('\n') {
        if nl > end / 2 {
            end = nl + 1;
        }
    }
    let kept = &text[..end];
    (kept.to_owned(), text[end..].chars().count())
}

/// A page of `items`, with what to ask for next.
pub(crate) fn page_of(items: Vec<Value>, page: usize, size: usize) -> Map<String, Value> {
    let total = items.len();
    let start = (page - 1).saturating_mul(size).min(total);
    let end = (start + size).min(total);
    let mut out = Map::new();
    out.insert("total".to_owned(), total.into());
    out.insert("page".to_owned(), page.into());
    out.insert("pageSize".to_owned(), size.into());
    if end < total {
        out.insert("nextPage".to_owned(), (page + 1).into());
    }
    out.insert("items".to_owned(), Value::Array(items[start..end].to_vec()));
    out
}
