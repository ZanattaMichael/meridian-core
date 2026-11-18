//! Stages: the documents a run selects, in the order their needs allow,
//! and whether a target fits a stage's kind and version.

use super::*;

/// One document, as a stage.
#[derive(Clone, Copy)]
pub(crate) enum Stage<'a> {
    Infrastructure(&'a Infrastructure),
    ResourceSet(&'a ResourceSet),
}

impl Stage<'_> {
    pub(crate) fn id(&self) -> StageId {
        match self {
            Stage::Infrastructure(d) => StageId {
                kind: StageKind::Infrastructure,
                name: d.metadata.name.clone(),
            },
            Stage::ResourceSet(d) => StageId {
                kind: StageKind::ResourceSet,
                name: d.metadata.name.clone(),
            },
        }
    }

    pub(crate) fn target(&self) -> &str {
        match self {
            Stage::Infrastructure(d) => &d.spec.target,
            Stage::ResourceSet(d) => &d.spec.target,
        }
    }

    pub(crate) fn document(&self) -> Document {
        match *self {
            Stage::Infrastructure(d) => Document::Infrastructure(d.clone()),
            Stage::ResourceSet(d) => Document::ResourceSet(d.clone()),
        }
    }

    pub(crate) fn target_version(&self) -> &str {
        match self {
            Stage::Infrastructure(d) => &d.spec.target_version,
            Stage::ResourceSet(d) => &d.spec.target_version,
        }
    }

    pub(crate) fn labels(&self) -> &BTreeMap<String, String> {
        match self {
            Stage::Infrastructure(d) => &d.metadata.labels,
            Stage::ResourceSet(d) => &d.metadata.labels,
        }
    }

    pub(crate) fn imports(&self) -> &[meridian_ir::Import] {
        match self {
            Stage::Infrastructure(d) => &d.spec.imports,
            Stage::ResourceSet(d) => &d.spec.imports,
        }
    }

    /// Whether this stage is `document`.
    fn is(&self, document: &Document) -> bool {
        match (self, document) {
            (Stage::Infrastructure(a), Document::Infrastructure(b)) => {
                a.metadata.name == b.metadata.name
            }
            (Stage::ResourceSet(a), Document::ResourceSet(b)) => a.metadata.name == b.metadata.name,
            _ => false,
        }
    }

    /// A document, as a stage.
    pub(crate) fn of(document: &Document) -> Stage<'_> {
        match document {
            Document::Infrastructure(doc) => Stage::Infrastructure(doc),
            Document::ResourceSet(doc) => Stage::ResourceSet(doc),
        }
    }

    pub(crate) fn in_bundle<'b>(&self, bundle: &'b Bundle) -> Option<&'b Document> {
        bundle.documents.iter().find(|document| self.is(document))
    }
}

/// How many documents of `bundle` a selector picks. A run that picks none
/// does nothing, which a caller will usually want to say out loud.
pub fn selected(bundle: &Bundle, selector: &Selector, data: &Data) -> usize {
    stages(bundle, selector, data).len()
}

/// Whether a document of this kind can be aimed at a target of this
/// category, and if not, why.
///
/// The two kinds answer different questions. An Infrastructure document says
/// what should exist; a ResourceSet says how a machine that exists is
/// configured. A tool that provisions has no machine to be pointed at, and a
/// tool that configures one machine has nothing to provision, so the wrong
/// pairing is refused before the plugin sees it instead of compiling into
/// something that quietly means less than the document says.
///
/// Containerization takes both: a container project either stands on its
/// own (Infrastructure) or runs on the daemon of a host the document names
/// (ResourceSet). A plugin that does not state a category was built before
/// categories existed, and is not second-guessed.
pub fn kind_fits(kind: StageKind, category: Category) -> Result<(), String> {
    let wants = match (kind, category) {
        (StageKind::ResourceSet, Category::InfrastructureAsCode)
        | (StageKind::ResourceSet, Category::ContainerOrchestration) => StageKind::Infrastructure,
        (StageKind::Infrastructure, Category::ConfigurationManagement) => StageKind::ResourceSet,
        _ => return Ok(()),
    };
    let why = match wants {
        StageKind::Infrastructure => {
            "it creates what should exist and is not pointed at one machine, so it takes \
             kind: Infrastructure, without a targetHost"
        }
        StageKind::ResourceSet => {
            "it configures a machine that already exists, so it takes kind: ResourceSet, \
             with a targetHost"
        }
    };
    let (document, tool) = match kind {
        StageKind::ResourceSet => ("a ResourceSet", "an infrastructure-as-code"),
        StageKind::Infrastructure => ("an Infrastructure", "a configuration-management"),
    };
    let tool = match category {
        Category::ContainerOrchestration => "a container-orchestration",
        _ => tool,
    };
    Err(format!(
        "this is {document} document, and its target is {tool} tool: {why}"
    ))
}

/// Whether `asked`, a document's `targetVersion`, is one of the versions a
/// target declares or a more specific version under one of them: `3.1` is
/// under `3`, and `1.10` is not under `1.1`.
pub fn version_fits(asked: &str, declared: &[String]) -> bool {
    declared.iter().any(|have| {
        asked == have
            || asked
                .strip_prefix(have.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Every Infrastructure document, then every ResourceSet, each in the order
/// written. A ResourceSet can only depend on an Infrastructure document, so
/// this order satisfies every dependency a workflow can express.
///
/// A selector matches a document's labels as they are once their
/// interpolations are placed. A document whose labels cannot be decided stays
/// a stage, so that its error is reported instead of the document quietly
/// dropping out of the run.
pub(crate) fn stages<'a>(bundle: &'a Bundle, selector: &Selector, data: &Data) -> Vec<Stage<'a>> {
    let selected = |doc: &&Document| {
        meridian_ir::resolved_labels(doc, data).map_or(true, |labels| selector.matches(&labels))
    };
    let infrastructure = bundle
        .documents
        .iter()
        .filter(selected)
        .filter_map(Document::as_infrastructure)
        .map(Stage::Infrastructure);
    let resource_sets = bundle
        .documents
        .iter()
        .filter(selected)
        .filter_map(Document::as_resource_set)
        .map(Stage::ResourceSet);
    let written: Vec<Stage<'a>> = infrastructure.chain(resource_sets).collect();
    by_need(bundle, written)
}

/// Orders stages so that each comes after every stage it needs: the one its
/// host comes from, and every one it imports from.
///
/// The order given is kept wherever need allows, so a workflow with no
/// imports runs exactly as it was written. A stage that needs a document the
/// run does not include waits on nothing: what it needs is read from what
/// that document's last apply left behind. A cycle cannot be ordered and is
/// refused when the bundle is validated; should one arrive here anyway, its
/// stages are kept in the order given.
pub(crate) fn by_need<'a>(bundle: &'a Bundle, mut waiting: Vec<Stage<'a>>) -> Vec<Stage<'a>> {
    let mut ordered: Vec<Stage<'a>> = Vec::with_capacity(waiting.len());
    while !waiting.is_empty() {
        let ready = waiting.iter().position(|stage| {
            let Some(document) = stage.in_bundle(bundle) else {
                return true;
            };
            !bundle
                .needs(document)
                .iter()
                .any(|need| waiting.iter().any(|other| other.is(need)))
        });
        ordered.push(waiting.remove(ready.unwrap_or(0)));
    }
    ordered
}
