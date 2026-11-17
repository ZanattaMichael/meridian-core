use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, ErrorList, Position};
use crate::parse::parse_file;
use crate::types::*;

/// A set of documents loaded together. Cross-document references
/// (`targetHost.from: infrastructure`) resolve within a bundle and nowhere else.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bundle {
    pub documents: Vec<Document>,
}

/// Parses every path and returns the combined bundle. Parse failures from all
/// files are reported together, not just those of the first bad file.
pub fn load<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> Result<Bundle, ErrorList> {
    let mut documents = Vec::new();
    let mut errs = ErrorList::new();
    for path in paths {
        match parse_file(path) {
            Ok(parsed) => documents.extend(parsed),
            Err(e) => errs.extend(e),
        }
    }
    errs.finish(Bundle { documents })
}

/// Checks a single document in isolation. Cross-document references are not
/// checked here; use [`Bundle::validate`] for that.
pub fn validate(doc: &Document) -> Result<(), ErrorList> {
    let mut errs = ErrorList::new();
    validate_document(doc, &mut errs);
    errs.finish(())
}

impl Bundle {
    /// Groups already-parsed documents. Performs no validation.
    pub fn new(documents: impl IntoIterator<Item = Document>) -> Self {
        Bundle {
            documents: documents.into_iter().collect(),
        }
    }

    /// The one warning this run owes for documents written in a deprecated
    /// language version, or `None` when there are none.
    pub fn deprecation_warning(&self) -> Option<String> {
        crate::api::VersionTable::builtin().deprecation_warning(
            self.documents
                .iter()
                .map(|doc| (doc.api_version(), doc.name())),
        )
    }

    /// The Infrastructure document with the given `metadata.name`.
    pub fn infrastructure(&self, name: &str) -> Option<&Infrastructure> {
        self.documents
            .iter()
            .filter_map(Document::as_infrastructure)
            .find(|d| d.metadata.name == name)
    }

    /// The ResourceSet document with the given `metadata.name`.
    pub fn resource_set(&self, name: &str) -> Option<&ResourceSet> {
        self.documents
            .iter()
            .filter_map(Document::as_resource_set)
            .find(|d| d.metadata.name == name)
    }

    /// Checks every document in the bundle plus the cross-document references
    /// between them, reporting all diagnostics at once.
    pub fn validate(&self) -> Result<(), ErrorList> {
        let mut errs = ErrorList::new();
        let mut seen: HashMap<(Kind, &str), &Position> = HashMap::new();
        for doc in &self.documents {
            match seen.get(&(doc.kind(), doc.name())) {
                Some(first) => errs.push(Error::new(
                    doc.position().clone(),
                    "metadata.name",
                    format!(
                        "duplicate {} document name {:?} (first declared at {first})",
                        doc.kind(),
                        doc.name()
                    ),
                )),
                None => {
                    seen.insert((doc.kind(), doc.name()), doc.position());
                }
            }
            validate_document(doc, &mut errs);
        }
        for rs in self.documents.iter().filter_map(Document::as_resource_set) {
            self.validate_target_host(rs, &mut errs);
        }
        for doc in &self.documents {
            self.validate_imports(doc, &mut errs);
        }
        if errs.is_empty() {
            // Only worth saying once every reference is known to resolve.
            self.validate_no_cycle(&mut errs);
        }
        errs.finish(())
    }

    /// Every document named `name`, of either kind.
    pub fn named(&self, name: &str) -> Vec<&Document> {
        self.documents.iter().filter(|d| d.name() == name).collect()
    }

    /// The document an import's `ref` names: an Infrastructure document for
    /// an output, and for an artifact whichever document has the name.
    pub fn import_source(&self, import: &Import) -> Option<&Document> {
        if import.from == IMPORT_FROM_OUTPUT {
            return self
                .documents
                .iter()
                .find(|d| d.kind() == Kind::Infrastructure && d.name() == import.r#ref);
        }
        match self.named(&import.r#ref).as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Checks a document's `spec.imports`: that each is well formed, and
    /// that the document and the file or output it names exist.
    fn validate_imports(&self, doc: &Document, errs: &mut ErrorList) {
        let mut names: HashMap<&str, &Position> = HashMap::new();
        for (i, import) in doc.imports().iter().enumerate() {
            let path = format!("spec.imports[{i}]");
            let mut fail = |field: &str, msg: String| {
                errs.push(Error::new(
                    import.pos.clone(),
                    format!("{path}.{field}"),
                    msg,
                ))
            };
            if import.name.is_empty() {
                fail("name", "missing required field".into());
            } else if !is_variable_name(&import.name) {
                fail(
                    "name",
                    format!(
                        "{:?} cannot name an import: it becomes the name of the tool's variable, \
                         so it must start with a letter or an underscore and hold only letters, \
                         digits and underscores",
                        import.name
                    ),
                );
            } else if let Some(first) = names.get(import.name.as_str()) {
                fail(
                    "name",
                    format!(
                        "duplicate import name {:?} (first declared at {first})",
                        import.name
                    ),
                );
            } else {
                names.insert(&import.name, &import.pos);
            }

            let (needs, other) = match import.from.as_str() {
                "" => {
                    fail("from", "missing required field".into());
                    continue;
                }
                IMPORT_FROM_ARTIFACT => (("file", &import.file), ("output", &import.output)),
                IMPORT_FROM_OUTPUT => (("output", &import.output), ("file", &import.file)),
                unknown => {
                    fail(
                        "from",
                        format!(
                            "unknown source {unknown:?} (expected {IMPORT_FROM_ARTIFACT:?} or \
                             {IMPORT_FROM_OUTPUT:?})"
                        ),
                    );
                    continue;
                }
            };
            if needs.1.is_empty() {
                fail(
                    needs.0,
                    format!("missing required field for from: {}", import.from),
                );
            }
            if !other.1.is_empty() {
                fail(
                    other.0,
                    format!(
                        "does not apply to from: {}, which takes `{}`",
                        import.from, needs.0
                    ),
                );
            }
            if import.r#ref.is_empty() {
                fail("ref", "missing required field".into());
                continue;
            }

            let named = self.named(&import.r#ref);
            if import.from == IMPORT_FROM_OUTPUT {
                let Some(source) = self.infrastructure(&import.r#ref) else {
                    fail(
                        "ref",
                        if named.is_empty() {
                            format!(
                                "no Infrastructure document named {:?} is loaded",
                                import.r#ref
                            )
                        } else {
                            format!(
                                "{:?} is a ResourceSet, and only an Infrastructure document \
                                 declares outputs",
                                import.r#ref
                            )
                        },
                    );
                    continue;
                };
                if !import.output.is_empty() && source.output(&import.output).is_none() {
                    fail(
                        "output",
                        format!(
                            "Infrastructure {:?} declares no output {:?}",
                            import.r#ref, import.output
                        ),
                    );
                }
                if doc.kind() == Kind::Infrastructure && doc.name() == import.r#ref {
                    fail("ref", "a document cannot import from itself".into());
                }
                continue;
            }
            match named.as_slice() {
                [] => fail(
                    "ref",
                    format!("no document named {:?} is loaded", import.r#ref),
                ),
                [source] => {
                    if source.kind() == doc.kind() && source.name() == doc.name() {
                        fail("ref", "a document cannot import from itself".into());
                    }
                }
                _ => fail(
                    "ref",
                    format!(
                        "both an Infrastructure and a ResourceSet document are named {:?}, so \
                         which one's artifact is meant cannot be told; rename one of them",
                        import.r#ref
                    ),
                ),
            }
        }
    }

    /// What `doc` cannot be applied before: the documents it imports from,
    /// and the Infrastructure document its host comes from.
    pub fn needs(&self, doc: &Document) -> Vec<&Document> {
        let mut found: Vec<Option<&Document>> = Vec::new();
        if let Document::ResourceSet(rs) = doc {
            if let Some(th) = &rs.spec.target_host {
                if th.from == TARGET_HOST_FROM_INFRASTRUCTURE {
                    found.push(
                        self.documents
                            .iter()
                            .find(|d| d.kind() == Kind::Infrastructure && d.name() == th.r#ref),
                    );
                }
            }
        }
        for import in doc.imports() {
            found.push(self.import_source(import));
        }
        let mut needs: Vec<&Document> = Vec::new();
        for source in found.into_iter().flatten() {
            let same = |d: &&Document| d.kind() == source.kind() && d.name() == source.name();
            if !needs.iter().any(same) {
                needs.push(source);
            }
        }
        needs
    }

    /// Refuses documents that wait on each other, which no order of
    /// applying can satisfy.
    fn validate_no_cycle(&self, errs: &mut ErrorList) {
        // 0 unvisited, 1 on the current path, 2 done.
        let mut state = vec![0u8; self.documents.len()];
        let index_of = |doc: &Document| {
            self.documents
                .iter()
                .position(|d| d.kind() == doc.kind() && d.name() == doc.name())
        };
        fn visit<'a>(
            bundle: &'a Bundle,
            at: usize,
            state: &mut [u8],
            path: &mut Vec<usize>,
            index_of: &dyn Fn(&Document) -> Option<usize>,
        ) -> Option<Vec<usize>> {
            state[at] = 1;
            path.push(at);
            for need in bundle.needs(&bundle.documents[at]) {
                let Some(next) = index_of(need) else { continue };
                match state[next] {
                    1 => {
                        let start = path.iter().position(|p| *p == next).unwrap_or(0);
                        let mut cycle = path[start..].to_vec();
                        cycle.push(next);
                        return Some(cycle);
                    }
                    0 => {
                        if let Some(cycle) = visit(bundle, next, state, path, index_of) {
                            return Some(cycle);
                        }
                    }
                    _ => {}
                }
            }
            path.pop();
            state[at] = 2;
            None
        }
        for start in 0..self.documents.len() {
            if state[start] != 0 {
                continue;
            }
            let mut path = Vec::new();
            if let Some(cycle) = visit(self, start, &mut state, &mut path, &index_of) {
                let names: Vec<String> = cycle
                    .iter()
                    .map(|i| {
                        let doc = &self.documents[*i];
                        format!("{} {:?}", doc.kind(), doc.name())
                    })
                    .collect();
                let first = &self.documents[cycle[0]];
                errs.push(Error::new(
                    first.position().clone(),
                    "spec",
                    format!(
                        "these documents wait on each other, so none of them can be applied \
                         first: {}. A document waits on the one its targetHost comes from and \
                         on every one it imports from",
                        names.join(" needs ")
                    ),
                ));
                return;
            }
        }
    }

    /// Checks a ResourceSet's binding to the node it configures, including the
    /// lookup into a sibling Infrastructure document.
    fn validate_target_host(&self, rs: &ResourceSet, errs: &mut ErrorList) {
        let Some(th) = &rs.spec.target_host else {
            return;
        };
        let mut fail = |path: &str, msg: String| errs.push(Error::new(th.pos.clone(), path, msg));
        match th.from.as_str() {
            "" => fail("spec.targetHost.from", "missing required field".into()),
            TARGET_HOST_FROM_INLINE => {
                if th.host.is_empty() {
                    fail("spec.targetHost.host", "missing required field for from: inline".into());
                }
            }
            TARGET_HOST_FROM_INFRASTRUCTURE => {
                if th.r#ref.is_empty() {
                    fail("spec.targetHost.ref", "missing required field for from: infrastructure".into());
                }
                if th.output.is_empty() {
                    fail("spec.targetHost.output", "missing required field for from: infrastructure".into());
                }
                if th.r#ref.is_empty() || th.output.is_empty() {
                    return;
                }
                if let Err(e) = self.lookup_target_host(th) {
                    errs.push(e);
                }
            }
            other => fail(
                "spec.targetHost.from",
                format!(
                    "unknown source {other:?} (expected {TARGET_HOST_FROM_INFRASTRUCTURE:?} or {TARGET_HOST_FROM_INLINE:?})"
                ),
            ),
        }
    }

    fn lookup_target_host(&self, th: &TargetHost) -> Result<(&Infrastructure, &Output), Error> {
        let Some(infra) = self.infrastructure(&th.r#ref) else {
            return Err(Error::new(
                th.pos.clone(),
                "spec.targetHost.ref",
                format!("no Infrastructure document named {:?} is loaded", th.r#ref),
            ));
        };
        let Some(output) = infra.output(&th.output) else {
            return Err(Error::new(
                th.pos.clone(),
                "spec.targetHost.output",
                format!(
                    "Infrastructure {:?} declares no output {:?}",
                    th.r#ref, th.output
                ),
            ));
        };
        Ok((infra, output))
    }

    /// The Infrastructure document and output a ResourceSet's `targetHost`
    /// refers to.
    pub fn resolve_target_host(
        &self,
        rs: &ResourceSet,
    ) -> Result<(&Infrastructure, &Output), Error> {
        match &rs.spec.target_host {
            Some(th) if th.from == TARGET_HOST_FROM_INFRASTRUCTURE => self.lookup_target_host(th),
            _ => Err(Error::new(
                rs.pos.clone(),
                "spec.targetHost",
                format!(
                    "ResourceSet {:?} does not reference an Infrastructure document",
                    rs.metadata.name
                ),
            )),
        }
    }
}

/// Whether `name` can be a tool's variable: a letter or an underscore, then
/// letters, digits and underscores.
fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The longest `metadata.name` accepted: it becomes one path segment, and
/// every common file system takes 255 bytes in one.
pub const MAX_DOCUMENT_NAME: usize = 128;

/// Whether `name` can be `metadata.name`: one path segment that stays where
/// it is put. Letters, digits, `.`, `_` and `-`, starting with a letter or a
/// digit, at most [`MAX_DOCUMENT_NAME`] bytes. The name becomes a directory
/// under `--out` and under the state directory, so `/`, `\`, `..`, a leading
/// `.` and anything a shell or a tool would read specially are all refused.
pub fn is_document_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && name.len() <= MAX_DOCUMENT_NAME
}

fn validate_document(doc: &Document, errs: &mut ErrorList) {
    let pos = doc.position();
    let mut fail =
        |pos: &Position, path: String, msg: String| errs.push(Error::new(pos.clone(), path, msg));

    if doc.name().is_empty() {
        fail(pos, "metadata.name".into(), "missing required field".into());
    } else if !is_document_name(doc.name()) {
        fail(
            pos,
            "metadata.name".into(),
            format!(
                "{:?} cannot name a document: it becomes a directory under the output and \
                 state directories, so it must start with a letter or a digit, hold only \
                 letters, digits, `.`, `_` and `-`, and be at most {MAX_DOCUMENT_NAME} \
                 characters",
                doc.name()
            ),
        );
    }
    if doc.target().is_empty() {
        fail(pos, "spec.target".into(), "missing required field".into());
    }
    let resources = doc.resources();
    if resources.is_empty() {
        fail(
            pos,
            "spec.resources".into(),
            "at least one resource is required".into(),
        );
    }

    let mut declared: HashMap<&str, &Position> = HashMap::with_capacity(resources.len());
    for (i, r) in resources.iter().enumerate() {
        let path = format!("spec.resources[{i}]");
        if r.id.is_empty() {
            fail(
                &r.pos,
                format!("{path}.id"),
                "missing required field".into(),
            );
        } else if let Some(first) = declared.get(r.id.as_str()) {
            fail(
                &r.pos,
                format!("{path}.id"),
                format!(
                    "duplicate resource id {:?} (first declared at {first})",
                    r.id
                ),
            );
        } else {
            declared.insert(&r.id, &r.pos);
        }
        if r.r#type.is_empty() {
            fail(
                &r.pos,
                format!("{path}.type"),
                "missing required field".into(),
            );
        }
    }

    for (i, r) in resources.iter().enumerate() {
        let path = format!("spec.resources[{i}]");
        for (j, dep) in r.depends_on.iter().enumerate() {
            let p = format!("{path}.dependsOn[{j}]");
            check_if_missing(&dep.if_missing, &dep.pos, &p, &mut fail);
            if dep.resource.is_empty() {
                fail(
                    &dep.pos,
                    format!("{p}.resource"),
                    "missing required field".into(),
                );
            } else if dep.resource == r.id {
                fail(
                    &dep.pos,
                    format!("{p}.resource"),
                    format!("resource {:?} cannot depend on itself", r.id),
                );
            } else if !declared.contains_key(dep.resource.as_str()) {
                fail(
                    &dep.pos,
                    format!("{p}.resource"),
                    format!(
                        "resource {:?} depends on unknown resource {:?}",
                        r.id, dep.resource
                    ),
                );
            }
        }
        for (j, n) in r.notifies.iter().enumerate() {
            let p = format!("{path}.notifies[{j}]");
            check_if_missing(&n.if_missing, &n.pos, &p, &mut fail);
            if !n.on.is_empty() && n.on != NOTIFY_ON_CHANGED {
                fail(
                    &n.pos,
                    format!("{p}.on"),
                    format!(
                        "unknown event {:?} (expected {NOTIFY_ON_CHANGED:?}, which is also \
                         what leaving `on` out means: a notify fires when the resource \
                         really changes, and no target can fire one on anything else)",
                        n.on
                    ),
                );
            }
            if n.action.is_empty() {
                fail(
                    &n.pos,
                    format!("{p}.action"),
                    "missing required field".into(),
                );
            }
            if n.resource.is_empty() {
                fail(
                    &n.pos,
                    format!("{p}.resource"),
                    "missing required field".into(),
                );
            } else if n.resource == r.id {
                fail(
                    &n.pos,
                    format!("{p}.resource"),
                    format!("resource {:?} cannot notify itself", r.id),
                );
            } else if !declared.contains_key(n.resource.as_str()) {
                fail(
                    &n.pos,
                    format!("{p}.resource"),
                    format!(
                        "resource {:?} notifies unknown resource {:?}",
                        r.id, n.resource
                    ),
                );
            }
        }
    }

    if let Some((at, cycle)) = dependency_cycle(resources) {
        fail(
            &resources[at].pos,
            format!("spec.resources[{at}].dependsOn"),
            format!(
                "dependency cycle among {} resources: {}",
                cycle.len() - 1,
                cycle.join(" -> ")
            ),
        );
    }

    if let Document::Infrastructure(infra) = doc {
        let mut names: HashMap<&str, &Position> = HashMap::with_capacity(infra.spec.outputs.len());
        for (i, o) in infra.spec.outputs.iter().enumerate() {
            let p = format!("spec.outputs[{i}]");
            if o.name.is_empty() {
                fail(&o.pos, format!("{p}.name"), "missing required field".into());
            } else if let Some(first) = names.get(o.name.as_str()) {
                fail(
                    &o.pos,
                    format!("{p}.name"),
                    format!(
                        "duplicate output name {:?} (first declared at {first})",
                        o.name
                    ),
                );
            } else {
                names.insert(&o.name, &o.pos);
            }
        }
    }

    crate::resolve::check(doc, errs);
}

fn check_if_missing(
    policy: &IfMissing,
    pos: &Position,
    path: &str,
    fail: &mut impl FnMut(&Position, String, String),
) {
    if let IfMissing::Unknown(v) = policy {
        fail(
            pos,
            format!("{path}.ifMissing"),
            format!("unknown policy {v:?} (expected \"error\" or \"skip\")"),
        );
    }
}

/// Finds one dependency cycle among a document's resources: the index of the
/// resource it is reported at, and the ids along it, closing on the id it
/// opened with and read in apply order (each resource is waited for by the
/// next).
///
/// Only `dependsOn` counts. A notify is a deferred trigger, not an ordering
/// constraint, and a notification loop is legal. Every resource is
/// considered, whatever its `when` says: an order that only works while one
/// condition happens to be false is still a mistake in the document.
///
/// The walk follows declaration order, so the same document always reports
/// the same cycle, and it keeps its own stack, because a chain of
/// dependencies can be as long as the document.
fn dependency_cycle(resources: &[Resource]) -> Option<(usize, Vec<String>)> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        OnPath,
        Done,
    }
    let mut by_id: HashMap<&str, usize> = HashMap::with_capacity(resources.len());
    for (i, r) in resources.iter().enumerate() {
        // The first declaration wins; a duplicate id is reported on its own.
        by_id.entry(r.id.as_str()).or_insert(i);
    }
    // Per resource, the resources that depend on it, in declaration order.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); resources.len()];
    for (to, r) in resources.iter().enumerate() {
        for dep in &r.depends_on {
            match by_id.get(dep.resource.as_str()) {
                // A resource depending on itself has its own diagnostic.
                Some(&from) if from != to => dependents[from].push(to),
                _ => {}
            }
        }
    }

    let mut state = vec![State::Unvisited; resources.len()];
    for start in 0..resources.len() {
        if state[start] != State::Unvisited {
            continue;
        }
        let mut path: Vec<(usize, usize)> = vec![(start, 0)];
        state[start] = State::OnPath;
        while let Some(&mut (node, ref mut followed)) = path.last_mut() {
            let Some(&next) = dependents[node].get(*followed) else {
                state[node] = State::Done;
                path.pop();
                continue;
            };
            *followed += 1;
            match state[next] {
                State::OnPath => {
                    let opens = path.iter().position(|&(n, _)| n == next).unwrap_or(0);
                    let mut cycle: Vec<String> = path[opens..]
                        .iter()
                        .map(|&(n, _)| resources[n].id.clone())
                        .collect();
                    cycle.push(resources[next].id.clone());
                    return Some((next, cycle));
                }
                State::Unvisited => {
                    state[next] = State::OnPath;
                    path.push((next, 0));
                }
                State::Done => {}
            }
        }
    }
    None
}
