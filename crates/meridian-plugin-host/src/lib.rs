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
/// Signatures on plugin binaries. The code is in the `meridian-signing`
/// crate, which is not about plugins and can be used for any file.
pub mod signature {
    pub use meridian_signing::*;
}

use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use meridian_sdk::{
    Capabilities, Category, Change, CompileError, ImportError, ImportFormat, Operations,
    PlacedEscape, RunError, SecretRef, Value,
};

pub use client::{Client, StartOptions, HANDSHAKE_TIMEOUT};
pub use registry::{binary_name, Registry, Target};
pub use signature::{Policy, PublicKey, SecretKey, Signature, Verification};

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
    /// The openings of the tool's own expression syntax that a value placed
    /// from data must not form, and how each is written as literal text.
    pub placed_text: Vec<PlacedEscape>,
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
    /// What each resource type takes, as the target describes it for
    /// editors; empty when it does not say.
    pub resource_schemas: Vec<meridian_sdk::ResourceSchema>,
    /// The kinds of existing configuration the target can turn into Meridian
    /// documents; empty when it does not import.
    pub import_formats: Vec<ImportFormat>,
    /// Whether the target can ask its installed tool for the schemas of the
    /// tool's own resource types, for `meridian schema sync`.
    pub exports_schemas: bool,
    /// What an operator can configure about the target's runner, as the
    /// plugin declares it; empty when it declares none.
    pub settings: Vec<meridian_sdk::Setting>,
    /// The binary the plugin was loaded from; empty for an in-process emitter.
    pub path: String,
    /// What checking the binary's signature found, before it was started.
    pub signature: Signature,
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
    /// The run was stopped before the tool finished: the operator asked, or
    /// it ran past its time limit. Nothing is wrong with the tool or the
    /// plugin.
    Stopped(Stopped),
}

impl fmt::Display for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunFailure::Tool(e) => e.fmt(f),
            RunFailure::Plugin(e) => e.fmt(f),
            RunFailure::Stopped(e) => e.fmt(f),
        }
    }
}

/// When a plan or an apply is to be stopped before the tool finishes.
///
/// The default stops nothing: a tool takes as long as it takes, as it always
/// did. Either way of stopping asks the plugin to cancel, waits for it to say
/// what the tool had done, and reports the run as [`RunFailure::Stopped`].
#[derive(Clone, Debug)]
pub struct Stop {
    /// Set to true by whoever wants the run stopped now: a signal handler on
    /// Ctrl-C, or another thread. Read, never cleared, by the host.
    pub when: Option<Arc<AtomicBool>>,
    /// How long one plan or apply may take before it is stopped.
    pub after: Option<Duration>,
    /// How long, after asking the plugin to stop, to wait for it to report
    /// what the tool had done. A plugin that says nothing by then is left to
    /// itself and the run is reported with nothing done.
    pub grace: Duration,
}

/// How long a plugin has to answer a stop unless a caller says otherwise. The
/// SDK gives a tool ten seconds to exit on its own before killing it, so a
/// well-behaved plugin answers within that; a little more covers the answer
/// itself.
pub const STOP_GRACE: Duration = Duration::from_secs(20);

impl Default for Stop {
    fn default() -> Self {
        Stop {
            when: None,
            after: None,
            grace: STOP_GRACE,
        }
    }
}

impl Stop {
    /// Stops a run when `flag` turns true.
    pub fn when(flag: Arc<AtomicBool>) -> Self {
        Stop {
            when: Some(flag),
            ..Stop::default()
        }
    }

    /// Stops a run that has taken longer than `limit`.
    pub fn after(limit: Duration) -> Self {
        Stop {
            after: Some(limit),
            ..Stop::default()
        }
    }

    /// Whether this ever stops anything.
    pub fn is_set(&self) -> bool {
        self.when.is_some() || self.after.is_some()
    }
}

/// A run that was stopped before the tool finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    pub cause: StopCause,
    /// What the tool had done before it was stopped, as far as the plugin
    /// reported it. Empty when the plugin could not say.
    pub changes: Vec<Change>,
    /// The end of what the tool printed, when the plugin reported it.
    pub output: String,
    /// What the plugin was running, with any secret removed.
    pub command: String,
    /// Whether the plugin answered the request to stop. When it did not, the
    /// tool may still be running on its own.
    pub answered: bool,
}

/// Why a run was stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopCause {
    /// Whoever drives the run asked for it to stop.
    Interrupted,
    /// The run reached its time limit.
    TimedOut(Duration),
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.cause {
            StopCause::Interrupted => write!(f, "interrupted: the run was stopped")?,
            StopCause::TimedOut(limit) => {
                write!(f, "timed out after {}", describe_duration(limit))?
            }
        }
        if !self.command.is_empty() {
            write!(f, " (was running `{}`)", self.command)?;
        }
        if !self.answered {
            write!(
                f,
                "; the plugin did not confirm that the tool was stopped, so it may still be \
                 running"
            )?;
        }
        if !self.output.is_empty() {
            write!(f, "\n{}", self.output.trim_end())?;
        }
        Ok(())
    }
}

/// A duration as a person writes one: `30s`, `5m`, `1h30m`, `250ms`.
pub fn describe_duration(d: Duration) -> String {
    let millis = d.as_millis();
    if millis == 0 {
        return "0s".to_owned();
    }
    if millis < 1000 {
        return format!("{millis}ms");
    }
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    let mut text = String::new();
    if h > 0 {
        text.push_str(&format!("{h}h"));
    }
    if m > 0 {
        text.push_str(&format!("{m}m"));
    }
    if s > 0 || text.is_empty() {
        text.push_str(&format!("{s}s"));
    }
    if !millis.is_multiple_of(1000) {
        text.push_str(&format!("{}ms", millis % 1000));
    }
    text
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
        secrets: Vec::new(),
        credential: graph.credential.as_ref().map(|c| {
            // A field is secret by the type's say-so, not by what it holds:
            // before the document is resolved it holds the reference as
            // written, and that is not text a plugin may read either.
            let is_secret = |field: &str| c.kind().is_some_and(|kind| kind.is_secret(field));
            let mut secret_fields: Vec<String> = c
                .fields
                .iter()
                .filter(|(k, v)| is_secret(k) || matches!(v, Value::Secret(_)))
                .map(|(k, _)| k.clone())
                .collect();
            secret_fields.sort_unstable();
            meridian_sdk::Credential {
                name: c.name.clone(),
                kind: c.r#type.clone(),
                ambient: c.is_ambient(),
                text: c
                    .fields
                    .iter()
                    .filter(|(k, _)| !is_secret(k))
                    .filter_map(|(k, v)| match v {
                        Value::String(text) => Some((k.clone(), text.clone())),
                        _ => None,
                    })
                    .collect(),
                flags: c
                    .fields
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Value::Bool(b) => Some((k.clone(), *b)),
                        _ => None,
                    })
                    .collect(),
                secret_fields,
            }
        }),
    }
}

/// Names every secret the graph's params refer to, in the order they are
/// met, so a plugin can write a reference to each: by the name the document
/// declared it under, or, for one made from a declared secret by an
/// expression (a secret joined with text, say), `derived_1`, `derived_2`
/// and so on, since no name of the document's is its own.
pub fn name_secrets(graph: &mut meridian_sdk::ResourceGraph, declared: &[(String, SecretRef)]) {
    fn walk(value: &Value, found: &mut Vec<SecretRef>) {
        match value {
            Value::Secret(at) => {
                if !found.contains(at) {
                    found.push(*at);
                }
            }
            Value::List(items) => items.iter().for_each(|item| walk(item, found)),
            Value::Map(entries) => entries.values().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    let mut used = Vec::new();
    for resource in &graph.resources {
        for value in resource.params.values() {
            walk(value, &mut used);
        }
    }
    let mut derived = 0;
    graph.secrets = used
        .into_iter()
        .map(|secret| {
            let name = match declared.iter().find(|(_, at)| *at == secret) {
                Some((name, _)) => name.clone(),
                None => {
                    derived += 1;
                    format!("derived_{derived}")
                }
            };
            meridian_sdk::SecretName { secret, name }
        })
        .collect();
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    #[test]
    fn a_duration_reads_as_a_person_writes_one() {
        assert_eq!(describe_duration(Duration::ZERO), "0s");
        assert_eq!(describe_duration(Duration::from_millis(300)), "300ms");
        assert_eq!(describe_duration(Duration::from_secs(1)), "1s");
        assert_eq!(describe_duration(Duration::from_secs(90)), "1m30s");
        assert_eq!(describe_duration(Duration::from_secs(3600)), "1h");
        assert_eq!(describe_duration(Duration::from_secs(3661)), "1h1m1s");
        assert_eq!(describe_duration(Duration::from_millis(1500)), "1s500ms");
    }

    #[test]
    fn a_stopped_run_describes_itself() {
        let stopped = Stopped {
            cause: StopCause::TimedOut(Duration::from_secs(300)),
            changes: Vec::new(),
            output: "last words\n".to_owned(),
            command: "tool apply".to_owned(),
            answered: true,
        };
        assert_eq!(
            stopped.to_string(),
            "timed out after 5m (was running `tool apply`)\nlast words"
        );
        let unanswered = Stopped {
            cause: StopCause::Interrupted,
            answered: false,
            output: String::new(),
            command: String::new(),
            ..stopped
        };
        assert_eq!(
            unanswered.to_string(),
            "interrupted: the run was stopped; the plugin did not confirm that the tool was \
             stopped, so it may still be running"
        );
    }
}
