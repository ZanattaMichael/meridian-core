//! The only place the protocol's shapes and this crate's shapes meet.
//!
//! Keeping the conversion in one module means a protocol change has exactly
//! one place to break, and neither an emitter nor a host ever handles a
//! generated type.

use meridian_secret::SecretValue;
use std::collections::BTreeMap;

use meridian_value::{
    canonical_map, decode_map, decode_value, PlacedEscape, SecretRef, Value, SENSITIVE,
};

use crate::credential::{Credential, HeldCredential};
use crate::import::*;
use crate::pb;
use crate::rules::{DescribedRule, Rule};
use crate::run::*;
use crate::schema::{ParamKind, ParamSchema, ResourceSchema};
use crate::schemas::{SchemaExport, SchemaRequest};
use crate::secrets::{SecretName, Secrets};
use crate::settings::Setting;
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
        settings_json: if g.settings.is_empty() {
            Vec::new()
        } else {
            canonical_map(&g.settings).into_bytes()
        },
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
                secret_params: secret_params_of(&r.params),
                adopt: r.adopt.clone(),
            })
            .collect(),
        secrets: g
            .secrets
            .iter()
            .map(|s| pb::SecretName {
                slot: slot_to_wire(s.secret),
                name: s.name.clone(),
            })
            .collect(),
        credential: g.credential.as_ref().map(credential_to_wire),
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
    out.secrets = g
        .secrets
        .into_iter()
        .map(|s| SecretName {
            secret: SecretRef::new(u64::from(s.slot)),
            name: s.name,
        })
        .collect();
    out.credential = g.credential.map(credential_from_wire);
    if !g.settings_json.is_empty() {
        out.settings = decode_map(&g.settings_json).map_err(|e| format!("settings: {e}"))?;
    }
    for r in g.resources {
        let mut params =
            decode_map(&r.params_json).map_err(|e| format!("resource {:?}: {e}", r.id))?;
        for at in &r.secret_params {
            place_secret(&mut params, &at.path, SecretRef::new(u64::from(at.slot)))
                .map_err(|e| format!("resource {:?}: {e}", r.id))?;
        }
        out.resources.push(Resource {
            id: r.id,
            r#type: r.r#type,
            state: r.state,
            params,
            runtime_when: r.runtime_when,
            adopt: r.adopt,
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

/// A slot on the wire. A slot is an index into one run's table, so one too
/// big for the wire's integer would mean a run with billions of secrets.
fn slot_to_wire(secret: SecretRef) -> u32 {
    u32::try_from(secret.slot()).expect("a run has fewer secrets than u32::MAX")
}

/// Where in `params` a secret stands: the path of keys (and list indexes,
/// as decimal text) to each `Value::Secret`, with its slot. `canonical_map`
/// writes the mask in each of those places.
fn secret_params_of(params: &std::collections::BTreeMap<String, Value>) -> Vec<pb::SecretParam> {
    fn walk(value: &Value, path: &mut Vec<String>, found: &mut Vec<pb::SecretParam>) {
        match value {
            Value::Secret(at) => found.push(pb::SecretParam {
                path: path.clone(),
                slot: slot_to_wire(*at),
            }),
            Value::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    path.push(i.to_string());
                    walk(item, path, found);
                    path.pop();
                }
            }
            Value::Map(entries) => {
                for (key, item) in entries {
                    path.push(key.clone());
                    walk(item, path, found);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    let mut path = Vec::new();
    for (key, value) in params {
        path.push(key.clone());
        walk(value, &mut path, &mut found);
        path.pop();
    }
    found
}

/// Puts a `Value::Secret` back where the sender said one stands. The place
/// must exist and hold the mask, or the message does not describe its own
/// params and is refused.
fn place_secret(
    params: &mut std::collections::BTreeMap<String, Value>,
    path: &[String],
    secret: SecretRef,
) -> Result<(), String> {
    let shown = || path.join(".");
    let (first, rest) = path
        .split_first()
        .ok_or_else(|| "a secret param has an empty path".to_owned())?;
    let mut at = params
        .get_mut(first)
        .ok_or_else(|| format!("secret param {}: no such param", shown()))?;
    for step in rest {
        at = match at {
            Value::List(items) => step
                .parse::<usize>()
                .ok()
                .and_then(|i| items.get_mut(i))
                .ok_or_else(|| format!("secret param {}: no such list item", shown()))?,
            Value::Map(entries) => entries
                .get_mut(step)
                .ok_or_else(|| format!("secret param {}: no such key", shown()))?,
            _ => {
                return Err(format!(
                    "secret param {}: not inside a list or map",
                    shown()
                ))
            }
        };
    }
    match at {
        Value::String(s) if s == SENSITIVE => {
            *at = Value::Secret(secret);
            Ok(())
        }
        _ => Err(format!(
            "secret param {}: the params do not hold a secret's mask there",
            shown()
        )),
    }
}

pub fn capabilities_to_wire(c: Capabilities) -> pb::Capabilities {
    pb::Capabilities {
        native_notify: c.native_notify,
        synthetic_notify: c.synthetic_notify,
        runtime_condition: c.runtime_condition,
        secrets: c.secrets,
        credentials: c.credentials,
        adopt: adoption_to_wire(c.adopt) as i32,
    }
}

fn adoption_to_wire(a: Adoption) -> pb::Adoption {
    match a {
        Adoption::None => pb::Adoption::None,
        Adoption::Import => pb::Adoption::Import,
        Adoption::Converge => pb::Adoption::Converge,
    }
}

/// A value this protocol version does not define is read as
/// [`Adoption::None`], so the host refuses rather than guesses.
fn adoption_from_wire(a: i32) -> Adoption {
    match pb::Adoption::try_from(a) {
        Ok(pb::Adoption::Import) => Adoption::Import,
        Ok(pb::Adoption::Converge) => Adoption::Converge,
        _ => Adoption::None,
    }
}

/// Converts the credential a compile sees: no value is in it.
pub fn credential_to_wire(c: &Credential) -> pb::Credential {
    pb::Credential {
        name: c.name.clone(),
        r#type: c.kind.clone(),
        ambient: c.ambient,
        text: c.text.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        flags: c.flags.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        secret_fields: c.secret_fields.clone(),
    }
}

pub fn credential_from_wire(c: pb::Credential) -> Credential {
    let mut secret_fields = c.secret_fields;
    secret_fields.sort_unstable();
    Credential {
        name: c.name,
        kind: c.r#type,
        ambient: c.ambient,
        text: c.text.into_iter().collect(),
        flags: c.flags.into_iter().collect(),
        secret_fields,
    }
}

pub fn placed_text_to_wire(escapes: &[PlacedEscape]) -> Vec<pb::PlacedEscape> {
    escapes
        .iter()
        .map(|e| pb::PlacedEscape {
            text: e.text.clone(),
            written: e.written.clone(),
            only_at_start: e.only_at_start,
        })
        .collect()
}

pub fn placed_text_from_wire(escapes: Vec<pb::PlacedEscape>) -> Vec<PlacedEscape> {
    escapes
        .into_iter()
        .map(|e| PlacedEscape {
            text: e.text,
            written: e.written,
            only_at_start: e.only_at_start,
        })
        .collect()
}

pub fn capabilities_from_wire(c: Option<pb::Capabilities>) -> Capabilities {
    c.map(|c| Capabilities {
        native_notify: c.native_notify,
        synthetic_notify: c.synthetic_notify,
        runtime_condition: c.runtime_condition,
        secrets: c.secrets,
        credentials: c.credentials,
        adopt: adoption_from_wire(c.adopt),
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
                hint: e.hint,
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
            hint: e.hint,
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

pub fn resource_schema_to_wire(s: &ResourceSchema) -> pb::ResourceSchema {
    pb::ResourceSchema {
        r#type: s.r#type.clone(),
        description: s.description.clone(),
        states: s.states.clone(),
        default_state: s.default_state.clone(),
        params: s.params.iter().map(param_schema_to_wire).collect(),
        open: s.open,
        actions: s.actions.clone(),
        source: s.source.clone(),
    }
}

fn param_schema_to_wire(p: &ParamSchema) -> pb::ParamSchema {
    pb::ParamSchema {
        name: p.name.clone(),
        description: p.description.clone(),
        // Any travels as the empty string, the proto3 default.
        kind: match p.kind {
            ParamKind::Any => String::new(),
            kind => kind.as_str().to_owned(),
        },
        required: p.required,
        values: p.values.clone(),
        params: p.params.iter().map(param_schema_to_wire).collect(),
        open: p.open,
        computed: p.computed,
        deprecated: p.deprecated,
        sensitive: p.sensitive,
    }
}

pub fn resource_schema_from_wire(s: pb::ResourceSchema) -> ResourceSchema {
    ResourceSchema {
        r#type: s.r#type,
        description: s.description,
        states: s.states,
        default_state: s.default_state,
        params: s.params.into_iter().map(param_schema_from_wire).collect(),
        open: s.open,
        actions: s.actions,
        source: s.source,
    }
}

fn param_schema_from_wire(p: pb::ParamSchema) -> ParamSchema {
    ParamSchema {
        name: p.name,
        description: p.description,
        kind: ParamKind::from_name(&p.kind),
        required: p.required,
        values: p.values,
        params: p.params.into_iter().map(param_schema_from_wire).collect(),
        open: p.open,
        computed: p.computed,
        deprecated: p.deprecated,
        sensitive: p.sensitive,
    }
}

pub fn export_schemas_request(r: &SchemaRequest) -> pb::ExportSchemasRequest {
    pb::ExportSchemasRequest {
        work_dir: r.work_dir.to_string_lossy().into_owned(),
        graphs: r.graphs.iter().map(graph_to_wire).collect(),
        types: r.types.clone(),
    }
}

pub fn export_schemas_request_from_wire(
    r: pb::ExportSchemasRequest,
) -> Result<SchemaRequest, String> {
    let graphs = r
        .graphs
        .into_iter()
        .map(graph_from_wire)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading a resource graph: {e}"))?;
    Ok(SchemaRequest {
        work_dir: r.work_dir.into(),
        graphs,
        types: r.types,
    })
}

pub fn export_schemas_response(
    outcome: Result<SchemaExport, RunError>,
) -> pb::ExportSchemasResponse {
    match outcome {
        Ok(export) => pb::ExportSchemasResponse {
            schemas: export.schemas.iter().map(resource_schema_to_wire).collect(),
            tool: export.tool,
            tool_version: export.tool_version,
            pins: export.pins.into_iter().collect(),
            notes: export.notes,
            error: None,
        },
        Err(e) => pb::ExportSchemasResponse {
            error: Some(run_error_to_wire(e)),
            ..Default::default()
        },
    }
}

pub fn export_schemas_response_from_wire(
    r: pb::ExportSchemasResponse,
) -> Result<SchemaExport, RunError> {
    if let Some(e) = r.error {
        return Err(run_error_from_wire(e, Vec::new()));
    }
    Ok(SchemaExport {
        schemas: r
            .schemas
            .into_iter()
            .map(resource_schema_from_wire)
            .collect(),
        tool: r.tool,
        tool_version: r.tool_version,
        pins: r.pins.into_iter().collect(),
        notes: r.notes,
    })
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

/// Converts a run context, with its secrets. The bytes of each secret are
/// copied into the message, which is sent over the plugin's private socket
/// and dropped: the one place outside protected memory a secret passes
/// through on its way to a plugin.
pub fn context_to_wire(c: &RunContext) -> pb::RunContext {
    pb::RunContext {
        document: c.document.clone(),
        host: c.host.clone(),
        work_dir: c.work_dir.to_string_lossy().into_owned(),
        inputs_json: canonical_map(&c.inputs).into_bytes(),
        secrets: c
            .secrets
            .slots()
            .map(|(secret, name, value)| pb::Secret {
                slot: slot_to_wire(secret),
                name: name.to_owned(),
                value: value.expose(<[u8]>::to_vec),
            })
            .collect(),
        credential: c.credential.as_ref().map(|held| pb::CredentialValues {
            name: held.declared.name.clone(),
            r#type: held.declared.kind.clone(),
            ambient: held.declared.ambient,
            text: held
                .declared
                .text
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            flags: held
                .declared
                .flags
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            secrets: held
                .secrets()
                .map(|(field, value)| pb::CredentialSecret {
                    field: field.to_owned(),
                    value: value.expose(<[u8]>::to_vec),
                })
                .collect(),
        }),
        save_plan: c.save_plan,
        saved_plan: c.saved_plan.as_ref().map(saved_plan_to_wire),
    }
}

pub fn saved_plan_to_wire(saved: &SavedPlan) -> pb::SavedPlan {
    pb::SavedPlan {
        data: saved.data.clone(),
        changes: saved.changes.iter().cloned().map(change_to_wire).collect(),
    }
}

pub fn saved_plan_from_wire(saved: pb::SavedPlan) -> Result<SavedPlan, String> {
    Ok(SavedPlan {
        data: saved.data,
        changes: changes_from_wire(saved.changes)?,
    })
}

/// Converts a run context without its secrets, for a call that needs none:
/// neither the secrets nor the credential's values cross.
pub fn context_to_wire_without_secrets(c: &RunContext) -> pb::RunContext {
    pb::RunContext {
        document: c.document.clone(),
        host: c.host.clone(),
        work_dir: c.work_dir.to_string_lossy().into_owned(),
        inputs_json: canonical_map(&c.inputs).into_bytes(),
        secrets: Vec::new(),
        credential: None,
        save_plan: c.save_plan,
        saved_plan: c.saved_plan.as_ref().map(saved_plan_to_wire),
    }
}

/// Converts a run context back. Each secret is taken into protected memory
/// and its bytes in the message are wiped on the way.
pub fn context_from_wire(c: Option<pb::RunContext>) -> Result<RunContext, String> {
    let mut c = c.ok_or("received no run context")?;
    let mut secrets = Vec::with_capacity(c.secrets.len());
    for secret in &mut c.secrets {
        let value = SecretValue::take_from(&mut secret.value)
            .map_err(|e| format!("holding the secret {:?}: {e}", secret.name))?;
        secrets.push((
            SecretRef::new(u64::from(secret.slot)),
            std::mem::take(&mut secret.name),
            value,
        ));
    }
    let credential = match c.credential.take() {
        None => None,
        Some(mut held) => {
            let mut values = Vec::with_capacity(held.secrets.len());
            for secret in &mut held.secrets {
                let value = SecretValue::take_from(&mut secret.value).map_err(|e| {
                    format!(
                        "holding the credential {:?}'s {}: {e}",
                        held.name, secret.field
                    )
                })?;
                values.push((std::mem::take(&mut secret.field), value));
            }
            let declared = Credential {
                name: held.name,
                kind: held.r#type,
                ambient: held.ambient,
                text: held.text.into_iter().collect(),
                flags: held.flags.into_iter().collect(),
                secret_fields: Vec::new(),
            };
            Some(HeldCredential::from_values(declared, values))
        }
    };
    Ok(RunContext {
        document: c.document,
        host: c.host,
        work_dir: c.work_dir.into(),
        inputs: decode_map(&c.inputs_json).map_err(|e| format!("reading the inputs: {e}"))?,
        secrets: Secrets::from_values(secrets),
        credential,
        save_plan: c.save_plan,
        saved_plan: match c.saved_plan {
            None => None,
            Some(saved) => Some(saved_plan_from_wire(saved)?),
        },
    })
}

fn change_to_wire(c: Change) -> pb::Change {
    let action = match c.action {
        ChangeAction::Create => pb::ChangeAction::Create,
        ChangeAction::Update => pb::ChangeAction::Update,
        ChangeAction::Delete => pb::ChangeAction::Delete,
        ChangeAction::Replace => pb::ChangeAction::Replace,
        ChangeAction::Adopt => pb::ChangeAction::Adopt,
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
                Ok(pb::ChangeAction::Adopt) => ChangeAction::Adopt,
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
            saved: plan.saved.as_ref().map(saved_plan_to_wire),
        },
        Err(e) => pb::PlanResult {
            changes: Vec::new(),
            unknown: false,
            error: Some(run_error_to_wire(e)),
            saved: None,
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
        saved: match r.saved {
            None => None,
            Some(saved) => Some(saved_plan_from_wire(saved)?),
        },
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

pub fn settings_to_wire(settings: Vec<Setting>) -> Vec<pb::Setting> {
    settings
        .into_iter()
        .map(|s| pb::Setting {
            name: s.name,
            variable: s.variable,
            default_value: s.default,
            description: s.description,
        })
        .collect()
}

pub fn settings_from_wire(settings: Vec<pb::Setting>) -> Vec<Setting> {
    settings
        .into_iter()
        .map(|s| Setting {
            name: s.name,
            variable: s.variable,
            default: s.default_value,
            description: s.description,
        })
        .collect()
}

pub fn rules_to_wire(rules: &[Rule]) -> Vec<pb::Rule> {
    rules
        .iter()
        .map(|r| pb::Rule {
            name: r.name.to_owned(),
            meaning: r.meaning.to_owned(),
            hint: r.hint.to_owned(),
        })
        .collect()
}

pub fn rules_from_wire(rules: Vec<pb::Rule>) -> Vec<DescribedRule> {
    rules
        .into_iter()
        .map(|r| DescribedRule {
            name: r.name,
            meaning: r.meaning,
            hint: r.hint,
        })
        .collect()
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
                settings_json: if d.settings.is_empty() {
                    Vec::new()
                } else {
                    canonical_map(&d.settings).into_bytes()
                },
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
        let settings = if d.settings_json.is_empty() {
            BTreeMap::new()
        } else {
            decode_map(&d.settings_json)
                .map_err(|e| format!("document {:?}: settings: {e}", d.name))?
        };
        imported.documents.push(ImportedDocument {
            kind,
            name: d.name,
            target: d.target,
            target_version: d.target_version,
            host: d.host,
            settings,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn secret(text: &str) -> SecretValue {
        SecretValue::from_reader(text.as_bytes(), 64).unwrap()
    }

    #[test]
    fn a_secret_in_a_param_crosses_the_wire_as_a_place_and_a_slot_and_never_a_value() {
        let graph = ResourceGraph {
            resources: vec![Resource {
                id: "db".to_owned(),
                params: BTreeMap::from([
                    ("password".to_owned(), Value::Secret(SecretRef::new(3))),
                    (
                        "users".to_owned(),
                        Value::List(vec![Value::Map(BTreeMap::from([(
                            "token".to_owned(),
                            Value::Secret(SecretRef::new(5)),
                        )]))]),
                    ),
                ]),
                ..Resource::default()
            }],
            secrets: vec![
                SecretName {
                    secret: SecretRef::new(3),
                    name: "db_password".to_owned(),
                },
                SecretName {
                    secret: SecretRef::new(5),
                    name: "api_token".to_owned(),
                },
            ],
            ..ResourceGraph::default()
        };
        let wire = graph_to_wire(&graph);
        let resource = &wire.resources[0];
        assert_eq!(
            String::from_utf8(resource.params_json.clone()).unwrap(),
            "{\"password\":\"(sensitive)\",\"users\":[{\"token\":\"(sensitive)\"}]}"
        );
        let places: Vec<(Vec<String>, u32)> = resource
            .secret_params
            .iter()
            .map(|p| (p.path.clone(), p.slot))
            .collect();
        assert_eq!(
            places,
            [
                (vec!["password".to_owned()], 3),
                (
                    vec!["users".to_owned(), "0".to_owned(), "token".to_owned()],
                    5
                )
            ]
        );
        assert_eq!(graph_from_wire(wire).unwrap(), graph);
    }

    #[test]
    fn a_secret_param_that_does_not_point_at_the_mask_is_refused() {
        let mut wire = graph_to_wire(&ResourceGraph {
            resources: vec![Resource {
                id: "db".to_owned(),
                params: BTreeMap::from([("password".to_owned(), Value::from("plain"))]),
                ..Resource::default()
            }],
            ..ResourceGraph::default()
        });
        wire.resources[0].secret_params.push(pb::SecretParam {
            path: vec!["password".to_owned()],
            slot: 1,
        });
        let err = graph_from_wire(wire.clone()).unwrap_err();
        assert_eq!(
            err,
            "resource \"db\": secret param password: the params do not hold a secret's mask there"
        );
        wire.resources[0].secret_params[0].path = vec!["nothing".to_owned()];
        assert_eq!(
            graph_from_wire(wire).unwrap_err(),
            "resource \"db\": secret param nothing: no such param"
        );
    }

    #[test]
    fn a_run_context_carries_its_secrets_into_protected_memory_and_outputs_carry_none() {
        let context = RunContext {
            document: "web".to_owned(),
            secrets: Secrets::from_values(vec![(
                SecretRef::new(3),
                "db_password".to_owned(),
                secret("hunter2"),
            )]),
            ..RunContext::default()
        };
        let wire = context_to_wire(&context);
        assert_eq!(wire.secrets.len(), 1);
        assert_eq!(wire.secrets[0].name, "db_password");
        assert_eq!(wire.secrets[0].slot, 3);
        assert_eq!(wire.secrets[0].value, b"hunter2");
        let back = context_from_wire(Some(wire)).unwrap();
        assert_eq!(back, context);
        assert!(back
            .secrets
            .of(SecretRef::new(3))
            .unwrap()
            .expose(|b| b == b"hunter2"));
        assert!(context_to_wire_without_secrets(&context).secrets.is_empty());
    }

    #[test]
    fn capabilities_keep_their_secrets_flag_across_the_wire() {
        let all = Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: true,
            secrets: true,
            credentials: false,
            adopt: Adoption::Import,
        };
        assert_eq!(capabilities_from_wire(Some(capabilities_to_wire(all))), all);
        assert!(!capabilities_from_wire(None).secrets);
        let with_credentials = Capabilities {
            credentials: true,
            ..all
        };
        assert_eq!(
            capabilities_from_wire(Some(capabilities_to_wire(with_credentials))),
            with_credentials
        );
        assert!(!capabilities_from_wire(None).credentials);
    }

    #[test]
    fn adoption_crosses_the_wire_and_an_unknown_value_is_none() {
        for adopt in [Adoption::None, Adoption::Import, Adoption::Converge] {
            let c = Capabilities {
                adopt,
                ..Capabilities::default()
            };
            assert_eq!(capabilities_from_wire(Some(capabilities_to_wire(c))), c);
        }
        assert_eq!(capabilities_from_wire(None).adopt, Adoption::None);
        let unknown = pb::Capabilities {
            adopt: 99,
            ..pb::Capabilities::default()
        };
        assert_eq!(capabilities_from_wire(Some(unknown)).adopt, Adoption::None);
    }

    #[test]
    fn a_resource_keeps_its_adopt_across_the_wire_and_an_adopt_change_too() {
        let graph = ResourceGraph {
            name: "web".to_owned(),
            resources: vec![Resource {
                id: "vm".to_owned(),
                r#type: "aws_instance".to_owned(),
                adopt: "i-0abc123".to_owned(),
                ..Resource::default()
            }],
            ..ResourceGraph::default()
        };
        let back = graph_from_wire(graph_to_wire(&graph)).unwrap();
        assert_eq!(back.resources[0].adopt, "i-0abc123");
        let change = Change {
            resource: "vm".to_owned(),
            action: ChangeAction::Adopt,
            summary: String::new(),
        };
        let back = changes_from_wire(vec![change_to_wire(change.clone())]).unwrap();
        assert_eq!(back, [change]);
    }

    #[test]
    fn settings_cross_the_wire_with_their_value_types() {
        let graph = ResourceGraph {
            name: "web".to_owned(),
            settings: BTreeMap::from([
                ("engine".to_owned(), Value::from("DscV2")),
                ("depth".to_owned(), Value::Int(3)),
            ]),
            ..ResourceGraph::default()
        };
        let wire = graph_to_wire(&graph);
        assert_eq!(wire.settings_json, br#"{"depth":3,"engine":"DscV2"}"#);
        assert_eq!(graph_from_wire(wire).unwrap().settings, graph.settings);
        // None set is no bytes at all, as a plugin before settings sent.
        let wire = graph_to_wire(&ResourceGraph::default());
        assert!(wire.settings_json.is_empty());
        assert!(graph_from_wire(wire).unwrap().settings.is_empty());
    }

    fn declared_credential() -> Credential {
        Credential {
            name: "deploy".to_owned(),
            kind: "ssh".to_owned(),
            ambient: false,
            text: BTreeMap::from([("user".to_owned(), "deploy".to_owned())]),
            flags: BTreeMap::from([("agent".to_owned(), false)]),
            secret_fields: vec!["privateKey".to_owned()],
        }
    }

    #[test]
    fn a_graph_carries_its_credential_by_name_and_field_names_only() {
        let graph = ResourceGraph {
            name: "web".to_owned(),
            credential: Some(declared_credential()),
            ..ResourceGraph::default()
        };
        let wire = graph_to_wire(&graph);
        let credential = wire.credential.clone().unwrap();
        assert_eq!(credential.name, "deploy");
        assert_eq!(credential.r#type, "ssh");
        assert_eq!(credential.secret_fields, ["privateKey"]);
        assert_eq!(credential.text["user"], "deploy");
        // There is no field for a value: the message type cannot carry one.
        assert_eq!(graph_from_wire(wire).unwrap(), graph);
        assert!(graph_to_wire(&ResourceGraph::default())
            .credential
            .is_none());
    }

    #[test]
    fn a_run_context_carries_the_credential_values_into_protected_memory_and_outputs_carry_none() {
        let context = RunContext {
            document: "web".to_owned(),
            credential: Some(HeldCredential::from_values(
                declared_credential(),
                vec![("privateKey".to_owned(), secret("-----KEY-----"))],
            )),
            ..RunContext::default()
        };
        let wire = context_to_wire(&context);
        let sent = wire.credential.clone().unwrap();
        assert_eq!(sent.name, "deploy");
        assert_eq!(sent.text["user"], "deploy");
        assert_eq!(sent.secrets.len(), 1);
        assert_eq!(sent.secrets[0].field, "privateKey");
        assert_eq!(sent.secrets[0].value, b"-----KEY-----");

        let back = context_from_wire(Some(wire)).unwrap();
        let held = back.credential.as_ref().unwrap();
        assert_eq!(held.name(), "deploy");
        assert_eq!(held.kind(), "ssh");
        assert_eq!(held.text("user"), Some("deploy"));
        assert!(!held.flag("agent"));
        assert_eq!(held.declared.secret_fields, ["privateKey"]);
        assert!(held
            .secret("privateKey")
            .unwrap()
            .expose(|b| b == b"-----KEY-----"));
        assert!(held.secret("password").is_none());
        // Debug names the fields and never a value.
        let shown = format!("{held:?}");
        assert!(
            shown.contains("privateKey") && !shown.contains("KEY-----"),
            "{shown}"
        );

        // A call that takes no secrets gets no credential either.
        assert!(context_to_wire_without_secrets(&context)
            .credential
            .is_none());
    }
}
