//! The tools, prompts and resources the server offers.
//!
//! Designed for small models: few tools, flat arguments, short structured
//! results, and every error carrying its own fix. Results are paged and
//! capped, `describe_resource_type` returns one type at a time, `compile`
//! can return one named file, and long text is cut with a count of what was
//! left out.

use std::collections::BTreeMap;

use meridian_ast::ResourceGraph;
use meridian_ir::schema::{self as language, Choice, Field, Shape, Type};
use meridian_ir::{Document, Kind, Resolution};
use meridian_plugin_host::{to_sdk, EmitError, ImportFailure};
use meridian_sdk::{Data, ImportSource, ParamKind, ParamSchema, ResourceSchema};
use meridian_tooling::catalog::Target;
use meridian_tooling::check::{self, Diagnostic, Env, Severity, Source, Workspace};
use meridian_tooling::{fmt, lint};
use serde_json::{json, Map, Value};

use crate::diagnostics::{diagnostic, summary};
use crate::{
    arg_str, arg_usize, cut, embedded, explain, object, page_of, tool_error, tool_result, Server,
};

/// How much of one emitted file `compile` returns unasked.
const FILE_CHARS: usize = 12_000;
/// How much of one file it returns when that file is asked for by name.
const NAMED_FILE_CHARS: usize = 64_000;
/// How much of an example or a page is returned.
const TEXT_CHARS: usize = 32_000;
/// The most text a document argument may hold.
const MAX_DOCUMENT_CHARS: usize = 512_000;
/// How deep `describe_resource_type` lists nested params.
const PARAM_DEPTH: usize = 3;
/// The default and the most items of a page.
const PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 200;

/// The name a document given as text goes by in diagnostics.
const INLINE_NAME: &str = "document.yaml";

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
    })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn integer(description: &str) -> Value {
    json!({ "type": "integer", "minimum": 1, "description": description })
}

const DOCUMENT_ARG: &str =
    "The Meridian document as YAML or JSON text. May hold several documents \
                            separated by `---`. Give this or `file`.";
const FILE_ARG: &str =
    "A file under the directory the server was started in, instead of `document`.";

/// Every tool, in the order a model should learn them.
pub(crate) fn list() -> Vec<Value> {
    vec![
        tool(
            "list_targets",
            "The targets (tools) that are installed: each one's name for `spec.target`, its \
             category, the document kinds it takes, what it can do, the tool versions it \
             compiles for, how many resource types it has, and the formats it imports. Start \
             here.",
            json!({}),
            &[],
        ),
        tool(
            "list_resource_types",
            "The resource types of one target, with a one-line description each, paged. \
             `search` keeps the types whose name or description holds the word. A name with \
             `<...>` in it is a pattern: `<provider>_<type>` means any Terraform resource, such \
             as `aws_instance`.",
            json!({
                "target": string("The target, from `list_targets`."),
                "search": string("A word to look for in type names and descriptions."),
                "page": integer("The page to return, from 1."),
                "pageSize": integer("How many types per page; at most 200, default 50."),
            }),
            &["target"],
        ),
        tool(
            "describe_resource_type",
            "Everything about one resource type on one target: its params (name, kind, \
             required, allowed values, nested params), its states and default state, the \
             actions it performs when notified, and a minimal valid example document.",
            json!({
                "target": string("The target, from `list_targets`."),
                "type": string("The resource type, from `list_resource_types`."),
            }),
            &["target", "type"],
        ),
        tool(
            "describe_document",
            "The fields a document kind takes, from the parser's own tables: every field, \
             what it holds, whether it is required, and its fixed values where it has them.",
            json!({
                "kind": string("`Infrastructure` or `ResourceSet`. Leave out to list the kinds."),
            }),
            &[],
        ),
        tool(
            "get_example",
            "A worked example that compiles: by `name` for one example's text, or by \
             `target` and `search` to find one. With no arguments, every example is listed.",
            json!({
                "name": string("The example's name, as `get_example` lists it, such as `ansible/web-server.yaml`."),
                "target": string("Keep the examples written for this target."),
                "search": string("A word to look for in example names and descriptions, such as `secrets` or `conditions`."),
            }),
            &[],
        ),
        tool(
            "validate",
            "Checks a document against the language and against what its target declares \
             about its resource types, params and states. Starts no tool and no plugin \
             process. Returns diagnostics; an empty list means the document is valid.",
            json!({ "document": string(DOCUMENT_ARG), "file": string(FILE_ARG) }),
            &[],
        ),
        tool(
            "compile",
            "Compiles a document with its target's real plugin, in memory, and returns the \
             files the tool would be given, or the refusal as diagnostics. Nothing is written \
             and no tool runs. `file` returns one emitted file in full; without it every file \
             is returned, cut when long.",
            json!({
                "document": string(DOCUMENT_ARG),
                "file": string(FILE_ARG),
                "emittedFile": string("The name of one emitted file to return in full, as a previous `compile` listed it."),
            }),
            &[],
        ),
        tool(
            "lint",
            "Every error `validate` and `compile` would report, plus things that compile and \
             are probably mistakes, with a quick fix where the linter has one. `fixed` is the \
             document with every quick fix applied.",
            json!({ "document": string(DOCUMENT_ARG), "file": string(FILE_ARG) }),
            &[],
        ),
        tool(
            "format",
            "The document in the standard layout, changing no value. Fails only when the \
             text does not parse.",
            json!({ "document": string(DOCUMENT_ARG), "file": string(FILE_ARG) }),
            &[],
        ),
        tool(
            "explain",
            "What a diagnostic's rule means, why the target refuses it, and what to write \
             instead. Every diagnostic carries the call to make.",
            json!({
                "rule": string("The rule name from a diagnostic's `code`, such as `unsupported-state`."),
                "target": string("The target the diagnostic came from, if any."),
            }),
            &["rule"],
        ),
        tool(
            "import",
            "Turns a tool's own configuration (a Terraform file, a Compose file, a \
             playbook) into Meridian documents, with notes on what could not be carried \
             over. `list_targets` says which formats each target imports.",
            json!({
                "target": string("The target whose importer reads the source, such as `terraform` or `docker`."),
                "source": string("The configuration text."),
                "fileName": string("The file name the source would have, such as `main.tf` or `compose.yaml`; the importer reads the format from it. Defaults to the format's usual name."),
                "name": string("What to call the imported document; the importer chooses when left out."),
            }),
            &["target", "source"],
        ),
        tool(
            "plan",
            "Asks the tool what applying a file would change, by running `meridian plan` \
             on it. Only offered when the server was started with `--allow-plan`: it runs \
             the real tool and takes the project's state lock. Nothing is applied.",
            json!({
                "file": string("The document file to plan, under the directory the server was started in."),
                "data": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Data files for `--data`, under the same directory, in layering order.",
                },
            }),
            &["file"],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

/// Answers `tools/call`. A tool that cannot run returns a result with
/// `isError`, never a protocol error: the protocol error is for a call the
/// server cannot read.
pub(crate) fn call(server: &Server, params: &Value) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((crate::INVALID_PARAMS, "tools/call names a tool".to_owned()));
    };
    let empty = Value::Object(Map::new());
    let args = params.get("arguments").unwrap_or(&empty);
    if !args.is_object() {
        return Err((crate::INVALID_PARAMS, "`arguments` is an object".to_owned()));
    }
    Ok(match name {
        "list_targets" => list_targets(server),
        "list_resource_types" => list_resource_types(server, args),
        "describe_resource_type" => describe_resource_type(server, args),
        "describe_document" => describe_document(args),
        "get_example" => get_example(args),
        "validate" => validate(server, args),
        "compile" => compile(server, args),
        "lint" => lint_tool(server, args),
        "format" => format(server, args),
        "explain" => {
            let Some(rule) = arg_str(args, "rule") else {
                return Ok(tool_error("`rule` is required: the `code` of a diagnostic"));
            };
            tool_result(
                true,
                explain::explain(server, rule, arg_str(args, "target")),
            )
        }
        "import" => import(server, args),
        "plan" => plan(server, args),
        _ => tool_error(format!(
            "there is no tool called `{name}`; `tools/list` gives the tools"
        )),
    })
}

/// The document a tool was given: as text, or read from a file under the
/// root.
fn document_arg(server: &Server, args: &Value) -> Result<(String, String), String> {
    if let Some(text) = args.get("document").and_then(Value::as_str) {
        if text.chars().count() > MAX_DOCUMENT_CHARS {
            return Err(format!(
                "the document is longer than {MAX_DOCUMENT_CHARS} characters; give it as a file"
            ));
        }
        if text.trim().is_empty() {
            return Err("`document` is empty".to_owned());
        }
        return Ok((INLINE_NAME.to_owned(), text.to_owned()));
    }
    if let Some(file) = arg_str(args, "file") {
        let path = server.confined(file)?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {file}: {e}"))?;
        return Ok((file.to_owned(), text));
    }
    Err("give the document as `document` (its text) or `file` (a path under the server's directory)".to_owned())
}

fn workspace_of(name: &str, text: &str) -> Workspace {
    Workspace::new(&[Source::new(name, text)])
}

fn env<'a>(server: &'a Server, with_plugins: bool) -> Env<'a> {
    Env {
        registry: if with_plugins {
            server.registry()
        } else {
            None
        },
        catalog: server.catalog(),
        data: None,
    }
}

fn diagnostics_json(workspace: &Workspace, found: &[Diagnostic]) -> Value {
    Value::Array(found.iter().map(|d| diagnostic(workspace, d)).collect())
}

fn has_errors(found: &[Diagnostic]) -> bool {
    found.iter().any(|d| d.severity == Severity::Error)
}

/// What a document's kinds and target are, for the result's header.
fn documents_json(workspace: &Workspace) -> Value {
    let mut out = Vec::new();
    for file in &workspace.files {
        for parsed in &file.docs {
            out.push(json!({
                "name": parsed.doc.name(),
                "kind": parsed.doc.kind().as_str(),
                "target": parsed.doc.target(),
            }));
        }
    }
    Value::Array(out)
}

// --- list_targets ---------------------------------------------------------

fn kinds_of(target: &Target) -> Vec<&'static str> {
    [Kind::Infrastructure, Kind::ResourceSet]
        .into_iter()
        .filter(|k| target.refuses_kind(*k).is_none())
        .map(Kind::as_str)
        .collect()
}

fn list_targets(server: &Server) -> Value {
    let targets: Vec<Value> = server
        .catalog()
        .targets()
        .map(|t| {
            let info = server
                .registry()
                .and_then(|r| r.target(&t.name))
                .map(|p| p.info());
            let imports: Vec<Value> = info
                .map(|i| {
                    i.import_formats
                        .iter()
                        .map(|f| {
                            json!({ "name": f.name, "description": f.description, "patterns": f.patterns })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut capabilities = Vec::new();
            if t.capabilities.native_notify || t.capabilities.synthetic_notify {
                capabilities.push("notify");
            }
            if t.capabilities.runtime_condition {
                capabilities.push("runtimeWhen");
            }
            if t.capabilities.secrets {
                capabilities.push("secrets");
            }
            if t.capabilities.credentials {
                capabilities.push("credentials");
            }
            if t.capabilities.adopt != meridian_sdk::Adoption::None {
                capabilities.push("adopt");
            }
            let mut entry = json!({
                "name": t.name,
                "category": t.category.as_str(),
                "kinds": kinds_of(t),
                "can": t.can(),
                "capabilities": capabilities,
                "versions": t.versions,
                "resourceTypes": t.types.len(),
                "imports": imports,
            });
            if let Some(info) = info {
                if !info.settings_keys.is_empty() {
                    entry["settingsKeys"] = json!(info.settings_keys);
                }
            }
            if let Some(cache) = &t.cache {
                entry["toolSchemas"] = json!({
                    "from": cache.versions,
                    "types": cache.types,
                    "freshness": cache.freshness.describe(),
                });
            }
            entry
        })
        .collect();
    let mut body = object([("targets", Value::Array(targets))]);
    if server.catalog().is_empty() {
        body["note"] = "no target plugin is loaded: put the meridian-target-* binaries beside \
                        the meridian binary, or start the server with --plugin-dir"
            .into();
    }
    tool_result(true, body)
}

// --- list_resource_types --------------------------------------------------

fn target_arg<'a>(server: &'a Server, args: &Value) -> Result<&'a Target, String> {
    let Some(name) = arg_str(args, "target") else {
        return Err("`target` is required; `list_targets` gives the names".to_owned());
    };
    server.catalog().target(name).ok_or_else(|| {
        let names = crate::target_names(server.catalog());
        if names.is_empty() {
            "no target plugin is loaded, so no target can be described".to_owned()
        } else {
            format!(
                "there is no target `{name}` (loaded targets: {})",
                names.join(", ")
            )
        }
    })
}

fn list_resource_types(server: &Server, args: &Value) -> Value {
    let target = match target_arg(server, args) {
        Ok(t) => t,
        Err(e) => return tool_error(e),
    };
    let search = arg_str(args, "search").map(str::to_lowercase);
    let items: Vec<Value> = target
        .types
        .iter()
        .filter(|t| {
            search.as_ref().is_none_or(|w| {
                t.r#type.to_lowercase().contains(w) || t.description.to_lowercase().contains(w)
            })
        })
        .map(|t| {
            let mut entry =
                json!({ "type": t.r#type, "description": first_sentence(&t.description) });
            if t.is_pattern() {
                entry["pattern"] = true.into();
            }
            if t.open {
                entry["open"] = true.into();
            }
            if !t.source.is_empty() {
                entry["from"] = t.source.clone().into();
            }
            entry
        })
        .collect();
    let page = arg_usize(args, "page", 1, usize::MAX);
    let size = arg_usize(args, "pageSize", PAGE_SIZE, MAX_PAGE_SIZE);
    let mut body = Map::new();
    body.insert("target".to_owned(), target.name.clone().into());
    body.extend(page_of(items, page, size));
    if target.types.is_empty() {
        body.insert(
            "note".to_owned(),
            "this target lists no resource types; any type name is passed to the tool as written"
                .into(),
        );
    }
    tool_result(true, Value::Object(body))
}

fn first_sentence(text: &str) -> String {
    let text = text.trim();
    match text.find(". ") {
        Some(at) if at < 200 => text[..=at].to_owned(),
        _ => cut(text, 200).0,
    }
}

// --- describe_resource_type -----------------------------------------------

fn param_json(p: &ParamSchema, depth: usize) -> Value {
    let mut out = json!({
        "name": p.name,
        "kind": p.kind.as_str(),
        "required": p.required,
    });
    if !p.description.is_empty() {
        out["description"] = p.description.clone().into();
    }
    if !p.values.is_empty() {
        out["values"] = json!(p.values);
    }
    if p.computed {
        out["computed"] = true.into();
    }
    if p.sensitive {
        out["sensitive"] = true.into();
    }
    if p.deprecated {
        out["deprecated"] = true.into();
    }
    if p.open {
        out["open"] = true.into();
    }
    if !p.params.is_empty() {
        if depth > 0 {
            out["params"] =
                Value::Array(p.params.iter().map(|q| param_json(q, depth - 1)).collect());
        } else {
            out["nestedParams"] = p.params.len().into();
        }
    }
    out
}

/// A value of the kind a param takes, for an example.
fn placeholder(p: &ParamSchema) -> Value {
    if let Some(first) = p.values.first() {
        return first.clone().into();
    }
    match p.kind {
        ParamKind::Bool => true.into(),
        ParamKind::Integer | ParamKind::Number => 1.into(),
        ParamKind::List => {
            if p.params.is_empty() {
                json!([])
            } else {
                json!([required_map(&p.params)])
            }
        }
        ParamKind::Map => required_map(&p.params),
        ParamKind::String | ParamKind::Any => "example".into(),
    }
}

fn required_map(params: &[ParamSchema]) -> Value {
    let map: Map<String, Value> = params
        .iter()
        .filter(|p| p.required && !p.computed)
        .map(|p| (p.name.clone(), placeholder(p)))
        .collect();
    Value::Object(map)
}

/// A minimal document using `schema` on `target`, as YAML.
fn example_document(target: &Target, schema: &ResourceSchema, type_name: &str) -> String {
    let kinds = kinds_of(target);
    let resource_set = kinds == ["ResourceSet"];
    let mut resource = Map::new();
    resource.insert("id".to_owned(), "example".into());
    resource.insert("type".to_owned(), type_name.into());
    if !schema.default_state.is_empty() {
        resource.insert("state".to_owned(), schema.default_state.clone().into());
    } else if let Some(state) = schema.states.first() {
        resource.insert("state".to_owned(), state.clone().into());
    }
    let params = required_map(&schema.params);
    if !params.as_object().is_some_and(Map::is_empty) {
        resource.insert("params".to_owned(), params);
    }
    let mut spec = Map::new();
    spec.insert("target".to_owned(), target.name.clone().into());
    if resource_set {
        spec.insert(
            "targetHost".to_owned(),
            json!({ "from": "inline", "host": "host01.example.com" }),
        );
    }
    spec.insert("resources".to_owned(), json!([Value::Object(resource)]));
    let doc = json!({
        "apiVersion": "meridian/v1",
        "kind": if resource_set { "ResourceSet" } else { "Infrastructure" },
        "metadata": { "name": "example" },
        "spec": Value::Object(spec),
    });
    let mut text = String::new();
    yaml(&doc, 0, &mut text);
    // The formatter settles the layout the way `meridian fmt` would.
    fmt::format("example.yaml", &text, fmt::Options::default()).unwrap_or(text)
}

/// Writes a value as block YAML. The values here are the ones the example
/// is built from: short strings, numbers, booleans, mappings and lists.
fn yaml(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match inner {
                    Value::Object(m) if !m.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        yaml(inner, indent + 2, out);
                    }
                    Value::Array(a) if !a.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        yaml(inner, indent + 2, out);
                    }
                    _ => out.push_str(&format!("{pad}{key}: {}\n", scalar(inner))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(m) if !m.is_empty() => {
                        let mut inner = String::new();
                        yaml(item, indent + 2, &mut inner);
                        // The first line of the mapping sits beside the dash.
                        let first = inner.trim_start_matches(' ');
                        out.push_str(&format!("{pad}- {first}"));
                    }
                    _ => out.push_str(&format!("{pad}- {}\n", scalar(item))),
                }
            }
        }
        _ => out.push_str(&format!("{pad}{}\n", scalar(value))),
    }
}

/// A scalar as YAML text. A string that YAML would read as something else
/// is quoted, which is what the writer does for a compiled document too.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => {
            let plain = !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:@".contains(c))
                && !s.chars().all(|c| c.is_ascii_digit() || c == '.')
                && !matches!(
                    s.as_str(),
                    "true" | "false" | "null" | "yes" | "no" | "on" | "off"
                );
            if plain {
                s.clone()
            } else {
                serde_json::to_string(s).expect("renders")
            }
        }
        Value::Object(_) => "{}".to_owned(),
        Value::Array(_) => "[]".to_owned(),
        other => other.to_string(),
    }
}

fn describe_resource_type(server: &Server, args: &Value) -> Value {
    let target = match target_arg(server, args) {
        Ok(t) => t,
        Err(e) => return tool_error(e),
    };
    let Some(name) = arg_str(args, "type") else {
        return tool_error("`type` is required; `list_resource_types` gives the names");
    };
    let Some(schema) = target.resource(name) else {
        let known: Vec<&str> = target.types.iter().map(|t| t.r#type.as_str()).collect();
        return tool_error(if known.is_empty() {
            format!(
                "target `{}` lists no resource types, so `{name}` cannot be described; it is \
                 passed to the tool as written",
                target.name
            )
        } else if known.len() <= 40 {
            format!(
                "target `{}` has no resource type `{name}` (it has: {})",
                target.name,
                known.join(", ")
            )
        } else {
            format!(
                "target `{}` has no resource type `{name}`; `list_resource_types` with a \
                 `search` word finds the right one",
                target.name
            )
        });
    };
    // The example is checked the way `validate` checks, so a model is never
    // handed a starting point the target would refuse.
    let example = example_document(target, schema, name);
    let workspace = workspace_of("example.yaml", &example);
    let found = check::check(&workspace, &env(server, false));
    let mut body = json!({
        "target": target.name,
        "type": schema.r#type,
        "description": schema.description,
        "open": schema.open,
        "states": schema.states,
        "defaultState": schema.default_state,
        "actions": schema.actions,
        "params": schema.params.iter().map(|p| param_json(p, PARAM_DEPTH)).collect::<Vec<_>>(),
        "example": example,
        "exampleValid": !has_errors(&found),
    });
    if schema.is_pattern() {
        body["pattern"] = true.into();
        body["note"] = format!(
            "`{}` is a pattern: `{name}` is one name that fits it. The params are the tool's \
             own, checked by the tool; `meridian schema sync` can cache them for checking here.",
            schema.r#type
        )
        .into();
    }
    if !schema.source.is_empty() {
        body["from"] = schema.source.clone().into();
    }
    if schema.open {
        body["note"] = "the type takes params beyond the ones listed; they are passed to the \
                        tool as written"
            .into();
    }
    tool_result(true, body)
}

// --- describe_document ----------------------------------------------------

fn field_json(field: &Field, depth: usize) -> Value {
    let (ty, extra): (&str, Option<Value>) = match &field.ty {
        Type::String => ("string", None),
        Type::Bool => ("boolean", None),
        Type::StringList => ("list of strings", None),
        Type::StringMap => ("mapping of strings", None),
        Type::Params => ("mapping; its keys belong to the resource type", None),
        Type::Enum(choices) => (
            "one of",
            Some(Value::Array(
                choices
                    .iter()
                    .map(|Choice { value, doc }| json!({ "value": value, "meaning": doc }))
                    .collect(),
            )),
        ),
        Type::Object(shape) => ("mapping", Some(shape_json(shape, depth))),
        Type::List(shape) => ("list of mappings", Some(shape_json(shape, depth))),
    };
    let mut out = json!({
        "name": field.name,
        "type": ty,
        "required": field.required,
        "doc": field.doc,
    });
    match (&field.ty, extra) {
        (Type::Enum(_), Some(values)) => out["values"] = values,
        (_, Some(fields)) => out["fields"] = fields["fields"].clone(),
        _ => {}
    }
    out
}

fn shape_json(shape: &Shape, depth: usize) -> Value {
    if depth == 0 {
        return json!({ "name": shape.name, "fields": [] });
    }
    json!({
        "name": shape.name,
        "doc": shape.doc,
        "fields": shape.fields.iter().map(|f| field_json(f, depth - 1)).collect::<Vec<_>>(),
    })
}

fn describe_document(args: &Value) -> Value {
    let Some(name) = arg_str(args, "kind") else {
        let kinds: Vec<Value> = language::DOCUMENTS
            .iter()
            .map(|(kind, shape)| json!({ "kind": kind.as_str(), "doc": shape.doc }))
            .collect();
        return tool_result(true, object([("kinds", Value::Array(kinds))]));
    };
    let Some(kind) = language::kind_named(name) else {
        let known: Vec<&str> = language::DOCUMENTS
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        return tool_error(format!(
            "there is no document kind `{name}` (the kinds are: {})",
            known.join(", ")
        ));
    };
    let shape = language::document(kind);
    let mut body = shape_json(shape, 8);
    body["kind"] = kind.as_str().into();
    tool_result(true, body)
}

// --- get_example ----------------------------------------------------------

/// What an example is about: its leading comment, the targets and kinds it
/// names.
struct Example {
    name: &'static str,
    text: &'static str,
    description: String,
    targets: Vec<String>,
    kinds: Vec<String>,
}

fn examples() -> Vec<Example> {
    embedded::EXAMPLES
        .iter()
        .filter(|(_, text)| check::is_meridian(text))
        .map(|(name, text)| {
            let description: Vec<&str> = text
                .lines()
                .take_while(|l| l.starts_with('#'))
                .map(|l| l.trim_start_matches('#').trim())
                .take_while(|l| !l.is_empty())
                .collect();
            let mut targets = Vec::new();
            let mut kinds = Vec::new();
            for line in text.lines() {
                if let Some(t) = line.strip_prefix("  target: ") {
                    let t = t.trim().trim_matches('"').to_owned();
                    if !targets.contains(&t) {
                        targets.push(t);
                    }
                }
                if let Some(k) = line.strip_prefix("kind: ") {
                    let k = k.trim().to_owned();
                    if !kinds.contains(&k) {
                        kinds.push(k);
                    }
                }
            }
            Example {
                name,
                text,
                description: description.join(" "),
                targets,
                kinds,
            }
        })
        .collect()
}

fn example_entry(e: &Example) -> Value {
    json!({
        "name": e.name,
        "targets": e.targets,
        "kinds": e.kinds,
        "description": cut(&e.description, 300).0,
    })
}

fn get_example(args: &Value) -> Value {
    let all = examples();
    if let Some(name) = arg_str(args, "name") {
        let name = name.trim_start_matches("examples/");
        let Some(found) = all.iter().find(|e| e.name == name) else {
            return tool_error(format!(
                "there is no example called `{name}`; call `get_example` with no arguments \
                 for the list"
            ));
        };
        let (text, omitted) = cut(found.text, TEXT_CHARS);
        let mut body = example_entry(found);
        body["text"] = text.into();
        if omitted > 0 {
            body["omittedChars"] = omitted.into();
        }
        return tool_result(true, body);
    }
    let target = arg_str(args, "target").map(str::to_lowercase);
    let search = arg_str(args, "search").map(str::to_lowercase);
    let matching: Vec<&Example> = all
        .iter()
        .filter(|e| {
            target
                .as_ref()
                .is_none_or(|t| e.targets.iter().any(|x| x == t))
        })
        .filter(|e| {
            search.as_ref().is_none_or(|w| {
                e.name.to_lowercase().contains(w) || e.description.to_lowercase().contains(w)
            })
        })
        .collect();
    if matching.len() == 1 {
        let found = matching[0];
        let (text, omitted) = cut(found.text, TEXT_CHARS);
        let mut body = example_entry(found);
        body["text"] = text.into();
        if omitted > 0 {
            body["omittedChars"] = omitted.into();
        }
        return tool_result(true, body);
    }
    let items: Vec<Value> = matching.iter().map(|e| example_entry(e)).collect();
    let mut body = page_of(items, 1, MAX_PAGE_SIZE);
    if matching.is_empty() {
        body.insert(
            "note".to_owned(),
            "nothing matched; call `get_example` with no arguments for every example".into(),
        );
    } else {
        body.insert(
            "note".to_owned(),
            "call `get_example` with one `name` for its text".into(),
        );
    }
    tool_result(true, Value::Object(body))
}

// --- validate, lint, format -----------------------------------------------

fn validate(server: &Server, args: &Value) -> Value {
    let (name, text) = match document_arg(server, args) {
        Ok(d) => d,
        Err(e) => return tool_error(e),
    };
    let workspace = workspace_of(&name, &text);
    let found = check::check(&workspace, &env(server, false));
    let ok = !has_errors(&found);
    tool_result(
        ok,
        object([
            ("ok", ok.into()),
            ("documents", documents_json(&workspace)),
            ("diagnostics", diagnostics_json(&workspace, &found)),
            ("summary", summary(&found)),
        ]),
    )
}

fn lint_tool(server: &Server, args: &Value) -> Value {
    let (name, text) = match document_arg(server, args) {
        Ok(d) => d,
        Err(e) => return tool_error(e),
    };
    let workspace = workspace_of(&name, &text);
    let mut found = check::check(&workspace, &env(server, true));
    found.extend(lint::lint(&workspace, server.catalog()));
    found.sort_by(|a, b| (a.span.start, &a.code).cmp(&(b.span.start, &b.code)));
    let (fixed, applied) = lint::apply_fixes(&name, &text, &found);
    let ok = !has_errors(&found);
    let mut body = object([
        ("ok", ok.into()),
        ("documents", documents_json(&workspace)),
        ("diagnostics", diagnostics_json(&workspace, &found)),
        ("summary", summary(&found)),
    ]);
    if applied > 0 {
        body["fixesApplied"] = applied.into();
        body["fixed"] = fixed.into();
    }
    tool_result(ok, body)
}

fn format(server: &Server, args: &Value) -> Value {
    let (name, text) = match document_arg(server, args) {
        Ok(d) => d,
        Err(e) => return tool_error(e),
    };
    match fmt::format(&name, &text, fmt::Options::default()) {
        Ok(formatted) => tool_result(
            true,
            object([
                ("changed", (formatted != text).into()),
                ("document", formatted.into()),
            ]),
        ),
        Err(e) => tool_error(format!(
            "{e}; `validate` says where the text does not parse"
        )),
    }
}

// --- compile --------------------------------------------------------------

/// Emits one document the way the checker compiles it, and returns the
/// files, or why it could not: `None` when the document needs data.
fn emit(
    server: &Server,
    workspace: &Workspace,
    file: &check::Parsed,
    parsed: &check::ParsedDoc,
) -> Result<Option<meridian_sdk::Emitted>, String> {
    let data = Data::new();
    let doc = match meridian_ir::resolve(&parsed.doc, &data) {
        Ok(Resolution::Kept { document, .. }) => *document,
        Ok(Resolution::Skipped { condition }) => {
            return Err(format!(
                "skipped: `spec.when` ({condition}) is false with no data"
            ))
        }
        Err(_) => return Ok(None),
    };
    if meridian_expr::has_interpolation(doc.target()) || file.interpolates(parsed.index) {
        return Ok(None);
    }
    let Some(plugin) = server.registry().and_then(|r| r.target(doc.target())) else {
        return Err(format!("no plugin provides target `{}`", doc.target()));
    };
    let bundle = workspace.bundle_for(&file.name);
    let graph = match &doc {
        Document::Infrastructure(infra) => ResourceGraph::build_infrastructure(infra),
        Document::ResourceSet(rs) => {
            let host = bundle
                .resolve_target_host(rs)
                .map(|(_, output)| output.value.clone())
                .ok()
                .filter(|host| meridian_sdk::check_host(host).is_ok())
                .unwrap_or_else(|| "host".to_owned());
            ResourceGraph::build(rs, &host)
        }
    }
    .map_err(|e| e.msg)?;
    match plugin.emit(&to_sdk(&graph), &data) {
        Ok(emitted) => Ok(Some(emitted)),
        Err(EmitError::Refused(e)) => Err(e.msg),
        Err(EmitError::Plugin(e)) => Err(e.to_string()),
    }
}

fn compile(server: &Server, args: &Value) -> Value {
    let (name, text) = match document_arg(server, args) {
        Ok(d) => d,
        Err(e) => return tool_error(e),
    };
    if server.registry().is_none() {
        return tool_error(
            "no target plugin is loaded, so nothing can be compiled; `validate` checks the \
             document against the language",
        );
    }
    let wanted = arg_str(args, "emittedFile");
    let workspace = workspace_of(&name, &text);
    let found = check::check(&workspace, &env(server, true));
    let mut documents = Vec::new();
    let mut extra: Vec<Diagnostic> = Vec::new();
    let mut ok = !has_errors(&found);
    if ok {
        for file in &workspace.files {
            for parsed in &file.docs {
                let mut entry = json!({
                    "name": parsed.doc.name(),
                    "kind": parsed.doc.kind().as_str(),
                    "target": parsed.doc.target(),
                });
                match emit(server, &workspace, file, parsed) {
                    Ok(Some(emitted)) => {
                        let mut files = Vec::new();
                        for (path, content) in &emitted.artifact.files {
                            if wanted.is_some_and(|w| w != path) {
                                files.push(
                                    json!({ "name": path, "chars": content.chars().count() }),
                                );
                                continue;
                            }
                            let limit = if wanted.is_some() {
                                NAMED_FILE_CHARS
                            } else {
                                FILE_CHARS
                            };
                            let (kept, omitted) = cut(content, limit);
                            let mut f = json!({ "name": path, "chars": content.chars().count(), "text": kept });
                            if omitted > 0 {
                                f["omittedChars"] = omitted.into();
                                f["note"] = format!(
                                    "cut; `compile` with `emittedFile: {path:?}` returns it in full"
                                )
                                .into();
                            }
                            files.push(f);
                        }
                        if let Some(w) = wanted {
                            if !emitted.artifact.files.contains_key(w) {
                                entry["note"] = format!(
                                    "no emitted file is called `{w}`; the files are listed by name"
                                )
                                .into();
                            }
                        }
                        entry["files"] = Value::Array(files);
                    }
                    Ok(None) => {
                        entry["files"] = json!([]);
                        extra.push(Diagnostic {
                            file: file.name.clone(),
                            span: file.span_of(parsed.index, "spec", 0, 0),
                            severity: Severity::Info,
                            code: "needs-data".to_owned(),
                            message: format!(
                                "document {:?} reads data or a secret, so it was checked against \
                                 the language and not compiled here; `meridian compile --data` \
                                 compiles it with values",
                                parsed.doc.name()
                            ),
                            source: "meridian".to_owned(),
                            fix: None,
                        });
                    }
                    Err(message) => {
                        ok = false;
                        entry["files"] = json!([]);
                        extra.push(Diagnostic {
                            file: file.name.clone(),
                            span: file.span_of(parsed.index, "spec.target", 0, 0),
                            severity: Severity::Error,
                            code: "refused".to_owned(),
                            message,
                            source: parsed.doc.target().to_owned(),
                            fix: None,
                        });
                    }
                }
                documents.push(entry);
            }
        }
    }
    let mut all = found;
    all.extend(extra);
    tool_result(
        ok,
        object([
            ("ok", ok.into()),
            ("documents", Value::Array(documents)),
            ("diagnostics", diagnostics_json(&workspace, &all)),
            ("summary", summary(&all)),
        ]),
    )
}

// --- import ---------------------------------------------------------------

fn import(server: &Server, args: &Value) -> Value {
    let Some(target_name) = arg_str(args, "target") else {
        return tool_error("`target` is required: the target whose importer reads the source");
    };
    let Some(source) = args.get("source").and_then(Value::as_str) else {
        return tool_error("`source` is required: the configuration text");
    };
    if source.chars().count() > MAX_DOCUMENT_CHARS {
        return tool_error(format!(
            "the source is longer than {MAX_DOCUMENT_CHARS} characters; `meridian import` reads files"
        ));
    }
    let Some(plugin) = server.registry().and_then(|r| r.target(target_name)) else {
        return tool_error(format!(
            "there is no target `{target_name}` (loaded targets: {})",
            crate::target_names(server.catalog()).join(", ")
        ));
    };
    let formats = &plugin.info().import_formats;
    if formats.is_empty() {
        return tool_error(format!(
            "target `{target_name}` does not import; `list_targets` says which targets do"
        ));
    }
    let file_name = arg_str(args, "fileName")
        .map(str::to_owned)
        .or_else(|| {
            formats
                .first()
                .and_then(|f| f.patterns.first())
                .map(|p| p.replace('*', "import").replace('?', "x"))
        })
        .unwrap_or_else(|| "import".to_owned());
    if file_name.contains(['/', '\\']) {
        return tool_error("`fileName` is a bare file name, with no directory");
    }
    let request = ImportSource {
        files: BTreeMap::from([(file_name.clone(), source.to_owned())]),
        name: arg_str(args, "name").unwrap_or("").to_owned(),
        root: String::new(),
    };
    let imported = match plugin.import(&request) {
        Ok(imported) => imported,
        Err(ImportFailure::Source(e)) => {
            return tool_result(
                false,
                json!({
                    "ok": false,
                    "diagnostics": [{
                        "file": if e.file.is_empty() { file_name } else { e.file },
                        "line": e.line,
                        "severity": "error",
                        "code": "import-source",
                        "source": target_name,
                        "message": e.msg,
                        "explain": format!("explain(rule='import-source', target='{target_name}')"),
                    }],
                }),
            )
        }
        Err(ImportFailure::Plugin(e)) => return tool_error(e.to_string()),
    };
    let rendered = meridian_import::render_all(&imported, &file_name);
    let documents: Vec<Value> = rendered
        .iter()
        .map(|r| {
            let (text, omitted) = cut(&r.text, TEXT_CHARS);
            let mut d =
                json!({ "name": r.name, "fileName": r.file_name, "todos": r.todos, "text": text });
            if omitted > 0 {
                d["omittedChars"] = omitted.into();
            }
            d
        })
        .collect();
    let notes: Vec<Value> = imported
        .notes
        .iter()
        .map(|n| {
            json!({
                "level": n.level.as_str(),
                "origin": n.origin,
                "subject": n.subject,
                "message": n.msg,
                "document": n.document,
                "resource": n.resource,
            })
        })
        .collect();
    tool_result(
        true,
        object([
            ("ok", true.into()),
            ("documents", Value::Array(documents)),
            ("needingAttention", imported.needing_attention().into()),
            ("notes", Value::Array(notes)),
        ]),
    )
}

// --- plan -----------------------------------------------------------------

fn plan(server: &Server, args: &Value) -> Value {
    let Some(command) = server.plan_command() else {
        return tool_error(
            "`plan` is off: it runs the real tool and takes the state lock. Start the server \
             with `meridian mcp --allow-plan` to offer it. `compile` shows what the tool would \
             be given without running it.",
        );
    };
    let Some(file) = arg_str(args, "file") else {
        return tool_error("`file` is required: the document file to plan");
    };
    let path = match server.confined(file) {
        Ok(p) => p,
        Err(e) => return tool_error(e),
    };
    let mut data_files = Vec::new();
    for d in args
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(d) = d.as_str() else {
            return tool_error("`data` is a list of file names");
        };
        match server.confined(d) {
            Ok(p) => data_files.push(p),
            Err(e) => return tool_error(e),
        }
    }
    let mut process = std::process::Command::new(&command.program);
    process
        .arg("plan")
        .args(&command.args)
        .arg("--output")
        .arg("json")
        .env("MERIDIAN_ANNOTATIONS", "off")
        .current_dir(server.root());
    for d in &data_files {
        process.arg("--data").arg(d);
    }
    process.arg(&path);
    let output = match process.output() {
        Ok(o) => o,
        Err(e) => return tool_error(format!("running {} plan: {e}", command.program.display())),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code();
    // `meridian plan --output json` prints one object; it is the result, as
    // it stands, with the report for people beside it.
    let mut body = match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(Value::Object(map)) => map,
        _ => {
            let mut map = Map::new();
            map.insert(
                "error".to_owned(),
                json!({ "message": format!("meridian plan printed no result (exit {code:?})") }),
            );
            map
        }
    };
    let ok = body.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let (report, omitted) = cut(stderr.trim(), FILE_CHARS);
    body.insert("report".to_owned(), report.into());
    if omitted > 0 {
        body.insert("reportOmittedChars".to_owned(), omitted.into());
    }
    body.insert("exitStatus".to_owned(), json!(code));
    tool_result(ok, Value::Object(body))
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

pub(crate) fn prompts() -> Vec<Value> {
    vec![json!({
        "name": "authoring-guide",
        "description": "The loop to follow to write a Meridian document that compiles, in \
                        under a page. Read it first.",
        "arguments": [
            { "name": "target", "description": "The target to write for, if known.", "required": false },
            { "name": "task", "description": "What the document should do, in a sentence.", "required": false },
        ],
    })]
}

pub(crate) fn prompt(server: &Server, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != "authoring-guide" {
        return Err((
            crate::INVALID_PARAMS,
            format!("there is no prompt called `{name}`; the prompt is `authoring-guide`"),
        ));
    }
    let empty = Value::Object(Map::new());
    let args = params.get("arguments").unwrap_or(&empty);
    let target = arg_str(args, "target");
    let task = arg_str(args, "task");
    let targets = crate::target_names(server.catalog());
    let mut text = String::from(
        "You are writing a Meridian document: one YAML file that `meridian` compiles into \
         the native configuration of one tool (Terraform, Ansible, Kubernetes, and others). \
         You have not been trained on this language; do not guess. Look everything up with \
         the tools, and let the compiler tell you what is wrong.\n\n\
         Follow this loop:\n\n\
         1. `list_targets`: pick the target for the tool asked for. Note its `kinds`: an \
         infrastructure-as-code or container-orchestration target takes `kind: Infrastructure`; \
         a configuration-management target takes `kind: ResourceSet`, which needs \
         `spec.targetHost`.\n\
         2. `get_example` with that `target`: read one example that compiles, and copy its \
         shape.\n\
         3. `list_resource_types` for the target, then `describe_resource_type` for each type \
         you will use. Use only the params it lists, with the kinds it gives; fill every \
         `required` param; use a `state` from `states`.\n\
         4. Draft the document. Order resources with `dependsOn`, and use `notifies` only \
         where `describe_resource_type` lists an action for the notified type.\n\
         5. `validate` the draft. For every diagnostic: read `message`, use a value from \
         `accepted` when there is one, apply `fix` when there is one, and call `explain` if \
         the rule is not clear. Correct and validate again until the list is empty.\n\
         6. `compile` the document. It returns the files the tool would be given; read \
         them once to see that they say what was asked. Fix any refusal the same way.\n\
         7. `lint` it, and apply the quick fixes it offers. `format` it last.\n\n\
         Rules: never write a password, a token or a key into a document; declare it under \
         `spec.secrets` and read it as `${{ secret.<name> }}`. Quote a file mode (`\"0644\"`). \
         Do not invent a field, a param or a type: if a tool does not list it, it does not \
         exist. When the task cannot be done on the target, say so and say why, quoting the \
         diagnostic.\n",
    );
    if !targets.is_empty() {
        text.push_str(&format!(
            "\nThe targets loaded here are: {}.\n",
            targets.join(", ")
        ));
    }
    if let Some(target) = target {
        text.push_str(&format!("\nThe target for this task is `{target}`.\n"));
    }
    if let Some(task) = task {
        text.push_str(&format!("\nThe task: {task}\n"));
    }
    Ok(json!({
        "description": "How to write a Meridian document with these tools",
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// The URI a page is served under: `meridian://docs/<path without .md>`.
fn page_uri(path: &str) -> String {
    format!("meridian://docs/{}", path.trim_end_matches(".md"))
}

fn page_title(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("# "))
        .unwrap_or("")
        .trim()
        .to_owned()
}

pub(crate) fn resources() -> Vec<Value> {
    embedded::PAGES
        .iter()
        .map(|(path, text)| {
            json!({
                "uri": page_uri(path),
                "name": path.trim_end_matches(".md").rsplit('/').next().unwrap_or(path),
                "title": page_title(text),
                "mimeType": "text/markdown",
            })
        })
        .collect()
}

pub(crate) fn resource(params: &Value) -> Result<Value, (i64, String)> {
    let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
    let Some((_, text)) = embedded::PAGES
        .iter()
        .find(|(path, _)| page_uri(path) == uri)
    else {
        return Err((
            crate::INVALID_PARAMS,
            format!("there is no resource `{uri}`; `resources/list` gives them"),
        ));
    };
    let (kept, omitted) = cut(text, TEXT_CHARS * 2);
    let mut text = kept;
    if omitted > 0 {
        text.push_str(&format!("\n\n[{omitted} characters left out]\n"));
    }
    Ok(json!({
        "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }],
    }))
}
