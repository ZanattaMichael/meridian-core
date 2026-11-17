//! What Meridian's importers share.
//!
//! An importer is the half of a target plugin that reads its tool's existing
//! configuration and returns the Meridian documents that describe the same
//! thing (see `meridian_sdk::Importer`). Reading a tool's files is the
//! importer's own business, but several tools write the same notations, and
//! every imported document is written out the same way. Those shared parts
//! live here:
//!
//! - [`yaml`] reads YAML and JSON into a tree that keeps key order and line
//!   numbers, with the scalar rules of the tool whose file it is;
//! - [`hcl`] reads HashiCorp's configuration language into the same shape its
//!   JSON syntax has;
//! - [`tf`] maps the Terraform language, which Terraform and OpenTofu both
//!   read, onto a document for either target;
//! - [`Notes`] collects what an importer has to tell the operator;
//! - [`render`] writes an imported document in Meridian's current syntax,
//!   with a `TODO` marker wherever the importer said something was lost;
//! - [`diff`] compares two configurations by what they mean, which is how a
//!   round-trip test knows import and compile agree.
//!
//! A plugin's library may depend on this crate as it does on the SDK: it
//! depends on nothing of the compiler's.

pub mod diff;
pub mod hcl;
mod notes;
mod render;
pub mod tf;
pub mod yaml;

pub use notes::{literal, slug, unique_id, Notes};
pub use render::{render, render_all, Rendered, TODO_MARKER};
