use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use meridian_value::{canonical_map, Value};

use crate::run::Category;
use crate::schema::ResourceSchema;

/// A point inside a source document. `line` and `column` are 1-based; a zero
/// `line` means the position is unknown. It travels with every resource so a
/// failure raised inside a plugin still points at the line the author wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Position {
    pub file: String,
    pub doc_idx: usize,
    pub line: usize,
    pub column: usize,
}

impl Position {
    /// Whether the position identifies anything at all.
    pub fn known(&self) -> bool {
        !self.file.is_empty() || self.line > 0
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.file.is_empty() {
            "<input>"
        } else {
            &self.file
        })?;
        if self.doc_idx > 0 {
            write!(f, "[doc {}]", self.doc_idx)?;
        }
        if self.line > 0 {
            write!(f, ":{}", self.line)?;
            if self.column > 0 {
                write!(f, ":{}", self.column)?;
            }
        }
        Ok(())
    }
}

/// Distinguishes an ordering constraint from a deferred trigger. The two are
/// kept structurally distinct because targets compile them differently: one
/// becomes task order, the other becomes a handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// From `dependsOn`: `from` must be applied before `to`.
    HardOrder,
    /// From `notifies`: `to` is triggered if `from` reports a change. It
    /// constrains nothing about apply order.
    Notify,
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EdgeKind::HardOrder => "dependsOn",
            EdgeKind::Notify => "notifies",
        })
    }
}

/// One unconditional unit of desired state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resource {
    pub id: String,
    pub r#type: String,
    pub state: String,
    pub params: BTreeMap<String, Value>,
    /// A condition checked on the node at the instant of apply. Only a target
    /// whose [`Capabilities::runtime_condition`] is true may accept one; plain
    /// `when` is evaluated and pruned long before a plugin sees the tree.
    pub runtime_when: String,
    /// The resource's declaration order in the source document, kept so a
    /// target that reorders resources still has the original tiebreak.
    pub index: usize,
    pub pos: Position,
}

/// A relation between two resources, carried with its kind intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub action: String,
    pub pos: Position,
}

/// A whole compiled document.
///
/// Resources are already in a deterministic apply order that satisfies every
/// ordering edge. A target is free to reorder them under its own semantics,
/// but it never has to derive an order from scratch, and two hosts compiling
/// the same document hand a plugin the same sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceGraph {
    /// The source document's `metadata.name`.
    pub name: String,
    /// The resolved target host, a literal by this point whether it was
    /// written inline or read from an Infrastructure output.
    pub host: String,
    /// The target name the document selected.
    pub target: String,
    /// The version of the tool the document asked for with
    /// `spec.targetVersion`, or empty. The host only hands over a version the
    /// plugin declared in [`Emitter::target_versions`], or one under it.
    pub target_version: String,
    pub resources: Vec<Resource>,
    pub edges: Vec<Edge>,
    /// What the document promises to later stages. Only a document that
    /// provisions infrastructure declares any.
    pub outputs: Vec<OutputDeclaration>,
}

/// A value a document promises to later stages, such as the address of a
/// machine it provisions. A plugin compiles it into the tool's own output
/// mechanism and reports its value under the same name when it applies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputDeclaration {
    pub name: String,
    /// The expression the author wrote, for example `${web_vm.public_ip}`: a
    /// reference to an attribute of a resource in this document, which only
    /// the tool can resolve.
    pub value: String,
    pub pos: Position,
}

impl ResourceGraph {
    /// Only the edges of one kind, in their existing order.
    pub fn edges_of_kind(&self, kind: EdgeKind) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.kind == kind)
    }

    /// One resource, by id.
    pub fn resource(&self, id: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.id == id)
    }

    /// The apply order as a list of resource ids.
    pub fn order(&self) -> Vec<&str> {
        self.resources.iter().map(|r| r.id.as_str()).collect()
    }

    /// Renders the tree in a stable textual form, so a test can assert that
    /// what left the host is exactly what reached the plugin.
    pub fn canonical(&self) -> String {
        let mut out = format!(
            "name={}\nhost={}\ntarget={}\n",
            self.name, self.host, self.target
        );
        if !self.target_version.is_empty() {
            let _ = writeln!(out, "targetVersion={}", self.target_version);
        }
        for r in &self.resources {
            let _ = writeln!(
                out,
                "resource {} type={} state={} runtimeWhen={} index={} params={}",
                r.id,
                r.r#type,
                r.state,
                r.runtime_when,
                r.index,
                canonical_map(&r.params)
            );
        }
        for e in &self.edges {
            let _ = writeln!(
                out,
                "edge {} -> {} kind={} action={}",
                e.from, e.to, e.kind, e.action
            );
        }
        out
    }
}

/// Resolved hierarchy data handed to a plugin alongside the tree.
pub type Data = BTreeMap<String, Value>;

/// A target's honest declaration of what it can express. It is the mechanism
/// behind the no-silent-capability-loss principle: a document that needs
/// something the target lacks fails at compile time and says so, instead of
/// compiling to output that quietly does less.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Capabilities {
    /// The target has its own change-triggered mechanism.
    pub native_notify: bool,
    /// Notification is emulated, at a cost the plan must surface to the
    /// operator.
    pub synthetic_notify: bool,
    /// The target can evaluate a condition at the instant of apply, which is
    /// what `runtimeWhen` requires.
    pub runtime_condition: bool,
}

/// The complete output of a compile: relative path to file content. It is
/// data, not files on disk, so a plan stays a pure function. Paths are ordered,
/// so printing or hashing an artifact is deterministic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Artifact {
    pub files: BTreeMap<String, String>,
}

/// A compile-time notice that does not fail the build but changes what an
/// operator should expect from the emitted artifact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Warning {
    /// The emitter that raised it.
    pub target: String,
    /// The offending resource id, when the warning is about one.
    pub resource: String,
    pub msg: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.resource.is_empty() {
            write!(f, "{}: {}", self.target, self.msg)
        } else {
            write!(f, "{}: {}: {}", self.target, self.resource, self.msg)
        }
    }
}

/// A target refusing to compile something.
///
/// It carries the resource, the rule and the source position separately, not
/// only a message, because those three survive the process boundary as fields:
/// a host can report a plugin's refusal against the author's own line, which a
/// flattened error string cannot do.
///
/// A `CompileError` is not a fault. A target rejecting a resource type it
/// cannot express is the capability contract working as designed, which is why
/// it travels as part of a successful response and never as a transport error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompileError {
    /// The emitter that refused. Filled in by the host when the error crosses
    /// the boundary, so a message never leaves the reader guessing which
    /// target spoke.
    pub target: String,
    /// The offending resource id.
    pub resource: String,
    /// A stable, machine-readable name for what was violated, for example
    /// `unsupported-resource-type`.
    pub rule: String,
    pub msg: String,
    pub pos: Position,
}

impl CompileError {
    /// A refusal of one resource.
    pub fn for_resource(r: &Resource, rule: &str, msg: impl Into<String>) -> Self {
        CompileError {
            target: String::new(),
            resource: r.id.clone(),
            rule: rule.to_owned(),
            msg: msg.into(),
            pos: r.pos.clone(),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.pos.known() {
            write!(f, "{}: ", self.pos)?;
        }
        f.write_str(&self.target)?;
        if !self.resource.is_empty() {
            write!(f, ": resource {:?}", self.resource)?;
        }
        if !self.rule.is_empty() {
            write!(f, " [{}]", self.rule)?;
        }
        write!(f, ": {}", self.msg)
    }
}

impl std::error::Error for CompileError {}

/// What a successful compile produced. Warnings describe what the target could
/// not preserve; they travel beside the artifact so a target never has to
/// choose between reporting a loss and producing output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emitted {
    pub artifact: Artifact,
    pub warnings: Vec<Warning>,
}

/// Compiles a target-agnostic resource tree into one target's native syntax.
///
/// It is deliberately separate from a runner: `emit` has no side effects, which
/// is what lets `meridian plan` be a pure function. A plugin that needs to
/// shell out to a real CLI does so behind a different interface, outside the
/// compile chain.
pub trait Emitter {
    /// The target name documents select with `spec.target`.
    fn name(&self) -> &str;
    /// Every IR resource type this target maps, sorted. Anything outside it is
    /// a compile error, never a best-effort guess.
    fn supported_resource_types(&self) -> Vec<String>;
    /// What this target can express natively.
    fn capabilities(&self) -> Capabilities;
    /// The kind of tool this target drives. The default exists so an emitter
    /// written before categories does not stop compiling; every real target
    /// should say.
    fn category(&self) -> Category {
        Category::Unspecified
    }
    /// The versions of its tool this target compiles for, such as `"3"` or
    /// `"1.9"`. A document's `spec.targetVersion` must be one of them, or a
    /// more specific version under one of them (`"3.1"` under `"3"`).
    ///
    /// The default, an empty list, means the target does not say. A document
    /// that asks such a target for a version is refused, because accepting a
    /// version nothing checks would be a silent loss.
    fn target_versions(&self) -> Vec<String> {
        Vec::new()
    }
    /// What each resource type takes, for editors: its params, states and
    /// notify actions. See [`crate::schema`].
    ///
    /// The default, an empty list, means the target does not say, and an
    /// editor then knows only the names in `supported_resource_types`. A
    /// schema is advice; `emit` stays the judge of what compiles.
    fn resource_schemas(&self) -> Vec<ResourceSchema> {
        Vec::new()
    }
    /// Compiles the tree, or refuses it.
    fn emit(&self, graph: &ResourceGraph, data: &Data) -> Result<Emitted, CompileError>;
}
