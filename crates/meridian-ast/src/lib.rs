//! The target-agnostic resource tree that every emitter consumes.
//!
//! The tree is deliberately a different type from the IR, not an alias for it.
//! The IR is what an author wrote; the tree is what the compiler decided, after
//! hierarchy resolution and condition pruning have run. By the time a tree is
//! built every `when` has already been evaluated and every surviving resource
//! is unconditional, so an emitter never has to reason about whether a
//! resource is really going to be applied.

use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Write as _};

use meridian_graph::Graph;
pub use meridian_ir::Output;
pub use meridian_ir::Position;
use meridian_ir::{canonical_map, Bundle, Infrastructure, ResourceSet, Value};

/// The graph crate's distinction between a hard ordering constraint and a
/// deferred trigger, redeclared here so an emitter can depend on the tree
/// alone. The order of the variants is the order edges sort in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// From `dependsOn`: `from` must be applied before `to`.
    HardOrder,
    /// From `notifies`: `to` is triggered if `from` reports a change.
    Notify,
}

impl From<meridian_graph::EdgeKind> for EdgeKind {
    fn from(kind: meridian_graph::EdgeKind) -> Self {
        match kind {
            meridian_graph::EdgeKind::HardOrder => EdgeKind::HardOrder,
            meridian_graph::EdgeKind::Notify => EdgeKind::Notify,
        }
    }
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
    /// Survives into the tree because it is not Meridian's to evaluate: it is
    /// a condition checked on the node at the instant of apply, and only a
    /// target that declares the runtime-condition capability may accept it.
    /// Plain `when` never reaches here.
    pub runtime_when: String,
    /// The resource's declaration order in the source document, kept so a
    /// target that reorders resources still has the original tiebreak.
    pub index: usize,
    pub pos: Position,
}

/// A relation between two resources, carried into the tree with its kind
/// intact so a target can compile `dependsOn` and `notifies` differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub action: String,
    pub pos: Position,
}

impl Edge {
    pub fn new(from: &str, to: &str, kind: EdgeKind, action: &str) -> Self {
        Edge {
            from: from.to_owned(),
            to: to.to_owned(),
            kind,
            action: action.to_owned(),
            pos: Position::default(),
        }
    }
}

/// A failure to build a tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
    pub pos: Position,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.pos.line > 0 || !self.pos.file.is_empty() {
            write!(f, "{}: ", self.pos)?;
        }
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

/// A whole compiled document: its resources in a deterministic apply order,
/// the edges between them, and the host they configure.
///
/// The resources and edges are private so the order the compiler derived, and
/// the index consistent with it, cannot be disturbed after the fact.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceGraph {
    /// The source document's `metadata.name`.
    pub name: String,
    /// The resolved target host. A literal by this point, whether it came from
    /// an inline declaration or an Infrastructure output.
    pub host: String,
    /// The target name the document declared.
    pub target: String,
    /// The version of the tool the document asked for with
    /// `spec.targetVersion`; empty when it did not.
    pub target_version: String,

    /// What the document promises to later stages, in declaration order.
    /// Only an Infrastructure document declares any.
    pub outputs: Vec<Output>,
    /// The credential the host is reached with, when the document names one
    /// in `targetHost.credential`. Resolved: its secret fields are slots.
    pub credential: Option<meridian_ir::Credential>,
    /// `spec.settings`: the tool's own settings for the whole document, which
    /// the target checks as it checks params. Empty when none are set.
    pub settings: BTreeMap<String, Value>,

    resources: Vec<Resource>,
    by_id: HashMap<String, usize>,
    edges: Vec<Edge>,
}

impl ResourceGraph {
    /// Turns a validated ResourceSet into a target-agnostic tree bound to
    /// `host`. The graph is built and topologically sorted here so every
    /// emitter sees the same deterministic order instead of deriving its own.
    pub fn build(rs: &ResourceSet, host: &str) -> Result<ResourceGraph, Error> {
        if host.is_empty() {
            return Err(Error {
                msg: format!(
                    "document {:?} has no resolved target host",
                    rs.metadata.name
                ),
                pos: rs.pos.clone(),
            });
        }
        let mut graph = ResourceGraph::from_resources(
            &rs.metadata.name,
            host,
            &rs.spec.target,
            &rs.spec.resources,
        )?;
        graph.target_version = rs.spec.target_version.clone();
        graph.settings = rs.spec.settings.clone();
        graph.credential = rs
            .spec
            .target_host
            .as_ref()
            .filter(|th| !th.credential.is_empty())
            .and_then(|th| rs.spec.credentials.iter().find(|c| c.name == th.credential))
            .cloned();
        Ok(graph)
    }

    /// Turns a validated Infrastructure document into a tree. It has no host:
    /// it provisions the things later documents are applied to, and hands
    /// them over through its declared outputs.
    pub fn build_infrastructure(doc: &Infrastructure) -> Result<ResourceGraph, Error> {
        let mut graph = ResourceGraph::from_resources(
            &doc.metadata.name,
            "",
            &doc.spec.target,
            &doc.spec.resources,
        )?;
        graph.outputs = doc.spec.outputs.clone();
        graph.target_version = doc.spec.target_version.clone();
        graph.settings = doc.spec.settings.clone();
        Ok(graph)
    }

    fn from_resources(
        name: &str,
        host: &str,
        target: &str,
        resources: &[meridian_ir::Resource],
    ) -> Result<ResourceGraph, Error> {
        // A surviving `when` means the condition stage has not run. Emitting it
        // would either leak a conditional into supposedly static output or drop
        // it silently; both are worse than refusing.
        if let Some(r) = resources.iter().find(|r| !r.when.is_empty()) {
            return Err(Error {
                msg: format!(
                    "resource {:?} still carries an unevaluated `when` expression; \
                     the condition stage must prune before an AST is built",
                    r.id
                ),
                pos: r.pos.clone(),
            });
        }

        // A graph error already carries its own position in its text.
        let wrap = |e: meridian_graph::Error| Error {
            msg: e.to_string(),
            pos: Position::default(),
        };
        let graph = Graph::build(resources).map_err(wrap)?;
        let nodes = graph.topo_sort_nodes().map_err(wrap)?;

        let resources = nodes
            .into_iter()
            .map(|n| Resource {
                id: n.resource.id.clone(),
                r#type: n.resource.r#type.clone(),
                state: n.resource.state.clone(),
                params: n.resource.params.clone(),
                runtime_when: n.resource.runtime_when.clone(),
                index: n.index,
                pos: n.resource.pos.clone(),
            })
            .collect();
        let edges = graph
            .edges()
            .iter()
            .map(|e| Edge {
                from: e.from.clone(),
                to: e.to.clone(),
                kind: e.kind.into(),
                action: e.action.clone(),
                pos: e.pos.clone(),
            })
            .collect();
        Ok(ResourceGraph::new(name, host, target, resources, edges))
    }

    /// Assembles a tree from parts that are already decided.
    ///
    /// [`ResourceGraph::build`] is the normal path: it derives the order from
    /// the graph. This exists for the cases where the order is the input, not
    /// the output: an emitter test that must hand a stage an order it would
    /// never be given, and any stage that rewrites a tree it has already
    /// built. The caller owns the order, so nothing about it is checked beyond
    /// resource identity: a resource repeating an earlier id is dropped.
    pub fn new(
        name: &str,
        host: &str,
        target: &str,
        resources: Vec<Resource>,
        mut edges: Vec<Edge>,
    ) -> ResourceGraph {
        let mut kept: Vec<Resource> = Vec::with_capacity(resources.len());
        let mut by_id = HashMap::with_capacity(resources.len());
        for r in resources {
            if !by_id.contains_key(&r.id) {
                by_id.insert(r.id.clone(), kept.len());
                kept.push(r);
            }
        }
        // By source, then kind, then destination and action, so two builds of
        // the same document always list their edges identically. The sort is
        // stable, so edges equal on all four keep their given order.
        edges.sort_by(|a, b| {
            (&a.from, a.kind, &a.to, &a.action).cmp(&(&b.from, b.kind, &b.to, &b.action))
        });
        ResourceGraph {
            name: name.to_owned(),
            host: host.to_owned(),
            target: target.to_owned(),
            target_version: String::new(),
            outputs: Vec::new(),
            credential: None,
            settings: BTreeMap::new(),
            resources: kept,
            by_id,
            edges,
        }
    }

    /// Resolves the document's `targetHost` against the bundle before
    /// building, so an Infrastructure output reaches the tree as a literal.
    pub fn build_from_bundle(bundle: &Bundle, rs: &ResourceSet) -> Result<ResourceGraph, Error> {
        let (_, output) = bundle.resolve_target_host(rs).map_err(|e| Error {
            msg: e.to_string(),
            pos: Position::default(),
        })?;
        ResourceGraph::build(rs, &output.value)
    }

    /// The resources in topological apply order.
    pub fn resources(&self) -> &[Resource] {
        &self.resources
    }

    /// Every edge, ordered deterministically.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Only the edges of one kind, in the same relative order.
    pub fn edges_of_kind(&self, kind: EdgeKind) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.kind == kind)
    }

    /// One resource, by id.
    pub fn resource(&self, id: &str) -> Option<&Resource> {
        self.by_id.get(id).map(|&i| &self.resources[i])
    }

    /// The apply order as a list of resource ids.
    pub fn order(&self) -> Vec<&str> {
        self.resources.iter().map(|r| r.id.as_str()).collect()
    }

    /// Renders the whole tree in a stable textual form, so determinism can be
    /// asserted on the tree itself, independent of any emitter.
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
                "resource {} type={} state={} runtimeWhen={} params={}",
                r.id,
                r.r#type,
                r.state,
                r.runtime_when,
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
