//! Stage one: map each resource to the tool's own vocabulary, or refuse it.
//!
//! This is where almost every refusal lives. The rule is the one the whole
//! project rests on: a document either compiles to something that does what
//! it says, or it does not compile and the author is told why. Never drop a
//! field you do not understand, and never guess.

use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, refuse_at, Entry};

/// The params a `file` resource may carry.
pub(crate) const PARAMS: [&str; 3] = ["content", "mode", "path"];

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Vec<Entry>, CompileError> {
    // TODO(plugin): a tool that can report values (an address, an id) compiles
    // each declared output into its own output mechanism here. This one has
    // nothing to report, so asking it for an output is refused.
    if let Some(output) = graph.outputs.first() {
        return Err(refuse_at(
            "",
            &output.pos,
            "unsupported-output",
            format!(
                "declares the output {:?}, and filesync reports no values",
                output.name
            ),
        ));
    }
    graph.resources.iter().map(entry).collect()
}

fn entry(r: &Resource) -> Result<Entry, CompileError> {
    if r.r#type != "file" {
        return Err(refuse(
            r,
            "unsupported-resource-type",
            format!(
                "has the type {:?}, which this target cannot express (supported: file)",
                r.r#type
            ),
        ));
    }
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            "unsupported-runtime-condition",
            format!(
                "has the runtime condition {:?}, and filesync evaluates nothing on the node",
                r.runtime_when
            ),
        ));
    }
    let present = match r.state.as_str() {
        "" | "present" => true,
        "absent" => false,
        other => {
            return Err(refuse(
                r,
                "unsupported-state",
                format!("has the state {other:?} (supported: present, absent)"),
            ))
        }
    };
    if let Some(unknown) = r.params.keys().find(|k| !PARAMS.contains(&k.as_str())) {
        return Err(refuse(
            r,
            "unknown-param",
            format!(
                "has the param {unknown:?}, which a file does not take (supported: {})",
                PARAMS.join(", ")
            ),
        ));
    }
    let path = text(r, "path")?
        .ok_or_else(|| refuse(r, "missing-param", "needs the param \"path\"".to_owned()))?;
    Ok(Entry {
        id: r.id.clone(),
        path,
        content: text(r, "content")?.unwrap_or_default(),
        mode: text(r, "mode")?.unwrap_or_else(|| "0644".to_owned()),
        present,
        pos: r.pos.clone(),
    })
}

/// A param that must be text when it is given at all.
fn text(r: &Resource, name: &str) -> Result<Option<String>, CompileError> {
    match r.params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(refuse(
            r,
            "invalid-param",
            format!(
                "has the param {name:?} as {}, and it must be text",
                other.describe()
            ),
        )),
    }
}
