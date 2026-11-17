//! Asking the installed tool for the schemas of its own resource types.
//!
//! A plugin declares the types Meridian itself defines through
//! [`Emitter::resource_schemas`](crate::Emitter::resource_schemas). The
//! tool's own types, `aws_instance` or `apps/v1/Deployment`, are too many and
//! change too often to be written into a plugin, but every tool publishes
//! them in a machine-readable form: `terraform providers schema -json`, the
//! Kubernetes OpenAPI document, a Pulumi package's schema, a DSC resource
//! manifest. A plugin that knows how to ask its tool implements
//! [`SchemaSource`], and `meridian schema sync` calls it once and caches the
//! answer under the project's state directory, keyed by the versions the
//! answer came from. Editors then read the cache and never run the tool.
//!
//! The rule a schema source keeps is the one a schema keeps: it is advice.
//! `emit` and the tool remain the judges of what compiles and applies, and a
//! schema must never promise something the tool refuses. What the source
//! cannot describe it leaves out, with a note, and an editor then knows as
//! much about that type as it did before: its name.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::{Log, ResourceGraph, ResourceSchema, RunError};

/// What the host hands a schema source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SchemaRequest {
    /// A directory the host created for this target and keeps between
    /// syncs. The plugin may initialise its tool there and keep what it
    /// downloaded in it; the host never reads it.
    pub work_dir: PathBuf,
    /// The documents that use this target, as the trees `emit` would get,
    /// so the plugin can read which providers, packages and versions the
    /// project uses.
    pub graphs: Vec<ResourceGraph>,
    /// The resource types the documents use, deduplicated and sorted.
    pub types: Vec<String>,
}

impl SchemaRequest {
    /// A request for the types in `graphs`.
    pub fn new(work_dir: PathBuf, graphs: Vec<ResourceGraph>) -> Self {
        let mut types: Vec<String> = graphs
            .iter()
            .flat_map(|g| g.resources.iter().map(|r| r.r#type.clone()))
            .collect();
        types.sort();
        types.dedup();
        SchemaRequest {
            work_dir,
            graphs,
            types,
        }
    }
}

/// What a schema source answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaExport {
    /// One entry per resource type described, under the exact name a
    /// document writes (never a pattern). A type the tool could not
    /// describe is left out.
    pub schemas: Vec<ResourceSchema>,
    /// The tool that produced the schemas, such as `terraform`.
    pub tool: String,
    /// Its version, such as `1.13.1`.
    pub tool_version: String,
    /// The exact versions of the parts the schemas were read from, keyed by
    /// the tool's own name for each: a provider, a package, an API server.
    /// The host records them, so a cache read from other versions is told
    /// apart from this one.
    pub pins: BTreeMap<String, String>,
    /// Things the operator should know: a type that could not be described,
    /// a provider that was skipped.
    pub notes: Vec<String>,
}

impl SchemaExport {
    pub fn new(tool: &str, tool_version: &str) -> Self {
        SchemaExport {
            tool: tool.to_owned(),
            tool_version: tool_version.to_owned(),
            ..SchemaExport::default()
        }
    }

    pub fn pin(&mut self, name: &str, version: &str) {
        self.pins.insert(name.to_owned(), version.to_owned());
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Sorts the schemas by type and drops a repeated type, keeping the
    /// first. Patterns are dropped too: a cache holds exact names only.
    pub fn tidy(&mut self) {
        self.schemas
            .retain(|s| !s.is_pattern() && !s.r#type.is_empty());
        self.schemas.sort_by(|a, b| a.r#type.cmp(&b.r#type));
        self.schemas.dedup_by(|a, b| a.r#type == b.r#type);
        self.notes.dedup();
    }
}

/// The optional half of a plugin that asks its tool for schemas.
///
/// It may run the tool and use the network (to fetch providers, say), and
/// it changes nothing outside `request.work_dir`. It is called by `meridian
/// schema sync`, never by an editor, so it may take a while; what it writes
/// to `log` is shown to the person running the sync.
pub trait SchemaSource: Send + Sync {
    /// Describes the types in `request`, or as many of them as the tool
    /// can. A tool that cannot be run at all is a [`RunError`]; a type that
    /// cannot be described is a note.
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError>;
}

/// `tool --version`'s first line, reduced to the version number: the first
/// run of digits and dots that starts with a digit, with a leading `v`
/// allowed. `None` when the text holds no version.
pub fn version_in(text: &str) -> Option<String> {
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '/')) {
        let word = word.trim_start_matches('v');
        // Digits and dots, then anything a pre-release tag may hold.
        let end = word
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
            .unwrap_or(word.len());
        let candidate = &word[..end];
        let number = candidate
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(candidate.len());
        if candidate.starts_with(|c: char| c.is_ascii_digit()) && candidate[..number].contains('.')
        {
            return Some(candidate.trim_end_matches('.').to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_found_in_what_a_tool_prints() {
        assert_eq!(
            version_in("Terraform v1.13.1\non linux_amd64").as_deref(),
            Some("1.13.1")
        );
        assert_eq!(version_in("OpenTofu v1.11.5").as_deref(), Some("1.11.5"));
        assert_eq!(version_in("v3.145.0").as_deref(), Some("3.145.0"));
        assert_eq!(
            version_in("Client Version: v1.31.0\nServer Version: v1.30.2").as_deref(),
            Some("1.31.0")
        );
        assert_eq!(
            version_in("aws-cli/2.17.0 Python/3.11").as_deref(),
            Some("2.17.0")
        );
        assert_eq!(
            version_in("dsc 3.1.0-preview.2").as_deref(),
            Some("3.1.0-preview.2")
        );
        assert_eq!(version_in("no number here"), None);
    }

    #[test]
    fn a_request_lists_the_types_once_and_sorted() {
        let mut g = ResourceGraph::default();
        for t in ["b", "a", "b"] {
            g.resources.push(crate::Resource {
                r#type: t.to_owned(),
                ..Default::default()
            });
        }
        let request = SchemaRequest::new(PathBuf::from("w"), vec![g]);
        assert_eq!(request.types, ["a", "b"]);
    }

    #[test]
    fn an_export_keeps_exact_types_only() {
        let mut export = SchemaExport::new("t", "1");
        export.schemas.push(ResourceSchema::open("<p>_<t>", ""));
        export.schemas.push(ResourceSchema::open("b", ""));
        export.schemas.push(ResourceSchema::open("a", ""));
        export.schemas.push(ResourceSchema::open("a", "again"));
        export.tidy();
        let names: Vec<&str> = export.schemas.iter().map(|s| s.r#type.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(export.schemas[0].description, "");
    }
}
