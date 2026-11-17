//! The host side of Meridian's plugin protocol.
//!
//! A target is a separate process. The host launches the binary, reads one
//! handshake line telling it where the plugin is listening, checks the
//! protocol version, and then talks to it over gRPC. Nothing above this crate
//! needs to know whether a target is a subprocess or compiled in: the
//! [`Registry`] resolves a target name to something that can emit either way.
//!
//! The reason for a process boundary at all is that a target's mapping tables
//! are the part of Meridian most likely to be written by someone else. A
//! plugin that crashes, leaks memory, or is built against a different SDK
//! cannot take the host down with it, and a third party can ship one without a
//! fork. It also means a plugin may be written in any language: the plugins
//! this host was first proven against are Go binaries.

// A refusal carries its position, resource and hint by value. Emit runs once
// per target, so the size of the error path is not worth boxing the contract.
#![allow(clippy::result_large_err)]

mod client;
mod registry;

use std::fmt;

use meridian_sdk::{
    Capabilities, Category, CompileError, ImportError, ImportFormat, Operations, RunError,
};

pub use client::{Client, StartOptions, HANDSHAKE_TIMEOUT};
pub use registry::{binary_name, Registry, Target};

/// The naming convention a discoverable plugin binary follows. Discovery is by
/// name, not by executing every file in a directory, because running an
/// unknown binary to ask what it is would be a poor trade.
pub const BINARY_PREFIX: &str = "meridian-target-";

/// What a target reports about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// The wire protocol the plugin speaks.
    pub protocol_version: u32,
    /// The SDK release it was compiled against, reported and not inferred.
    pub sdk_version: String,
    /// The target name documents select with `spec.target`.
    pub name: String,
    /// Every IR resource type the target maps.
    pub resource_types: Vec<String>,
    pub capabilities: Capabilities,
    /// The kind of tool the target drives.
    pub category: Category,
    /// What the target can do beyond compiling.
    pub operations: Operations,
    /// Whether the target hands a run's inputs to its tool, which a
    /// document's `spec.imports` needs.
    pub accepts_inputs: bool,
    /// The versions of its tool the target compiles for; empty when it does
    /// not say.
    pub target_versions: Vec<String>,
    /// The kinds of existing configuration the target can turn into Meridian
    /// documents; empty when it does not import.
    pub import_formats: Vec<ImportFormat>,
    /// The binary the plugin was loaded from; empty for an in-process emitter.
    pub path: String,
}

/// A plugin-loading or plugin-transport failure.
///
/// It is a distinct type from [`CompileError`] because the two mean opposite
/// things: a `CompileError` is a target correctly refusing a document, while
/// this is the plugin mechanism itself failing. Collapsing them would let a
/// crashed binary read like a rejected resource.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Error {
    /// The target name, once it is known.
    pub plugin: String,
    pub path: String,
    pub msg: String,
    /// The underlying failure, when there is one.
    pub cause: String,
    /// Whatever the plugin wrote to stderr before it failed, kept because a
    /// plugin that dies during startup usually explains itself there and
    /// nowhere else.
    pub stderr: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.plugin.is_empty() {
            write!(f, "plugin {:?}", self.plugin)?;
        } else if !self.path.is_empty() {
            write!(f, "plugin {}", self.path)?;
        } else {
            f.write_str("plugin")?;
        }
        write!(f, ": {}", self.msg)?;
        if !self.cause.is_empty() {
            write!(f, ": {}", self.cause)?;
        }
        if !self.stderr.is_empty() {
            write!(f, "\nplugin stderr: {}", self.stderr)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

/// Why a target produced no artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// The target refused the document. This is the capability contract
    /// working as designed, and is reported against the author's source.
    Refused(CompileError),
    /// The plugin mechanism failed: the process died, or the call did.
    Plugin(Error),
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::Refused(e) => e.fmt(f),
            EmitError::Plugin(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for EmitError {}

/// Why a run produced no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunFailure {
    /// The tool failed: it exited non-zero, rejected the configuration, or
    /// could not reach the node. This is an answer about the infrastructure.
    Tool(RunError),
    /// The plugin mechanism failed: the target does not offer the operation,
    /// the process died, or it sent something this host cannot read.
    Plugin(Error),
}

impl fmt::Display for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunFailure::Tool(e) => e.fmt(f),
            RunFailure::Plugin(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RunFailure {}

/// Why an import produced no documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportFailure {
    /// The importer could not read the source: a syntax error, or a file that
    /// is not its tool's. This is an answer about what the operator handed
    /// over.
    Source(ImportError),
    /// The plugin mechanism failed: the target does not import, the process
    /// died, or it sent something this host cannot read.
    Plugin(Error),
}

impl fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportFailure::Source(e) => e.fmt(f),
            ImportFailure::Plugin(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ImportFailure {}

/// Converts the compiler's own tree into the public form a target consumes.
///
/// This is the only seam between the two, and they are separate types on
/// purpose. The compiler's tree owns invariants it built: its resources are in
/// an order it derived, and its index is consistent with that order. Handing
/// that type out as the public contract would publish those internals, and
/// every later change to how the compiler represents a document would become a
/// breaking change for every third-party plugin.
pub fn to_sdk(graph: &meridian_ast::ResourceGraph) -> meridian_sdk::ResourceGraph {
    let position = |p: &meridian_ast::Position| meridian_sdk::Position {
        file: p.file.clone(),
        doc_idx: p.doc_idx,
        line: p.line,
        column: p.column,
    };
    meridian_sdk::ResourceGraph {
        name: graph.name.clone(),
        host: graph.host.clone(),
        target: graph.target.clone(),
        target_version: graph.target_version.clone(),
        resources: graph
            .resources()
            .iter()
            .map(|r| meridian_sdk::Resource {
                id: r.id.clone(),
                r#type: r.r#type.clone(),
                state: r.state.clone(),
                params: r.params.clone(),
                runtime_when: r.runtime_when.clone(),
                index: r.index,
                pos: position(&r.pos),
            })
            .collect(),
        edges: graph
            .edges()
            .iter()
            .map(|e| meridian_sdk::Edge {
                from: e.from.clone(),
                to: e.to.clone(),
                kind: match e.kind {
                    meridian_ast::EdgeKind::HardOrder => meridian_sdk::EdgeKind::HardOrder,
                    meridian_ast::EdgeKind::Notify => meridian_sdk::EdgeKind::Notify,
                },
                action: e.action.clone(),
                pos: position(&e.pos),
            })
            .collect(),
        outputs: graph
            .outputs
            .iter()
            .map(|o| meridian_sdk::OutputDeclaration {
                name: o.name.clone(),
                value: o.value.clone(),
                pos: position(&o.pos),
            })
            .collect(),
    }
}
