//! A small, working plugin to copy when writing a new one.
//!
//! `python3 tools/new_plugin.py <name>` copies this crate to `plugins/<name>`
//! and renames everything in it. The guide that walks through what to change
//! is `docs/contributing/writing-a-plugin.md`. Every place that needs a
//! decision from you is marked `TODO(plugin)`.
//!
//! # The tool this template drives
//!
//! The tool is imaginary and deliberately tiny, so that the shape of a plugin
//! is all there is to read. It is called `filesync`. It reads a manifest with
//! one line per file it should manage, and it has two commands:
//!
//! ```text
//! filesync plan manifest.tsv     prints what it would change, changes nothing
//! filesync apply manifest.tsv    makes the changes, and prints what it did
//! ```
//!
//! Both print one line per change: `create /path`, `update /path` or
//! `delete /path`.
//!
//! # What a document looks like to this target
//!
//! Every resource has the type `file`, with the params `path` (required),
//! `content` and `mode`, and the state `present` (the default) or `absent`.
//!
//! # The four stages
//!
//! Compiling is always the same four steps, in this order, each in a module
//! of its own: [`transform`] maps each resource to the tool's vocabulary or
//! refuses it, [`sort`] applies the tool's ordering rules, [`validate`]
//! checks what only the tool's own rules can say, and [`emit`] serialises.
//! Running the tool is [`runner`], outside the compile chain, so compiling
//! never has a side effect.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod runner;
mod sort;
mod transform;
mod validate;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph,
};

pub use emit::MANIFEST_PATH;
pub use runner::TemplateRunner;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "template";

/// One line of the manifest: a file the tool manages.
///
/// TODO(plugin): replace this with your tool's own model. Most plugins have
/// one struct here per thing the tool's file format declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The resource's id, kept so a later stage can still name the resource.
    pub id: String,
    pub path: String,
    pub content: String,
    pub mode: String,
    pub present: bool,
    pub pos: Position,
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// A refusal of something that is not a resource, or of a resource that has
/// already been transformed.
pub(crate) fn refuse_at(resource: &str, pos: &Position, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        rule: rule.to_owned(),
        msg,
        pos: pos.clone(),
    }
}

/// The compiling half of the plugin.
pub struct Template;

impl Emitter for Template {
    fn name(&self) -> &str {
        NAME
    }

    /// Every resource type this target maps, sorted. A document that uses any
    /// other type is refused.
    fn supported_resource_types(&self) -> Vec<String> {
        vec!["file".to_owned()]
    }

    /// What each type takes, for editors: `meridian lsp` completes and
    /// documents the params and states listed here, and reports a param that
    /// is not listed before the document is ever compiled. Build it from the
    /// same table the transform stage checks against, so the two cannot
    /// disagree.
    ///
    /// TODO(plugin): a native target with open-ended types can leave this
    /// out; its types are then offered by name and their params are not
    /// checked in the editor.
    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        vec![meridian_sdk::schema::shared(
            "file",
            &transform::PARAMS,
            &["path"],
            &["present", "absent"],
            "present",
            &[],
        )]
    }

    /// TODO(plugin): declare only what the tool can really do. Each `true`
    /// here is a promise the capability matrix publishes: `native_notify`
    /// when the tool has its own way to react to a change, `synthetic_notify`
    /// when you build that reaction out of something else, and
    /// `runtime_condition` when the tool can check a condition on the node at
    /// the moment it applies a resource.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: false,
            runtime_condition: false,
        }
    }

    /// TODO(plugin): the kind of tool this is. The core uses it to decide
    /// which workflows the plugin can join.
    fn category(&self) -> Category {
        Category::ConfigurationManagement
    }

    // TODO(plugin): if the tool comes in versions that need different output,
    // also implement `target_versions`. Without it, a document that sets
    // `spec.targetVersion` for this target is refused, which is the right
    // answer for a plugin that would not act on the version.

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A `${id.param}` in a param is a reference to another resource. This
        // tool has nothing to read at apply time, so each one is settled now,
        // from what the document declares, or refused.
        //
        // TODO(plugin): keep this for a tool like that, which is most
        // configuration management tools. A tool with its own references
        // (Terraform, Compose) rewrites them into its own spelling in
        // `transform` instead; `meridian_sdk::expr` finds them for you.
        let graph = &meridian_sdk::expr::resolve_declared(graph, NAME)?;
        let mut entries = transform::transform(graph)?;
        let warnings = sort::apply_ordering(graph, &mut entries)?;
        validate::validate(&entries)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&entries),
            },
            warnings,
        })
    }
}
