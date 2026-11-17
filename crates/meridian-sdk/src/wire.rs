//! The only place the protocol's shapes and this crate's shapes meet.
//!
//! Keeping the conversion in one module means a protocol change has exactly
//! one place to break, and neither an emitter nor a host ever handles a
//! generated type.

use meridian_value::{canonical_map, decode_map, decode_value};

use crate::import::*;
use crate::pb;
use crate::run::*;
use crate::types::*;

fn to_i32(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn from_i32(n: i32) -> usize {
    usize::try_from(n).unwrap_or(0)
}

fn position_to_wire(p: &Position) -> pb::Position {
    pb::Position {
        file: p.file.clone(),
        doc_idx: to_i32(p.doc_idx),
        line: to_i32(p.line),
        column: to_i32(p.column),
    }
}

fn position_from_wire(p: Option<pb::Position>) -> Position {
    p.map(|p| Position {
        file: p.file,
        doc_idx: from_i32(p.doc_idx),
        line: from_i32(p.line),
        column: from_i32(p.column),
    })
    .unwrap_or_default()
}

/// Converts a resource tree into its protocol form.
pub fn graph_to_wire(g: &ResourceGraph) -> pb::ResourceGraph {
    pb::ResourceGraph {
        name: g.name.clone(),
        host: g.host.clone(),
        target: g.target.clone(),
        target_version: g.target_version.clone(),
        resources: g
            .resources
            .iter()
            .map(|r| pb::Resource {
                id: r.id.clone(),
                r#type: r.r#type.clone(),
                state: r.state.clone(),
                params_json: canonical_map(&r.params).into_bytes(),
                runtime_when: r.runtime_when.clone(),
                index: to_i32(r.index),
                pos: Some(position_to_wire(&r.pos)),
            })
            .collect(),
        edges: g
            .edges
            .iter()
            .map(|e| pb::Edge {
                from: e.from.clone(),
                to: e.to.clone(),
                kind: match e.kind {
                    EdgeKind::HardOrder => pb::EdgeKind::HardOrder,
                    EdgeKind::Notify => pb::EdgeKind::Notify,
                } as i32,
                action: e.action.clone(),
                pos: Some(position_to_wire(&e.pos)),
            })
            .collect(),
        outputs: g
            .outputs
            .iter()
            .map(|o| pb::OutputDeclaration {
                name: o.name.clone(),
                value: o.value.clone(),
                pos: Some(position_to_wire(&o.pos)),
            })
            .collect(),
    }
}

/// Converts a protocol tree back into this crate's own types.
///
/// An edge of unspecified kind is refused, not defaulted: guessing between an
/// ordering constraint and a deferred trigger would change what the document
/// means.
pub fn graph_from_wire(g: pb::ResourceGraph) -> Result<ResourceGraph, String> {
    let mut out = ResourceGraph {
        name: g.name,
        host: g.host,
        target: g.target,
        target_version: g.target_version,
        outputs: g
            .outputs
            .into_iter()
            .map(|o| OutputDeclaration {
                name: o.name,
                value: o.value,
                pos: position_from_wire(o.pos),
            })
            .collect(),
        ..ResourceGraph::default()
    };
    for r in g.resources {
        let params = decode_map(&r.params_json).map_err(|e| format!("resource {:?}: {e}", r.id))?;
        out.resources.push(Resource {
            id: r.id,
            r#type: r.r#type,
            state: r.state,
            params,
            runtime_when: r.runtime_when,
            index: from_i32(r.index),
            pos: position_from_wire(r.pos),
        });
    }
    for e in g.edges {
        let kind = match pb::EdgeKind::try_from(e.kind) {
            Ok(pb::EdgeKind::HardOrder) => EdgeKind::HardOrder,
            Ok(pb::EdgeKind::Notify) => EdgeKind::Notify,
            _ => {
                return Err(format!(
                    "edge {} -> {}: edge kind {} is not a kind this protocol version defines",
                    e.from, e.to, e.kind
                ))
            }
        };
        out.edges.push(Edge {
            from: e.from,
            to: e.to,
            kind,
            action: e.action,
            pos: position_from_wire(e.pos),
        });
    }
    Ok(out)
}

pub fn capabilities_to_wire(c: Capabilities) -> pb::Capabilities {
    pb::Capabilities {
        native_notify: c.native_notify,
        synthetic_notify: c.synthetic_notify,
        runtime_condition: c.runtime_condition,
    }
}

pub fn capabilities_from_wire(c: Option<pb::Capabilities>) -> Capabilities {
    c.map(|c| Capabilities {
        native_notify: c.native_notify,
        synthetic_notify: c.synthetic_notify,
        runtime_condition: c.runtime_condition,
    })
    .unwrap_or_default()
}

/// Builds the protocol request carrying a tree and its resolved data.
pub fn emit_request(g: &ResourceGraph, data: &Data) -> pb::EmitRequest {
    pb::EmitRequest {
        graph: Some(graph_to_wire(g)),
        data_json: canonical_map(data).into_bytes(),
    }
}

/// Reads a protocol request back into a tree and its resolved data.
pub fn emit_request_from_wire(req: pb::EmitRequest) -> Result<(ResourceGraph, Data), String> {
    let graph = graph_from_wire(req.graph.ok_or("received no resource graph")?)
        .map_err(|e| format!("reading the resource graph: {e}"))?;
    let data = decode_map(&req.data_json).map_err(|e| format!("reading the resolved data: {e}"))?;
    Ok((graph, data))
}

/// Builds the protocol response for the outcome of a compile.
pub fn emit_response(outcome: Result<Emitted, CompileError>) -> pb::EmitResponse {
    match outcome {
        Ok(emitted) => pb::EmitResponse {
            artifact: Some(pb::Artifact {
                files: emitted.artifact.files.into_iter().collect(),
            }),
            warnings: emitted
                .warnings
                .into_iter()
                .map(|w| pb::Warning {
                    target: w.target,
                    resource: w.resource,
                    msg: w.msg,
                })
                .collect(),
            error: None,
        },
        Err(e) => pb::EmitResponse {
            artifact: None,
            warnings: Vec::new(),
            error: Some(pb::CompileError {
                resource: e.resource,
                rule: e.rule,
                msg: e.msg,
                pos: Some(position_to_wire(&e.pos)),
            }),
        },
    }
}

/// Converts a protocol response back into this crate's values. A refusal
/// carried in the response becomes a [`CompileError`] stamped with the target
/// that raised it.
pub fn emit_response_from_wire(
    resp: pb::EmitResponse,
    target: &str,
) -> Result<Emitted, CompileError> {
    if let Some(e) = resp.error {
        return Err(CompileError {
            target: target.to_owned(),
            resource: e.resource,
            rule: e.rule,
            msg: e.msg,
            pos: position_from_wire(e.pos),
        });
    }
    Ok(Emitted {
        artifact: Artifact {
            files: resp
                .artifact
                .map(|a| a.files.into_iter().collect())
                .unwrap_or_default(),
        },
        warnings: resp
            .warnings
            .into_iter()
            .map(|w| Warning {
                target: w.target,
                resource: w.resource,
                msg: w.msg,
            })
            .collect(),
    })
}

pub fn category_to_wire(c: Category) -> pb::Category {
    match c {
        Category::Unspecified => pb::Category::Unspecified,
        Category::InfrastructureAsCode => pb::Category::InfrastructureAsCode,
        Category::ConfigurationManagement => pb::Category::ConfigurationManagement,
        Category::Containerization => pb::Category::Containerization,
        Category::ContainerOrchestration => pb::Category::ContainerOrchestration,
    }
}

/// A category this protocol version does not define reads as unspecified: a
/// newer plugin stays loadable, and the core simply cannot route on it.
pub fn category_from_wire(c: i32) -> Category {
    match pb::Category::try_from(c) {
        Ok(pb::Category::InfrastructureAsCode) => Category::InfrastructureAsCode,
        Ok(pb::Category::ConfigurationManagement) => Category::ConfigurationManagement,
        Ok(pb::Category::Containerization) => Category::Containerization,
        Ok(pb::Category::ContainerOrchestration) => Category::ContainerOrchestration,
        _ => Category::Unspecified,
    }
}

pub fn operations_to_wire(o: Operations) -> pb::Operations {
    pb::Operations {
        plan: o.plan,
        apply: o.apply,
        outputs: o.outputs,
    }
}

pub fn operations_from_wire(o: Option<pb::Operations>) -> Operations {
    o.map(|o| Operations {
        plan: o.plan,
        apply: o.apply,
        outputs: o.outputs,
    })
    .unwrap_or_default()
}

pub fn artifact_to_wire(a: &Artifact) -> pb::Artifact {
    pb::Artifact {
        files: a.files.clone().into_iter().collect(),
    }
}

pub fn artifact_from_wire(a: Option<pb::Artifact>) -> Artifact {
    Artifact {
        files: a.map(|a| a.files.into_iter().collect()).unwrap_or_default(),
    }
}

pub fn context_to_wire(c: &RunContext) -> pb::RunContext {
    pb::RunContext {
        document: c.document.clone(),
        host: c.host.clone(),
        work_dir: c.work_dir.to_string_lossy().into_owned(),
        inputs_json: canonical_map(&c.inputs).into_bytes(),
    }
}

pub fn context_from_wire(c: Option<pb::RunContext>) -> Result<RunContext, String> {
    let c = c.ok_or("received no run context")?;
    Ok(RunContext {
        document: c.document,
        host: c.host,
        work_dir: c.work_dir.into(),
        inputs: decode_map(&c.inputs_json).map_err(|e| format!("reading the inputs: {e}"))?,
    })
}

fn change_to_wire(c: Change) -> pb::Change {
    let action = match c.action {
        ChangeAction::Create => pb::ChangeAction::Create,
        ChangeAction::Update => pb::ChangeAction::Update,
        ChangeAction::Delete => pb::ChangeAction::Delete,
        ChangeAction::Replace => pb::ChangeAction::Replace,
    };
    pb::Change {
        resource: c.resource,
        action: action as i32,
        summary: c.summary,
    }
}

fn changes_from_wire(changes: Vec<pb::Change>) -> Result<Vec<Change>, String> {
    changes
        .into_iter()
        .map(|c| {
            let action = match pb::ChangeAction::try_from(c.action) {
                Ok(pb::ChangeAction::Create) => ChangeAction::Create,
                Ok(pb::ChangeAction::Update) => ChangeAction::Update,
                Ok(pb::ChangeAction::Delete) => ChangeAction::Delete,
                Ok(pb::ChangeAction::Replace) => ChangeAction::Replace,
                // Guessing here could turn a delete into something milder.
                _ => {
                    return Err(format!(
                        "change to {:?}: action {} is not one this protocol version defines",
                        c.resource, c.action
                    ))
                }
            };
            Ok(Change {
                resource: c.resource,
                action,
                summary: c.summary,
            })
        })
        .collect()
}

fn outputs_to_wire(outputs: Vec<Output>) -> Vec<pb::Output> {
    outputs
        .into_iter()
        .map(|o| pb::Output {
            name: o.name,
            value_json: o.value.canonical().into_bytes(),
            sensitive: o.sensitive,
        })
        .collect()
}

fn outputs_from_wire(outputs: Vec<pb::Output>) -> Result<Vec<Output>, String> {
    outputs
        .into_iter()
        .map(|o| {
            let value = decode_value(&o.value_json)
                // The value is left out of the message: it may be a secret.
                .map_err(|_| format!("output {:?} is not valid JSON", o.name))?;
            Ok(Output {
                name: o.name,
                value,
                sensitive: o.sensitive,
            })
        })
        .collect()
}

fn run_error_to_wire(e: RunError) -> pb::RunError {
    pb::RunError {
        msg: e.msg,
        command: e.command,
        exit_code: e.exit_code.unwrap_or_default(),
        exited: e.exit_code.is_some(),
        output: e.output,
    }
}

fn run_error_from_wire(e: pb::RunError, changes: Vec<Change>) -> RunError {
    RunError {
        msg: e.msg,
        command: e.command,
        exit_code: e.exited.then_some(e.exit_code),
        output: e.output,
        changes,
    }
}

pub fn log_to_wire(line: LogLine) -> pb::LogLine {
    let stream = match line.stream {
        LogStream::Stdout => pb::log_line::Stream::Stdout,
        LogStream::Stderr => pb::log_line::Stream::Stderr,
    };
    pb::LogLine {
        stream: stream as i32,
        text: line.text,
    }
}

pub fn log_from_wire(line: pb::LogLine) -> LogLine {
    let stream = match pb::log_line::Stream::try_from(line.stream) {
        Ok(pb::log_line::Stream::Stderr) => LogStream::Stderr,
        _ => LogStream::Stdout,
    };
    LogLine {
        stream,
        text: line.text,
    }
}

pub fn plan_result(outcome: Result<Plan, RunError>) -> pb::PlanResult {
    match outcome {
        Ok(plan) => pb::PlanResult {
            changes: plan.changes.into_iter().map(change_to_wire).collect(),
            unknown: plan.unknown,
            error: None,
        },
        Err(e) => pb::PlanResult {
            changes: Vec::new(),
            unknown: false,
            error: Some(run_error_to_wire(e)),
        },
    }
}

/// The outer error is a malformed message; the inner one is the tool failing.
pub fn plan_result_from_wire(r: pb::PlanResult) -> Result<Result<Plan, RunError>, String> {
    if let Some(e) = r.error {
        return Ok(Err(run_error_from_wire(e, Vec::new())));
    }
    Ok(Ok(Plan {
        changes: changes_from_wire(r.changes)?,
        unknown: r.unknown,
    }))
}

pub fn apply_result(outcome: Result<Applied, RunError>) -> pb::ApplyResult {
    match outcome {
        Ok(applied) => pb::ApplyResult {
            changes: applied.changes.into_iter().map(change_to_wire).collect(),
            outputs: outputs_to_wire(applied.outputs),
            error: None,
        },
        Err(mut e) => pb::ApplyResult {
            changes: std::mem::take(&mut e.changes)
                .into_iter()
                .map(change_to_wire)
                .collect(),
            outputs: Vec::new(),
            error: Some(run_error_to_wire(e)),
        },
    }
}

/// The outer error is a malformed message; the inner one is the tool failing.
pub fn apply_result_from_wire(r: pb::ApplyResult) -> Result<Result<Applied, RunError>, String> {
    let changes = changes_from_wire(r.changes)?;
    if let Some(e) = r.error {
        return Ok(Err(run_error_from_wire(e, changes)));
    }
    Ok(Ok(Applied {
        changes,
        outputs: outputs_from_wire(r.outputs)?,
    }))
}

pub fn outputs_response(outcome: Result<Vec<Output>, RunError>) -> pb::OutputsResponse {
    match outcome {
        Ok(outputs) => pb::OutputsResponse {
            outputs: outputs_to_wire(outputs),
            error: None,
        },
        Err(e) => pb::OutputsResponse {
            outputs: Vec::new(),
            error: Some(run_error_to_wire(e)),
        },
    }
}

/// The outer error is a malformed message; the inner one is the tool failing.
pub fn outputs_response_from_wire(
    r: pb::OutputsResponse,
) -> Result<Result<Vec<Output>, RunError>, String> {
    if let Some(e) = r.error {
        return Ok(Err(run_error_from_wire(e, Vec::new())));
    }
    Ok(Ok(outputs_from_wire(r.outputs)?))
}

pub fn import_formats_to_wire(formats: Vec<ImportFormat>) -> Vec<pb::ImportFormat> {
    formats
        .into_iter()
        .map(|f| pb::ImportFormat {
            name: f.name,
            description: f.description,
            patterns: f.patterns,
        })
        .collect()
}

pub fn import_formats_from_wire(formats: Vec<pb::ImportFormat>) -> Vec<ImportFormat> {
    formats
        .into_iter()
        .map(|f| ImportFormat {
            name: f.name,
            description: f.description,
            patterns: f.patterns,
        })
        .collect()
}

pub fn import_request(source: &ImportSource) -> pb::ImportRequest {
    pb::ImportRequest {
        files: source.files.clone().into_iter().collect(),
        name: source.name.clone(),
        root: source.root.clone(),
    }
}

pub fn import_request_from_wire(request: pb::ImportRequest) -> ImportSource {
    ImportSource {
        files: request.files.into_iter().collect(),
        name: request.name,
        root: request.root,
    }
}

/// Builds the protocol response for the outcome of an import.
pub fn import_response(outcome: Result<Imported, ImportError>) -> pb::ImportResponse {
    let imported = match outcome {
        Ok(imported) => imported,
        Err(e) => {
            return pb::ImportResponse {
                documents: Vec::new(),
                notes: Vec::new(),
                error: Some(pb::ImportError {
                    msg: e.msg,
                    file: e.file,
                    line: to_i32(e.line),
                }),
            }
        }
    };
    pb::ImportResponse {
        documents: imported
            .documents
            .into_iter()
            .map(|d| pb::ImportedDocument {
                kind: match d.kind {
                    DocumentKind::Infrastructure => pb::DocumentKind::Infrastructure,
                    DocumentKind::ResourceSet => pb::DocumentKind::ResourceSet,
                } as i32,
                name: d.name,
                target: d.target,
                target_version: d.target_version,
                host: d.host,
                resources: d
                    .resources
                    .into_iter()
                    .map(|r| pb::ImportedResource {
                        id: r.id,
                        r#type: r.r#type,
                        state: r.state,
                        params_json: canonical_map(&r.params).into_bytes(),
                        depends_on: r.depends_on,
                        notifies: r
                            .notifies
                            .into_iter()
                            .map(|n| pb::ImportedNotify {
                                resource: n.resource,
                                action: n.action,
                            })
                            .collect(),
                        runtime_when: r.runtime_when,
                        origin: r.origin,
                    })
                    .collect(),
                outputs: d
                    .outputs
                    .into_iter()
                    .map(|o| pb::ImportedOutput {
                        name: o.name,
                        value: o.value,
                    })
                    .collect(),
            })
            .collect(),
        notes: imported
            .notes
            .into_iter()
            .map(|n| pb::ImportNote {
                level: match n.level {
                    NoteLevel::Info => pb::NoteLevel::Info,
                    NoteLevel::Changed => pb::NoteLevel::Changed,
                    NoteLevel::Untranslated => pb::NoteLevel::Untranslated,
                } as i32,
                origin: n.origin,
                subject: n.subject,
                msg: n.msg,
                document: n.document,
                resource: n.resource,
            })
            .collect(),
        error: None,
    }
}

/// The outer error is a malformed message; the inner one is the importer
/// refusing the source.
///
/// A document of unspecified kind is refused, not defaulted: the two kinds
/// run at different points of a workflow. A note of a level this protocol
/// version does not define is read as the most serious one, so a newer
/// plugin's warning is never shown as less than it is.
pub fn import_response_from_wire(
    response: pb::ImportResponse,
) -> Result<Result<Imported, ImportError>, String> {
    if let Some(e) = response.error {
        return Ok(Err(ImportError {
            msg: e.msg,
            file: e.file,
            line: from_i32(e.line),
        }));
    }
    let mut imported = Imported::default();
    for d in response.documents {
        let kind = match pb::DocumentKind::try_from(d.kind) {
            Ok(pb::DocumentKind::Infrastructure) => DocumentKind::Infrastructure,
            Ok(pb::DocumentKind::ResourceSet) => DocumentKind::ResourceSet,
            _ => {
                return Err(format!(
                    "document {:?}: kind {} is not one this protocol version defines",
                    d.name, d.kind
                ))
            }
        };
        let mut resources = Vec::with_capacity(d.resources.len());
        for r in d.resources {
            let params = decode_map(&r.params_json)
                .map_err(|e| format!("document {:?}: resource {:?}: {e}", d.name, r.id))?;
            resources.push(ImportedResource {
                id: r.id,
                r#type: r.r#type,
                state: r.state,
                params,
                depends_on: r.depends_on,
                notifies: r
                    .notifies
                    .into_iter()
                    .map(|n| ImportedNotify {
                        resource: n.resource,
                        action: n.action,
                    })
                    .collect(),
                runtime_when: r.runtime_when,
                origin: r.origin,
            });
        }
        imported.documents.push(ImportedDocument {
            kind,
            name: d.name,
            target: d.target,
            target_version: d.target_version,
            host: d.host,
            resources,
            outputs: d
                .outputs
                .into_iter()
                .map(|o| ImportedOutput {
                    name: o.name,
                    value: o.value,
                })
                .collect(),
        });
    }
    for n in response.notes {
        imported.notes.push(ImportNote {
            level: match pb::NoteLevel::try_from(n.level) {
                Ok(pb::NoteLevel::Info) => NoteLevel::Info,
                Ok(pb::NoteLevel::Changed) => NoteLevel::Changed,
                _ => NoteLevel::Untranslated,
            },
            origin: n.origin,
            subject: n.subject,
            msg: n.msg,
            document: n.document,
            resource: n.resource,
        });
    }
    Ok(Ok(imported))
}
