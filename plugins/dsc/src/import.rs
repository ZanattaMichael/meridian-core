//! Importing an existing DSC configuration.
//!
//! DSC configurations are written three ways, and this reads all three:
//!
//! - a PowerShell `Configuration` block, which is what DSC v1 and v2 are
//!   written in. Only what is declared with literals can be read; the rest
//!   of the script is a program, and is reported (see [`crate::powershell`]);
//! - a DSC v3 configuration document, in YAML or JSON;
//! - a DscPipelineRunner project, which is what this target compiles to, so
//!   it is the target's own mapping read backwards.
//!
//! In each, a resource instance becomes a resource whose type is
//! `Module/Resource` and whose params are the instance's properties.
//! `DependsOn` becomes `dependsOn`, the runner's `notify` becomes `notifies`
//! with the action `refresh`, and its `preCondition` becomes `runtimeWhen`.

use std::collections::{BTreeMap, BTreeSet};

use meridian_import::yaml::{self, Node, Rules};
use meridian_import::{literal, slug, unique_id, Notes};
use meridian_sdk::{
    DocumentKind, ImportError, ImportFormat, ImportSource, Imported, ImportedDocument,
    ImportedNotify, ImportedResource, Importer, Value,
};

use crate::powershell;
use crate::NAME;

/// Reads DSC configurations into documents for this target.
pub struct DscImporter;

impl Importer for DscImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        vec![
            ImportFormat::new("configuration", "PowerShell DSC configurations", &["*.ps1"]),
            ImportFormat::new(
                "document",
                "DSC v3 configuration documents and DscPipelineRunner projects",
                &[
                    "*.dsc.yaml",
                    "*.dsc.yml",
                    "*.dsc.json",
                    "*.dsc.config.yaml",
                    "*.dsc.config.yml",
                    "*.dsc.config.json",
                    "Datum.yml",
                    "*.yaml",
                    "*.yml",
                    "*.json",
                ],
            ),
        ]
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        let mut notes = Notes::new();
        let mut drafts: Vec<Draft> = Vec::new();
        // A runner project's settings are in a file of their own.
        let mut settings: Option<Settings> = None;

        let mut parsed = Vec::new();
        for (file, text) in &source.files {
            if file.to_ascii_lowercase().ends_with(".ps1") {
                let script = powershell::parse(text)
                    .map_err(|(line, msg)| ImportError::at(file, line, msg))?;
                from_script(file, &script, &mut drafts, &mut notes);
            } else {
                parsed.push((file, yaml::read(file, text, Rules::Yaml12)?));
            }
        }
        for (file, documents) in &parsed {
            for document in documents {
                if let Some(runner) = document.get("PipelineRunnerSettings") {
                    settings = Some(Settings {
                        engine: text_at(runner, "Engine"),
                        target: text_at(runner, "Target"),
                    });
                } else if document.get("resources").and_then(Node::as_seq).is_some() {
                    drafts.push(from_document(file, document));
                } else {
                    notes.untranslated(
                        &format!("{file}:{}", document.line),
                        "file",
                        "",
                        "this holds no `resources` list, so it is not a DSC configuration \
                         document; it was left out",
                    );
                }
            }
        }
        if drafts.is_empty() {
            return Err(ImportError::new(
                "there is no DSC configuration to import: no file holds a PowerShell \
                 Configuration block or a `resources` list",
            ));
        }

        let several = drafts.len() > 1;
        let mut names = BTreeSet::new();
        let mut documents = Vec::new();
        let mut all = notes.into_vec();
        for mut draft in drafts {
            let own = slug(&draft.name, '-');
            let base = match (source.name.is_empty(), several) {
                (false, false) => source.name.clone(),
                (false, true) => format!("{}-{own}", source.name),
                (true, _) => own,
            };
            let mut name = base.clone();
            let mut n = 2;
            while !names.insert(name.clone()) {
                name = format!("{base}-{n}");
                n += 1;
            }
            if let (Some(settings), Format::Runner) = (&settings, draft.format) {
                draft.apply(settings);
            }
            let mut notes = std::mem::take(&mut draft.notes);
            let document = draft.finish(name.clone(), &mut notes);
            // The notes were raised before the document had its name.
            all.extend(notes.into_vec().into_iter().map(|mut note| {
                note.document = name.clone();
                note
            }));
            documents.push(document);
        }
        Ok(Imported {
            documents,
            notes: all,
        })
    }
}

fn text_at(node: &Node, key: &str) -> String {
    node.get(key)
        .and_then(Node::scalar_text)
        .unwrap_or_default()
}

/// A runner project's own settings.
struct Settings {
    engine: String,
    target: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    PowerShell,
    V3,
    Runner,
}

/// One instance as read, before ids are final.
struct Instance {
    origin: String,
    /// The name it has in the configuration.
    name: String,
    r#type: String,
    params: BTreeMap<String, Value>,
    /// The instances it depends on and notifies, each as the configuration
    /// names it and as a `(type, name)` pair when it could be read as one.
    depends_on: Vec<(String, Option<(String, String)>)>,
    notify: Vec<(String, Option<(String, String)>)>,
    runtime_when: String,
}

/// A document being assembled.
struct Draft {
    format: Format,
    name: String,
    origin: String,
    host: String,
    /// The configuration is for a machine it does not name with a literal.
    unnamed_host: bool,
    engine: String,
    /// How the runner reaches the host, and the credential it uses.
    target: String,
    credential: String,
    instances: Vec<Instance>,
    notes: Notes,
}

impl Draft {
    fn new(format: Format, name: &str, origin: String) -> Self {
        Draft {
            format,
            name: name.to_owned(),
            origin,
            host: String::new(),
            unnamed_host: false,
            engine: String::new(),
            target: String::new(),
            credential: String::new(),
            instances: Vec::new(),
            notes: Notes::new(),
        }
    }

    fn apply(&mut self, settings: &Settings) {
        if self.engine.is_empty() {
            self.engine = settings.engine.clone();
        }
        if self.target.is_empty() {
            self.target = settings.target.clone();
        }
    }

    fn finish(self, name: String, notes: &mut Notes) -> ImportedDocument {
        let subject = "configuration";
        // Ids first, so a dependency can be written as one.
        let mut taken = BTreeSet::from(["runner".to_owned()]);
        let mut ids = Vec::new();
        for instance in &self.instances {
            let id = unique_id(&id_for(&instance.name), &mut taken);
            if id != instance.name {
                notes.info(
                    &instance.origin,
                    &format!("{} {:?}", instance.r#type, instance.name),
                    &id,
                    format!(
                        "a resource's id is letters, digits and underscores, and it is the \
                         instance's name in the compiled configuration, so {:?} became {id:?}",
                        instance.name
                    ),
                );
            }
            ids.push(id);
        }
        let find = |wanted: &Option<(String, String)>| -> Option<String> {
            let (r#type, name) = wanted.as_ref()?;
            self.instances
                .iter()
                .position(|i| {
                    i.name.eq_ignore_ascii_case(name)
                        && (i.r#type.eq_ignore_ascii_case(r#type)
                            || i.r#type
                                .rsplit_once('/')
                                .is_some_and(|(_, short)| short.eq_ignore_ascii_case(r#type)))
                })
                .map(|index| ids[index].clone())
        };

        let mut resources = Vec::new();
        // A runner resource says what a document alone cannot: the engine,
        // when it is not a targetVersion, and how the host is reached.
        let mut target_version = String::new();
        let mut runner = BTreeMap::new();
        match self.engine.as_str() {
            "DscV2" => target_version = "2".to_owned(),
            "DscV3" => target_version = "3".to_owned(),
            _ => {}
        }
        let local = !self.unnamed_host
            && matches!(self.host.as_str(), "" | "localhost" | "127.0.0.1" | "::1");
        let host = if self.unnamed_host {
            // Left empty, which the written document marks.
            String::new()
        } else if self.host.is_empty() {
            notes.info(
                &self.origin,
                subject,
                "",
                "the configuration names no machine, so the document configures localhost: \
                 the machine Meridian runs on",
            );
            "localhost".to_owned()
        } else {
            self.host.clone()
        };
        if !self.target.is_empty() && !(local && self.target == "Local") {
            runner.insert("target".to_owned(), Value::String(self.target.clone()));
        } else if !local {
            notes.changed(
                &self.origin,
                subject,
                "runner",
                format!(
                    "the configuration does not say how {} is reached. The runner was \
                     given target: WinRM, which is how a PowerShell configuration is pushed; \
                     change it to SSH if that is how this machine is managed",
                    if host.is_empty() {
                        "its machine".to_owned()
                    } else {
                        format!("{host:?}")
                    }
                ),
            );
            runner.insert("target".to_owned(), Value::String("WinRM".to_owned()));
        }
        if !self.credential.is_empty() {
            runner.insert(
                "credential".to_owned(),
                Value::String(self.credential.clone()),
            );
        }
        if !runner.is_empty() {
            resources.push(ImportedResource {
                id: "runner".to_owned(),
                r#type: "runner".to_owned(),
                params: runner,
                origin: self.origin.clone(),
                ..ImportedResource::default()
            });
        }

        for (instance, id) in self.instances.iter().zip(&ids) {
            let subject = format!("{} {:?}", instance.r#type, instance.name);
            let mut resource = ImportedResource {
                id: id.clone(),
                r#type: instance.r#type.clone(),
                params: instance.params.clone(),
                runtime_when: instance.runtime_when.replace("${", "$${"),
                origin: instance.origin.clone(),
                ..ImportedResource::default()
            };
            for (written, wanted) in &instance.depends_on {
                match find(wanted) {
                    Some(other) if other != *id => {
                        if !resource.depends_on.contains(&other) {
                            resource.depends_on.push(other);
                        }
                    }
                    _ => notes.untranslated(
                        &instance.origin,
                        &subject,
                        id,
                        format!(
                            "it depends on {written}, which is not a resource of this \
                             configuration, so the dependency was left out"
                        ),
                    ),
                }
            }
            for (written, wanted) in &instance.notify {
                match find(wanted) {
                    Some(other) if other != *id => resource.notifies.push(ImportedNotify {
                        resource: other,
                        action: "refresh".to_owned(),
                    }),
                    _ => notes.untranslated(
                        &instance.origin,
                        &subject,
                        id,
                        format!(
                            "it notifies {written}, which is not a resource of this \
                             configuration, so the notification was left out"
                        ),
                    ),
                }
            }
            resources.push(resource);
        }

        ImportedDocument {
            kind: DocumentKind::ResourceSet,
            name,
            target: NAME.to_owned(),
            target_version,
            host,
            resources,
            ..ImportedDocument::default()
        }
    }
}

/// The id an instance gets: its own name where that can be one, which a
/// name in a PowerShell configuration always can.
fn id_for(name: &str) -> String {
    let fits = name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if fits {
        name.to_owned()
    } else {
        slug(name, '_')
    }
}

/// Whether a type is `Module/Resource`, as the compiler requires.
fn is_dsc_type(t: &str) -> bool {
    let part = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    };
    t.split_once('/')
        .is_some_and(|(module, resource)| part(module) && part(resource))
}

// ---------------------------------------------------------------------------
// YAML and JSON: a DSC v3 document, or a runner project's node.

/// The properties every instance of a v3 document or a runner node may have.
const INSTANCE_KEYS: &[&str] = &[
    "name",
    "type",
    "properties",
    "dependsOn",
    "notify",
    "preCondition",
    "target",
];

fn from_document(file: &str, document: &Node) -> Draft {
    let origin = format!("{file}:{}", document.line);
    let is_v3 = document.get("$schema").is_some();
    let stem = file
        .rsplit('/')
        .next()
        .unwrap_or(file)
        .split('.')
        .next()
        .unwrap_or("imported");
    let mut draft = Draft::new(
        if is_v3 { Format::V3 } else { Format::Runner },
        stem,
        origin.clone(),
    );
    if is_v3 {
        draft.engine = "DscV3".to_owned();
    }

    // What the document holds besides its resources.
    let mut lost = Vec::new();
    for (key, value) in document.as_map().unwrap_or_default() {
        let empty = match &value.kind {
            yaml::Kind::Null => true,
            yaml::Kind::Map(entries) => entries.is_empty(),
            yaml::Kind::Seq(items) => items.is_empty(),
            _ => false,
        };
        if !matches!(key.as_str(), "resources" | "$schema") && !empty {
            lost.push(key.as_str());
        }
    }
    if !lost.is_empty() {
        draft.notes.untranslated(
            &origin,
            "configuration",
            "",
            format!(
                "a document has nothing for these parts of the configuration, so they were \
                 left out: {}. A resource that reads a parameter or a variable is marked",
                lost.join(", ")
            ),
        );
    }

    let mut targets: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut untargeted = false;
    for node in document
        .get("resources")
        .and_then(Node::as_seq)
        .unwrap_or_default()
    {
        let origin = format!("{file}:{}", node.line);
        let name = text_at(node, "name");
        let r#type = text_at(node, "type");
        let subject = format!("{type} {name:?}");
        if name.is_empty() || !is_dsc_type(&r#type) {
            draft.notes.untranslated(
                &origin,
                &subject,
                "",
                "an instance needs a name and a type written `Module/Resource`; this one \
                 was left out",
            );
            continue;
        }
        let mut instance = Instance {
            origin: origin.clone(),
            name,
            r#type,
            params: BTreeMap::new(),
            depends_on: Vec::new(),
            notify: Vec::new(),
            runtime_when: text_at(node, "preCondition"),
        };

        let mut expressions = Vec::new();
        if let Some(Value::Map(properties)) = node.get("properties").map(Node::value) {
            for (key, value) in properties {
                let value = if is_v3 {
                    v3_value(&value, &key, &mut expressions)
                } else {
                    runner_value(&value, &key, &mut expressions)
                };
                instance.params.insert(key, value);
            }
        }
        if !expressions.is_empty() {
            draft.notes.untranslated(
                &origin,
                &subject,
                &id_for(&instance.name),
                format!(
                    "{} computed when the configuration runs, and a document can only read \
                     a property of another resource. The expression was carried as the text \
                     it is, which is not what it meant: {}",
                    if expressions.len() == 1 {
                        "this property is"
                    } else {
                        "these properties are"
                    },
                    expressions.join(", ")
                ),
            );
        }

        for (key, list) in [
            ("dependsOn", &mut instance.depends_on),
            ("notify", &mut instance.notify),
        ] {
            for item in node.get(key).and_then(Node::as_seq).unwrap_or_default() {
                let written = item.scalar_text().unwrap_or_default();
                let wanted = if is_v3 {
                    resource_id(&written)
                } else {
                    // `Module/Resource/name`.
                    written
                        .rsplit_once('/')
                        .map(|(t, n)| (t.to_owned(), n.to_owned()))
                };
                list.push((written, wanted));
            }
        }

        match node.get("target") {
            Some(target) => {
                targets.insert((
                    text_at(target, "action"),
                    text_at(target, "computerName"),
                    text_at(target, "credentialRef"),
                ));
            }
            None => untargeted = true,
        }
        let unknown: Vec<&str> = node
            .as_map()
            .unwrap_or_default()
            .iter()
            .map(|(key, _)| key.as_str())
            .filter(|key| !INSTANCE_KEYS.contains(key))
            .collect();
        if !unknown.is_empty() {
            draft.notes.untranslated(
                &origin,
                &subject,
                &id_for(&instance.name),
                format!(
                    "a resource has nothing for these keys of the instance, so they were \
                     left out: {}",
                    unknown.join(", ")
                ),
            );
        }
        draft.instances.push(instance);
    }

    // A document is run one way on one host.
    let mut targets = targets.into_iter();
    match (targets.next(), targets.next(), untargeted) {
        (Some((action, computer, credential)), None, false) => {
            draft.target = action;
            draft.host = computer;
            draft.credential = credential;
        }
        (None, _, _) => {}
        _ => draft.notes.untranslated(
            &origin,
            "configuration",
            "",
            "the instances are not all run on the same machine in the same way, and a \
             document configures one host. The `target` of each instance was left out: \
             write one document for each machine",
        ),
    }
    draft
}

/// `[resourceId('Module/Resource', 'name')]`.
fn resource_id(text: &str) -> Option<(String, String)> {
    let inner = text
        .trim()
        .strip_prefix("[resourceId(")?
        .strip_suffix(")]")?;
    let (r#type, name) = inner.split_once(',')?;
    let unquote = |s: &str| s.trim().trim_matches(['\'', '"']).to_owned();
    Some((unquote(r#type), unquote(name)))
}

/// A property of a v3 document. A string in square brackets is an expression
/// DSC evaluates, and `[[` is how a literal bracket is written.
fn v3_value(value: &Value, key: &str, expressions: &mut Vec<String>) -> Value {
    match value {
        Value::String(text) => {
            if let Some(rest) = text.strip_prefix("[[") {
                return literal(&Value::String(format!("[{rest}")));
            }
            if text.starts_with('[') && text.ends_with(']') {
                expressions.push(key.to_owned());
            }
            literal(value)
        }
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| v3_value(item, key, expressions))
                .collect(),
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), v3_value(v, key, expressions)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A property of a runner node, which the runner reads as a PowerShell
/// expandable string: a backtick escapes, and `$` starts a variable or an
/// expression. The compiler writes exactly one expression, the accessor for
/// another resource's property, and that one is read back as a reference.
fn runner_value(value: &Value, key: &str, expressions: &mut Vec<String>) -> Value {
    match value {
        Value::String(text) => {
            let (text, computed) = runner_text(text);
            if computed {
                expressions.push(key.to_owned());
            }
            Value::String(text)
        }
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| runner_value(item, key, expressions))
                .collect(),
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), runner_value(v, key, expressions)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn runner_text(text: &str) -> (String, bool) {
    // A literal `${` is written `$${` in a document.
    fn flush(out: &mut String, literal: &mut String) {
        out.push_str(&literal.replace("${", "$${"));
        literal.clear();
    }
    let mut out = String::new();
    let mut literal = String::new();
    let mut computed = false;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            let mut chars = rest.chars();
            chars.next();
            if let Some(escaped) = chars.next() {
                literal.push(escaped);
            }
            rest = chars.as_str();
            continue;
        }
        if let Some(after) = rest.strip_prefix("$((reference '") {
            // `$((reference 'id').Path)` is what `${id.Path}` compiles to.
            let reference = after.split_once("')").and_then(|(id, tail)| {
                let end = tail.find(')')?;
                let path = &tail[..end];
                let plain = path.starts_with('.')
                    && path
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._[]".contains(c));
                plain.then(|| (id, path, &tail[end + 1..]))
            });
            if let Some((id, path, tail)) = reference {
                if literal.ends_with('$') {
                    // A dollar and then a reference cannot be told apart from
                    // a literal `${` once written.
                    computed = true;
                }
                flush(&mut out, &mut literal);
                out.push_str(&format!("${{{id}{path}}}"));
                rest = tail;
                continue;
            }
        }
        if c == '$' {
            computed = true;
        }
        literal.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut out, &mut literal);
    (out, computed)
}

// ---------------------------------------------------------------------------
// PowerShell.

/// What ships with PowerShell as PSDesiredStateConfiguration.
const BUILT_IN: &[&str] = &[
    "Archive",
    "Environment",
    "File",
    "Group",
    "GroupSet",
    "Log",
    "Package",
    "ProcessSet",
    "Registry",
    "Script",
    "Service",
    "ServiceSet",
    "User",
    "WaitForAll",
    "WaitForAny",
    "WaitForSome",
    "WindowsFeature",
    "WindowsFeatureSet",
    "WindowsOptionalFeature",
    "WindowsOptionalFeatureSet",
    "WindowsPackageCab",
    "WindowsProcess",
];

/// What PSDscResources, the maintained successor, has.
const PS_DSC_RESOURCES: &[&str] = &[
    "Archive",
    "Environment",
    "Group",
    "GroupSet",
    "MsiPackage",
    "ProcessSet",
    "Registry",
    "Script",
    "Service",
    "ServiceSet",
    "User",
    "WindowsFeature",
    "WindowsFeatureSet",
    "WindowsOptionalFeature",
    "WindowsOptionalFeatureSet",
    "WindowsPackageCab",
    "WindowsProcess",
];

const DEFAULT_MODULE: &str = "PSDesiredStateConfiguration";

/// The module a resource of a configuration comes from, and whether that had
/// to be guessed.
fn module_of(resource: &str, imports: &[powershell::Import]) -> (String, Option<String>) {
    let is = |list: &[&str]| list.iter().any(|r| r.eq_ignore_ascii_case(resource));
    // Asked for by name.
    for import in imports {
        if import
            .names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(resource))
        {
            if let Some(module) = import.modules.first() {
                return (module.clone(), None);
            }
        }
    }
    let modules: Vec<&String> = imports
        .iter()
        .filter(|i| i.names.is_empty())
        .flat_map(|i| &i.modules)
        .collect();
    let imported = |name: &str| modules.iter().any(|m| m.eq_ignore_ascii_case(name));
    if is(PS_DSC_RESOURCES) && imported("PSDscResources") {
        return ("PSDscResources".to_owned(), None);
    }
    if is(BUILT_IN) {
        return (DEFAULT_MODULE.to_owned(), None);
    }
    let others: Vec<&&String> = modules
        .iter()
        .filter(|m| {
            !m.eq_ignore_ascii_case("PSDscResources") && !m.eq_ignore_ascii_case(DEFAULT_MODULE)
        })
        .collect();
    match others.as_slice() {
        [one] => ((**one).clone(), None),
        [] => (
            DEFAULT_MODULE.to_owned(),
            Some(format!(
                "{resource} is not a resource PowerShell ships with and the configuration \
                 imports no module it could come from, so its type names \
                 {DEFAULT_MODULE}. Write the module it does come from"
            )),
        ),
        [first, ..] => (
            (**first).clone(),
            Some(format!(
                "the configuration imports several modules ({}) and does not say which one \
                 {resource} comes from. Its type names the first; correct it if that is wrong",
                others
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ),
    }
}

fn from_script(
    file: &str,
    script: &powershell::Script,
    drafts: &mut Vec<Draft>,
    notes: &mut Notes,
) {
    if script.configurations.is_empty() {
        notes.untranslated(
            file,
            "file",
            "",
            "the script holds no Configuration block, so there is nothing in it to import",
        );
        return;
    }
    for configuration in &script.configurations {
        let several = configuration.nodes.len() > 1;
        if configuration.nodes.is_empty() {
            notes.untranslated(
                &format!("{file}:{}", configuration.line),
                &format!("Configuration {}", configuration.name),
                "",
                "the configuration declares no resource that could be read, so no document \
                 was written for it",
            );
        }
        for (index, node) in configuration.nodes.iter().enumerate() {
            let origin = format!("{file}:{}", node.line);
            let subject = format!("Configuration {}", configuration.name);
            let name = match (&node.host, several) {
                (Some(host), true) => format!("{}-{host}", configuration.name),
                (None, true) => format!("{}-{}", configuration.name, index + 1),
                (_, false) => configuration.name.clone(),
            };
            let mut draft = Draft::new(Format::PowerShell, &name, origin.clone());
            draft.engine = "DscV2".to_owned();
            match &node.host {
                Some(host) => draft.host = host.clone(),
                None if !node.selector.is_empty() => {
                    draft.notes.untranslated(
                        &origin,
                        &subject,
                        "",
                        format!(
                            "the Node block is for {}, which is worked out when the script \
                             runs, and a document configures one host that it names. Write \
                             one document for each machine",
                            node.selector
                        ),
                    );
                    // Not localhost: the block was written for somewhere.
                    draft.unnamed_host = true;
                }
                None => {}
            }
            if index == 0 {
                if configuration.has_params {
                    draft.notes.untranslated(
                        &origin,
                        &subject,
                        "",
                        "the configuration takes parameters, and a document has none. A \
                         property that reads one is marked: write the value in, or take it \
                         from data",
                    );
                }
                for (line, text) in &configuration.skipped {
                    let mut shown: String = text.chars().take(60).collect();
                    if shown.len() < text.len() {
                        shown.push_str("...");
                    }
                    draft.notes.untranslated(
                        &format!("{file}:{line}"),
                        &subject,
                        "",
                        format!(
                            "this is PowerShell that runs when the configuration is \
                             compiled, and it was not read, nor anything declared inside \
                             it: {shown}"
                        ),
                    );
                }
                if !script.outside.is_empty() {
                    draft.notes.info(
                        &format!("{file}:{}", script.outside[0]),
                        &subject,
                        "",
                        "what the script does outside the Configuration block, such as \
                         compiling it and starting it, was not read: Meridian does both",
                    );
                }
            }
            for block in &node.resources {
                draft
                    .instances
                    .push(instance_of(file, block, configuration, &mut draft.notes));
            }
            drafts.push(draft);
        }
    }
}

fn instance_of(
    file: &str,
    block: &powershell::Block,
    configuration: &powershell::Configuration,
    notes: &mut Notes,
) -> Instance {
    let origin = format!("{file}:{}", block.line);
    let (module, guessed) = module_of(&block.resource, &configuration.imports);
    let r#type = format!("{module}/{}", block.resource);
    let subject = format!("{type} {:?}", block.name);
    let id = id_for(&block.name);
    if let Some(guessed) = guessed {
        notes.changed(&origin, &subject, &id, guessed);
    }
    let mut instance = Instance {
        origin: origin.clone(),
        name: block.name.clone(),
        r#type,
        params: BTreeMap::new(),
        depends_on: Vec::new(),
        notify: Vec::new(),
        runtime_when: String::new(),
    };
    let mut computed = Vec::new();
    for (key, value) in &block.properties {
        if key.eq_ignore_ascii_case("DependsOn") {
            let items = match value {
                Ok(Value::List(items)) => items.clone(),
                Ok(other) => vec![other.clone()],
                Err(raw) => vec![Value::String(raw.clone())],
            };
            for item in items {
                let written = match &item {
                    Value::String(s) => s.clone(),
                    other => other.canonical(),
                };
                // `[Resource]Name`.
                let wanted = written
                    .strip_prefix('[')
                    .and_then(|rest| rest.split_once(']'))
                    .map(|(t, n)| (t.to_owned(), n.to_owned()));
                instance.depends_on.push((written, wanted));
            }
            continue;
        }
        match value {
            Ok(value) => {
                instance.params.insert(key.clone(), literal(value));
            }
            Err(raw) => {
                computed.push(key.clone());
                instance
                    .params
                    .insert(key.clone(), literal(&Value::String(raw.clone())));
            }
        }
    }
    if !computed.is_empty() {
        notes.untranslated(
            &origin,
            &subject,
            &id,
            format!(
                "{} worked out by PowerShell when the configuration is compiled, which a \
                 document does not do. The expression was carried as the text it is, which \
                 is not what it meant; write the value in: {}",
                if computed.len() == 1 {
                    "this property is"
                } else {
                    "these properties are"
                },
                computed.join(", ")
            ),
        );
    }
    instance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_compiler_escaped_is_read_back() {
        assert_eq!(runner_text("costs `$5"), ("costs $5".to_owned(), false));
        assert_eq!(runner_text("a ``tick``"), ("a `tick`".to_owned(), false));
        assert_eq!(runner_text("`${x}"), ("$${x}".to_owned(), false));
        assert_eq!(
            runner_text("id=$((reference 'app').Id)!"),
            ("id=${app.Id}!".to_owned(), false)
        );
        // Anything else PowerShell would expand is not something a document
        // says.
        assert!(runner_text("$env:TEMP").1);
        assert!(runner_text("$((reference 'app'))").1);
    }

    #[test]
    fn a_resource_id_is_a_type_and_a_name() {
        assert_eq!(
            resource_id("[resourceId('Microsoft.Windows/Registry', 'key')]"),
            Some(("Microsoft.Windows/Registry".to_owned(), "key".to_owned()))
        );
        assert_eq!(resource_id("Microsoft.Windows/Registry/key"), None);
    }

    #[test]
    fn a_module_is_found_or_the_guess_is_said() {
        let imports = |modules: &[&str]| {
            vec![powershell::Import {
                modules: modules.iter().map(|m| (*m).to_owned()).collect(),
                names: Vec::new(),
            }]
        };
        assert_eq!(module_of("File", &[]).0, DEFAULT_MODULE);
        assert_eq!(
            module_of("Service", &imports(&["PSDscResources"])).0,
            "PSDscResources"
        );
        assert_eq!(
            module_of("File", &imports(&["PSDscResources"])).0,
            DEFAULT_MODULE
        );
        let (module, guessed) = module_of(
            "WebSite",
            &imports(&["PSDscResources", "WebAdministrationDsc"]),
        );
        assert_eq!((module.as_str(), guessed), ("WebAdministrationDsc", None));
        assert!(module_of("WebSite", &[]).1.is_some());
        assert!(module_of("WebSite", &imports(&["A", "B"])).1.is_some());
    }
}
