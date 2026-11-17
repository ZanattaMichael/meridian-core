//! The public contract a Meridian target plugin implements.
//!
//! A plugin is a separate binary. It implements [`Emitter`] over Meridian's
//! own resource tree; the host launches it, negotiates a protocol version, and
//! drives it over gRPC. Everything a third party needs lives in this crate,
//! which depends on none of the compiler's own.
//!
//! The types here are deliberately plain data with public fields. The
//! compiler's own tree keeps its invariants private because it builds them; by
//! the time a tree reaches a plugin those decisions are already made, and a
//! plugin author should be able to read the whole input without calling an
//! accessor for every field.
//!
//! What crosses this boundary is a compiled document, not a source one: every
//! compile-time `when` has been evaluated and pruned, the resources are in a
//! deterministic apply order, and the target host is a literal. A plugin's job
//! is mapping and serialisation, never evaluation.
//!
//! A plugin's `main` is one call to [`serve`]:
//!
//! ```no_run
//! # use meridian_sdk::*;
//! # struct MyTarget;
//! # impl Emitter for MyTarget {
//! #     fn name(&self) -> &str { "mine" }
//! #     fn supported_resource_types(&self) -> Vec<String> { Vec::new() }
//! #     fn capabilities(&self) -> Capabilities { Capabilities::default() }
//! #     fn emit(&self, _: &ResourceGraph, _: &Data) -> Result<Emitted, CompileError> {
//! #         Ok(Emitted::default())
//! #     }
//! # }
//! fn main() -> Result<(), ServeError> {
//!     serve(Plugin::new(MyTarget))
//! }
//! ```
//!
//! A plugin that also drives the real tool implements [`Runner`] and adds it
//! with [`Plugin::with_runner`]. A plugin that can read its tool's existing
//! configuration back into Meridian documents implements [`Importer`] and adds
//! it with [`Plugin::with_importer`].

// A refusal carries its position, resource and hint by value. Emit runs once
// per target, so the size of the error path is not worth boxing the contract.
#![allow(clippy::result_large_err)]

pub mod expr;
mod import;
mod listen;
mod run;
pub mod schema;
mod serve;
pub mod tool;
mod types;
pub mod version;
pub mod wire;

/// The protocol's generated message and service types. A host or a plugin
/// should not normally need them: [`wire`] converts to and from the types in
/// this crate's root.
#[allow(clippy::all, missing_docs)]
pub mod pb {
    tonic::include_proto!("meridian.plugin.v1");
}

pub use import::{
    DocumentKind, ImportError, ImportFormat, ImportNote, ImportSource, Imported, ImportedDocument,
    ImportedNotify, ImportedOutput, ImportedResource, Importer, NoteLevel,
};
pub use meridian_value::{canonical_map, decode_map, decode_value, SecretRef, Value, SENSITIVE};
pub use run::{
    Applied, Category, Change, ChangeAction, Log, LogLine, LogStream, NoLog, Operations, Output,
    Plan, RunContext, RunError, Runner,
};
pub use schema::{ParamKind, ParamSchema, ResourceSchema};
pub use serve::{serve, Plugin, ServeError};
pub use types::{
    Artifact, Capabilities, CompileError, Data, Edge, EdgeKind, Emitted, Emitter,
    OutputDeclaration, Position, Resource, ResourceGraph, Warning,
};
pub use version::PROTOCOLS_ENV;

/// The newest wire protocol this SDK speaks. A host and a plugin agree on a
/// version before the first call ([`version`]): the highest both speak. A host
/// refuses a plugin announcing a version outside the host's range, instead of
/// guessing at the difference.
///
/// It is a separate number from the SDK's release version on purpose: the SDK
/// gains helpers far more often than the wire format changes, and a plugin
/// built against an older SDK stays loadable for as long as its protocol is
/// in the host's range.
///
/// Version 2 added the plugin's category and operations to `Describe`, and
/// the `Runner` service. The optional `Importer` service was added without a
/// new version: a plugin built before it lists no import formats and is never
/// asked to import.
pub const PROTOCOL_VERSION: u32 = 2;

/// The oldest wire protocol this SDK still speaks. It trails
/// [`PROTOCOL_VERSION`] by at most one, and rises only through the
/// deprecation policy.
pub const MIN_PROTOCOL_VERSION: u32 = 2;

/// The SDK release a plugin was compiled against. It is reported over the
/// handshake, not inferred by the host, so a version mismatch is a fact the
/// host reads and not a guess it makes.
pub const VERSION: &str = "0.1.0";

/// Opens the single line a plugin writes to stdout once it is listening:
/// `MERIDIAN-PLUGIN|<protocol>|<sdk version>|<network>|<address>`. The host
/// reads that line to learn where to dial; anything else on stdout before it
/// means the binary is not a Meridian plugin, which is a much clearer failure
/// than a dial timeout.
pub const HANDSHAKE_MAGIC: &str = "MERIDIAN-PLUGIN";
