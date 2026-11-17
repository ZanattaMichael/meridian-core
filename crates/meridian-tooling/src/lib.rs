//! What an editor needs from Meridian.
//!
//! - [`fmt`]: the formatter behind `meridian fmt`.
//! - [`check`]: diagnostics from the real compiler and the target plugins.
//! - [`lint`]: the rules behind `meridian lint`.
//! - [`jsonschema`]: the JSON Schema export behind `meridian schema`.
//! - [`analysis`]: completion, hover, definitions and the outline, which
//!   the language server serves.
//! - [`catalog`]: what the loaded plugins say about themselves.
//! - [`syntax`]: a forgiving, positioned reading of a YAML file.
//!
//! None of these holds a copy of the language. The fields a document may
//! have come from [`meridian_ir::schema`], which the parser itself reads,
//! and everything about a target comes from its plugin.
pub mod analysis;
pub mod catalog;
pub mod check;
pub mod fmt;
pub mod jsonschema;
pub mod lint;
pub mod syntax;
