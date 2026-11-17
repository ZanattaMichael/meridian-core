//! Importing a configuration written in the Terraform language.
//!
//! Terraform and OpenTofu read the same language, and Meridian's targets for
//! them take the same kind of document: a resource's `type` is the tool's own
//! type and its `params` are that type's arguments. So one importer serves
//! both, and a [`Flavour`] says which target it is importing for and what
//! that target can express.
//!
//! The mapping is the targets' own, read backwards:
//!
//! | In the configuration | In the document |
//! |---|---|
//! | `resource "aws_instance" "web"` | a resource `web` of type `aws_instance` |
//! | `data "aws_ami" "ubuntu"` | a resource `ubuntu` of type `data.aws_ami` |
//! | `module "net"` | a resource `net` of type `module` |
//! | `variable "region"` | a resource `region` of type `variable` |
//! | each entry of `locals` | a resource of type `local`, with the param `value` |
//! | `provider "aws"` and its `required_providers` entry | a resource of type `provider` |
//! | `terraform { backend "s3" {} }` | a resource `s3` of type `backend` |
//! | `output "ip"` | an entry of the document's `outputs` |
//! | `aws_instance.web.id`, `var.region`, `module.net.id` | `${web.id}`, `${region}`, `${net.id}` |
//! | `depends_on` | `dependsOn` |
//! | `lifecycle { replace_triggered_by }` | `notifies` with the action `replace` |
//!
//! Everything else in a block is carried over as a param, unchanged, because
//! the targets write params straight back out. What has no counterpart
//! (`moved`, `import`, `check` and `removed` blocks, a `cloud` block, an
//! output's `sensitive`) is reported, never dropped quietly.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::{
    DocumentKind, ImportError, ImportFormat, ImportSource, Imported, ImportedDocument,
    ImportedNotify, ImportedOutput, ImportedResource, Value,
};

use crate::hcl;
use crate::yaml::{self, Rules};
use crate::{unique_id, Notes};

/// Which tool a configuration is imported for, and what its target can say.
#[derive(Clone, Copy, Debug)]
pub struct Flavour {
    /// The Meridian target the document selects.
    pub target: &'static str,
    /// The tool's name, for messages.
    pub tool: &'static str,
    /// The tool's command, for advice in messages.
    pub command: &'static str,
    /// File name endings read as HCL.
    pub hcl: &'static [&'static str],
    /// File name endings read as the JSON syntax.
    pub json: &'static [&'static str],
    /// Whether the target has `ephemeral.<type>` resources.
    pub ephemeral: bool,
    /// Whether the target has an `encryption` resource.
    pub encryption: bool,
}

/// Terraform.
pub const TERRAFORM: Flavour = Flavour {
    target: "terraform",
    tool: "Terraform",
    command: "terraform",
    hcl: &[".tf"],
    json: &[".tf.json"],
    ephemeral: false,
    encryption: false,
};

/// OpenTofu, which also reads `.tofu` files.
pub const OPENTOFU: Flavour = Flavour {
    target: "opentofu",
    tool: "OpenTofu",
    command: "tofu",
    hcl: &[".tofu", ".tf"],
    json: &[".tofu.json", ".tf.json"],
    ephemeral: true,
    encryption: true,
};

impl Flavour {
    /// The formats an importer of this flavour declares.
    pub fn formats(&self) -> Vec<ImportFormat> {
        let patterns =
            |endings: &[&str]| -> Vec<String> { endings.iter().map(|e| format!("*{e}")).collect() };
        vec![
            ImportFormat {
                name: "hcl".to_owned(),
                description: format!("{} configuration files", self.tool),
                patterns: patterns(self.hcl),
            },
            ImportFormat {
                name: "json".to_owned(),
                description: format!("{} configuration files in the JSON syntax", self.tool),
                patterns: patterns(self.json),
            },
        ]
    }
}

/// The names the language itself gives meaning to at the start of a
/// reference, in either flavour. A block named like one would be unreachable.
const NAMESPACES: &[&str] = &[
    "var",
    "local",
    "module",
    "data",
    "path",
    "terraform",
    "each",
    "count",
    "self",
    "ephemeral",
    "tofu",
    "key_provider",
    "method",
];

/// One top-level block, from either syntax.
struct Top {
    kind: String,
    labels: Vec<String>,
    /// The block's content as the JSON syntax holds it.
    body: BTreeMap<String, Value>,
    /// The attributes whose templates use a `%{ }` directive.
    directives: Vec<String>,
    origin: String,
}

/// What a block is addressed as, and the id it has in the document.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Managed,
    Data,
    Ephemeral,
    Module,
    Variable,
    Local,
}

impl Class {
    fn suffix(self) -> &'static str {
        match self {
            Class::Managed => "",
            Class::Data => "data",
            Class::Ephemeral => "ephemeral",
            Class::Module => "module",
            Class::Variable => "var",
            Class::Local => "local",
        }
    }
}

/// Every addressable block: its address in the configuration, and its id in
/// the document.
#[derive(Default)]
struct Addresses {
    /// `aws_instance.web`, `data.aws_ami.ubuntu`, `var.region` to an id.
    ids: BTreeMap<String, String>,
    /// The addresses whose id is not the block's own name.
    renamed: BTreeMap<String, String>,
    managed: BTreeSet<String>,
}

fn address(class: Class, r#type: &str, name: &str) -> String {
    match class {
        Class::Managed => format!("{type}.{name}"),
        Class::Data => format!("data.{type}.{name}"),
        Class::Ephemeral => format!("ephemeral.{type}.{name}"),
        Class::Module => format!("module.{name}"),
        Class::Variable => format!("var.{name}"),
        Class::Local => format!("local.{name}"),
    }
}

fn is_static(kind: &str) -> impl Fn(&str) -> bool + '_ {
    move |path: &str| match kind {
        "terraform" => path.starts_with("required_providers"),
        "variable" => path == "type",
        "output" => path == "depends_on",
        _ => {
            matches!(
                path,
                "depends_on"
                    | "provider"
                    | "providers"
                    | "lifecycle.ignore_changes"
                    | "lifecycle.replace_triggered_by"
            )
        }
    }
}

fn directives(body: &hcl::Body, prefix: &str, out: &mut Vec<String>) {
    for a in &body.attributes {
        if a.expr.has_directive() {
            out.push(format!("{prefix}{}", a.name));
        }
    }
    for b in &body.blocks {
        directives(&b.body, &format!("{prefix}{}.", b.r#type), out);
    }
}

fn tops_from_hcl(file: &str, text: &str, tops: &mut Vec<Top>) -> Result<(), ImportError> {
    let body = hcl::parse(file, text)?;
    if let Some(a) = body.attributes.first() {
        return Err(ImportError::at(
            file,
            a.line,
            format!(
                "{:?} is set outside any block, which a configuration file does not allow; is \
                 this a variable values file?",
                a.name
            ),
        ));
    }
    for block in body.blocks {
        let mut found = Vec::new();
        directives(&block.body, "", &mut found);
        let json = block.body.to_json(&is_static(&block.r#type));
        tops.push(Top {
            body: json,
            kind: block.r#type,
            labels: block.labels,
            directives: found,
            origin: format!("{file}:{}", block.line),
        });
    }
    Ok(())
}

fn tops_from_json(file: &str, text: &str, tops: &mut Vec<Top>) -> Result<(), ImportError> {
    let documents = yaml::read(file, text, Rules::Yaml12)?;
    let Some(root) = documents.first() else {
        return Ok(());
    };
    let Some(entries) = root.as_map() else {
        return Err(ImportError::at(
            file,
            root.line,
            format!(
                "a configuration is a JSON object, and this is {}",
                root.describe()
            ),
        ));
    };
    // A block's content is an object, or a list of them when the block repeats.
    fn bodies(node: &yaml::Node) -> Vec<(BTreeMap<String, Value>, usize)> {
        match &node.kind {
            yaml::Kind::Seq(items) => items.iter().flat_map(bodies).collect(),
            yaml::Kind::Map(_) => match node.value() {
                Value::Map(map) => vec![(map, node.line)],
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
    let mut push = |kind: &str, labels: Vec<String>, node: &yaml::Node| {
        for (mut body, line) in bodies(node) {
            body.remove("//");
            tops.push(Top {
                kind: kind.to_owned(),
                labels: labels.clone(),
                body,
                directives: Vec::new(),
                origin: format!("{file}:{line}"),
            });
        }
    };
    for (kind, node) in entries {
        match kind.as_str() {
            "//" => {}
            "resource" | "data" | "ephemeral" => {
                for (r#type, named) in node.as_map().unwrap_or_default() {
                    for (name, body) in named.as_map().unwrap_or_default() {
                        push(kind, vec![r#type.clone(), name.clone()], body);
                    }
                }
            }
            "module" | "variable" | "output" | "provider" => {
                for (name, body) in node.as_map().unwrap_or_default() {
                    push(kind, vec![name.clone()], body);
                }
            }
            _ => push(kind, Vec::new(), node),
        }
    }
    Ok(())
}

/// Writes a value of the JSON syntax back as an expression, for the one place
/// a document wants an expression as text: an output's value.
fn to_expression(value: &Value) -> String {
    match value {
        Value::String(s) => {
            // A string that is one interpolation is that expression.
            if let Ok(parts) = expr::parse(s) {
                if let [Part::Expr(e)] = parts.as_slice() {
                    return e.clone();
                }
            }
            format!(
                "\"{}\"",
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
            )
        }
        Value::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(to_expression)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Map(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("\"{k}\" = {}", to_expression(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.canonical(),
    }
}

/// Splits a path expression into its first `n` names and what follows them:
/// `aws_instance.web[0].id` with `n` of 2 is `(["aws_instance", "web"],
/// "[0].id")`.
fn split_names(expression: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut names = Vec::with_capacity(n);
    let mut rest = expression;
    for i in 0..n {
        if i > 0 {
            rest = rest.strip_prefix('.')?;
        }
        let (name, after) = expr::head(rest);
        if name.is_empty() {
            return None;
        }
        names.push(name);
        rest = after;
    }
    Some((names, rest))
}

impl Addresses {
    /// The document's spelling of a path expression, when it reads a block
    /// the configuration declares.
    fn resolve(&self, expression: &str) -> Option<String> {
        let (first, _) = expr::head(expression);
        let names = match first {
            "var" | "local" | "module" => 2,
            "data" | "ephemeral" => 3,
            _ => 2,
        };
        let (parts, rest) = split_names(expression, names)?;
        let id = self.ids.get(&parts.join("."))?;
        Some(format!("{id}{rest}"))
    }

    /// Rewrites the references in one template string.
    fn rewrite_string(&self, text: &str) -> String {
        let Ok(parts) = expr::parse(text) else {
            return text.to_owned();
        };
        let mut out = String::with_capacity(text.len());
        for part in parts {
            match part {
                Part::Text(literal) => out.push_str(&literal.replace("${", "$${")),
                Part::Expr(expression) => {
                    if expr::is_path(&expression) {
                        if let Some(rewritten) = self.resolve(&expression) {
                            out.push_str(&format!("${{{rewritten}}}"));
                            continue;
                        }
                        // A path that names no block of the configuration is
                        // something only the tool can resolve, such as the
                        // iterator of a dynamic block. A document reads a
                        // bare path as a reference to one of its own
                        // resources, so this one is parenthesised, which
                        // means the same to the tool and is handed over as
                        // written.
                        let (first, _) = expr::head(&expression);
                        if !NAMESPACES.contains(&first) {
                            out.push_str(&format!("${{({expression})}}"));
                            continue;
                        }
                    }
                    // Inside a longer expression an address stays the tool's
                    // own, so only one whose block was renamed has to change.
                    let mut expression = expression;
                    for (old, new) in &self.renamed {
                        expression = replace_address(&expression, old, new);
                    }
                    out.push_str(&format!("${{{expression}}}"));
                }
            }
        }
        out
    }

    fn rewrite(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.rewrite_string(s)),
            Value::List(items) => Value::List(items.iter().map(|v| self.rewrite(v)).collect()),
            Value::Map(map) => Value::Map(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.rewrite(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// Replaces every whole occurrence of an address inside an expression.
fn replace_address(expression: &str, old: &str, new: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(expression.len());
    let mut rest = expression;
    while let Some(at) = rest.find(old) {
        let before = rest[..at].chars().last();
        let after = rest[at + old.len()..].chars().next();
        let whole = !before.is_some_and(|c| word(c) || c == '.') && !after.is_some_and(word);
        out.push_str(&rest[..at]);
        out.push_str(if whole { new } else { old });
        rest = &rest[at + old.len()..];
    }
    out.push_str(rest);
    out
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Imports the configuration in `source` for one flavour of the language.
pub fn import(source: &ImportSource, flavour: &Flavour) -> Result<Imported, ImportError> {
    let name = if source.name.is_empty() {
        "imported".to_owned()
    } else {
        source.name.clone()
    };
    let mut notes = Notes::new();
    notes.in_document(&name);

    let mut tops = Vec::new();
    let mut read = 0;
    for (file, text) in &source.files {
        let base = file.rsplit('/').next().unwrap_or(file);
        // OpenTofu reads `main.tofu` instead of a `main.tf` beside it.
        let replaced_by = [(".tf.json", ".tofu.json"), (".tf", ".tofu")]
            .iter()
            .filter(|(_, theirs)| flavour.hcl.contains(theirs) || flavour.json.contains(theirs))
            .find_map(|(ours, theirs)| {
                let other = format!("{}{theirs}", file.strip_suffix(ours)?);
                source.files.contains_key(&other).then_some(other)
            });
        if let Some(other) = replaced_by {
            notes.info(
                file,
                "file",
                "",
                format!("not read: {} reads {other} instead of it", flavour.tool),
            );
            continue;
        }
        if flavour.json.iter().any(|e| base.ends_with(e)) {
            tops_from_json(file, text, &mut tops)?;
        } else if flavour.hcl.iter().any(|e| base.ends_with(e)) {
            tops_from_hcl(file, text, &mut tops)?;
        } else if base.ends_with(".tfvars") || base.ends_with(".tfvars.json") {
            notes.untranslated(
                file,
                "variable values",
                "",
                format!(
                    "a values file is not part of the configuration and was not read. Its \
                     values have to reach {} some other way, for example as each variable's \
                     default",
                    flavour.tool
                ),
            );
            continue;
        } else if base == ".terraform.lock.hcl" {
            notes.info(
                file,
                "dependency lock file",
                "",
                format!(
                    "not read: {} writes a new lock file where Meridian runs it",
                    flavour.tool
                ),
            );
            continue;
        } else {
            return Err(ImportError::at(
                file,
                0,
                format!(
                    "this is not a {} configuration file (expected a name ending in {})",
                    flavour.tool,
                    flavour
                        .hcl
                        .iter()
                        .chain(flavour.json)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        if base.ends_with("_override.tf")
            || base.starts_with("override.")
            || base.contains("_override.")
        {
            notes.changed(
                file,
                "override file",
                "",
                format!(
                    "{} merges an override file into the blocks it overrides; here its blocks \
                     were read as ordinary ones, so a block it overrides appears twice",
                    flavour.tool
                ),
            );
        }
        read += 1;
    }
    if read == 0 {
        return Err(ImportError::new(format!(
            "there is no {} configuration to import: no file ends in {}",
            flavour.tool,
            flavour
                .hcl
                .iter()
                .chain(flavour.json)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    // Ids first, in the order that keeps the names that matter most: a
    // managed resource's name is what its state is recorded under.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut addresses = Addresses::default();
    let mut claims: Vec<(Class, String, String, String)> = Vec::new();
    for top in &tops {
        let labels: Vec<&str> = top.labels.iter().map(String::as_str).collect();
        match (top.kind.as_str(), labels.as_slice()) {
            ("resource", [t, n]) => claims.push((
                Class::Managed,
                (*t).to_owned(),
                (*n).to_owned(),
                top.origin.clone(),
            )),
            ("data", [t, n]) => claims.push((
                Class::Data,
                (*t).to_owned(),
                (*n).to_owned(),
                top.origin.clone(),
            )),
            ("ephemeral", [t, n]) if flavour.ephemeral => claims.push((
                Class::Ephemeral,
                (*t).to_owned(),
                (*n).to_owned(),
                top.origin.clone(),
            )),
            ("module", [n]) => claims.push((
                Class::Module,
                String::new(),
                (*n).to_owned(),
                top.origin.clone(),
            )),
            ("variable", [n]) => claims.push((
                Class::Variable,
                String::new(),
                (*n).to_owned(),
                top.origin.clone(),
            )),
            ("locals", []) => {
                for key in top.body.keys() {
                    claims.push((Class::Local, String::new(), key.clone(), top.origin.clone()));
                }
            }
            _ => {}
        }
    }
    claims.sort_by_key(|(class, ..)| *class);
    for (class, r#type, block_name, origin) in &claims {
        let at = address(*class, r#type, block_name);
        if addresses.ids.contains_key(&at) {
            continue;
        }
        let free = !taken.contains(block_name) && !NAMESPACES.contains(&block_name.as_str());
        let id = if free {
            taken.insert(block_name.clone());
            block_name.clone()
        } else {
            let hint = if *class == Class::Managed {
                r#type.as_str()
            } else {
                class.suffix()
            };
            unique_id(&format!("{block_name}_{hint}"), &mut taken)
        };
        if id != *block_name {
            let renamed = address(*class, r#type, &id);
            let consequence = match class {
                Class::Managed => format!(
                    "An existing state still knows it as {at}: move it with `{} state mv {at} \
                     {renamed}` before applying, or it is destroyed and created again.",
                    flavour.command
                ),
                Class::Variable => {
                    "Whatever sets this variable has to use the new name.".to_owned()
                }
                Class::Module => format!(
                    "An existing state still knows it as {at}: move it with `{} state mv {at} \
                     {renamed}` before applying.",
                    flavour.command
                ),
                _ => "References to it inside the configuration were updated.".to_owned(),
            };
            notes.changed(
                origin,
                &at,
                &id,
                format!(
                    "a document's ids are unique and {block_name:?} was taken or reserved, so \
                     this is {id:?} here and {renamed} once compiled. {consequence}"
                ),
            );
            addresses.renamed.insert(at.clone(), renamed);
        }
        if *class == Class::Managed {
            addresses.managed.insert(at.clone());
        }
        addresses.ids.insert(at, id);
    }

    // What `required_providers` says about each provider, to be written on
    // the provider resource the document has for it.
    let mut required: BTreeMap<String, (BTreeMap<String, Value>, String)> = BTreeMap::new();
    for top in tops.iter().filter(|t| t.kind == "terraform") {
        let Some(Value::Map(providers)) = top.body.get("required_providers") else {
            continue;
        };
        for (local, value) in providers {
            let mut entry = BTreeMap::new();
            match value {
                Value::Map(map) => {
                    for (key, value) in map {
                        match key.as_str() {
                            "source" | "version" => {
                                entry.insert(key.clone(), value.clone());
                            }
                            other => notes.untranslated(
                                &top.origin,
                                &format!("required_providers.{local}.{other}"),
                                "",
                                "a provider resource carries a source and a version and nothing \
                                 else, so this was left out",
                            ),
                        }
                    }
                }
                // The oldest spelling: only a version constraint.
                Value::String(version) => {
                    entry.insert("version".to_owned(), Value::String(version.clone()));
                }
                _ => {}
            }
            required.insert(local.clone(), (entry, top.origin.clone()));
        }
    }

    let mut resources: Vec<ImportedResource> = Vec::new();
    let mut outputs = Vec::new();
    // Notifications found on the notified block, to be written on the
    // notifier once every resource exists.
    let mut notifies: Vec<(String, String)> = Vec::new();

    let mut push = |notes: &mut Notes,
                    resources: &mut Vec<ImportedResource>,
                    top: &Top,
                    subject: &str,
                    id: String,
                    r#type: String,
                    mut params: BTreeMap<String, Value>| {
        let managed = !r#type.contains('.')
            && !matches!(
                r#type.as_str(),
                "module" | "variable" | "local" | "provider" | "backend" | "encryption"
            );
        let mut depends_on = Vec::new();
        let mut kept = Vec::new();
        for at in strings(params.get("depends_on")) {
            match addresses.ids.get(&at) {
                Some(dep) => depends_on.push(dep.clone()),
                None => kept.push(Value::String(at)),
            }
        }
        if params.contains_key("depends_on") {
            if kept.is_empty() {
                params.remove("depends_on");
            } else {
                params.insert("depends_on".to_owned(), Value::List(kept));
            }
        }
        if let Some(Value::Map(lifecycle)) = params.get_mut("lifecycle") {
            let mut kept = Vec::new();
            for at in strings(lifecycle.get("replace_triggered_by")) {
                match addresses.ids.get(&at) {
                    // Only one managed resource replacing another is what a
                    // document's `replace` notification means.
                    Some(from) if managed && addresses.managed.contains(&at) => {
                        notifies.push((from.clone(), id.clone()));
                    }
                    _ => kept.push(Value::String(at)),
                }
            }
            if lifecycle.contains_key("replace_triggered_by") {
                if kept.is_empty() {
                    lifecycle.remove("replace_triggered_by");
                } else {
                    lifecycle.insert("replace_triggered_by".to_owned(), Value::List(kept));
                }
            }
            if lifecycle.is_empty() {
                params.remove("lifecycle");
            }
        }
        let params = params
            .iter()
            .map(|(k, v)| (k.clone(), addresses.rewrite(v)))
            .collect();
        for attribute in &top.directives {
            notes.untranslated(
                &top.origin,
                subject,
                &id,
                format!(
                    "{attribute:?} uses a %{{ }} template directive. A document's strings carry \
                     ${{ }} references only, so the directive compiles to literal text; rewrite \
                     it as an expression, such as a conditional or join()"
                ),
            );
        }
        resources.push(ImportedResource {
            id,
            r#type,
            params,
            depends_on,
            origin: top.origin.clone(),
            ..ImportedResource::default()
        });
    };

    let mut configured: BTreeSet<String> = BTreeSet::new();
    for top in &tops {
        let labels: Vec<&str> = top.labels.iter().map(String::as_str).collect();
        let lost = |notes: &mut Notes, what: &str, why: &str| {
            notes.untranslated(&top.origin, what, "", why);
        };
        match (top.kind.as_str(), labels.as_slice()) {
            ("resource", [t, n]) | ("data", [t, n]) | ("ephemeral", [t, n])
                if top.kind != "ephemeral" || flavour.ephemeral =>
            {
                let (class, r#type) = match top.kind.as_str() {
                    "resource" => (Class::Managed, (*t).to_owned()),
                    "data" => (Class::Data, format!("data.{t}")),
                    _ => (Class::Ephemeral, format!("ephemeral.{t}")),
                };
                let at = address(class, t, n);
                let id = addresses.ids[&at].clone();
                push(
                    &mut notes,
                    &mut resources,
                    top,
                    &format!("{} \"{t}\" \"{n}\"", top.kind),
                    id,
                    r#type,
                    top.body.clone(),
                );
            }
            ("module", [n]) => {
                let id = addresses.ids[&address(Class::Module, "", n)].clone();
                push(
                    &mut notes,
                    &mut resources,
                    top,
                    &format!("module \"{n}\""),
                    id,
                    "module".to_owned(),
                    top.body.clone(),
                );
            }
            ("variable", [n]) => {
                let id = addresses.ids[&address(Class::Variable, "", n)].clone();
                push(
                    &mut notes,
                    &mut resources,
                    top,
                    &format!("variable \"{n}\""),
                    id,
                    "variable".to_owned(),
                    top.body.clone(),
                );
            }
            ("locals", []) => {
                for (key, value) in &top.body {
                    let id = addresses.ids[&address(Class::Local, "", key)].clone();
                    let local = Top {
                        kind: "locals".to_owned(),
                        labels: Vec::new(),
                        body: BTreeMap::new(),
                        directives: top
                            .directives
                            .iter()
                            .filter(|d| *d == key)
                            .map(|_| "value".to_owned())
                            .collect(),
                        origin: top.origin.clone(),
                    };
                    push(
                        &mut notes,
                        &mut resources,
                        &local,
                        &format!("local value \"{key}\""),
                        id,
                        "local".to_owned(),
                        BTreeMap::from([("value".to_owned(), value.clone())]),
                    );
                }
            }
            ("provider", [local]) => {
                let alias = top.body.get("alias").and_then(Value::as_str);
                let wanted = match alias {
                    Some(alias) => format!("{local}_{alias}"),
                    None => (*local).to_owned(),
                };
                let free = !taken.contains(&wanted) && !NAMESPACES.contains(&wanted.as_str());
                let id = if free {
                    taken.insert(wanted.clone());
                    wanted
                } else {
                    unique_id(&format!("{wanted}_provider"), &mut taken)
                };
                let mut params = top.body.clone();
                if id != *local {
                    params.insert("name".to_owned(), Value::String((*local).to_owned()));
                }
                if configured.insert((*local).to_owned()) {
                    if let Some((entry, _)) = required.get(*local) {
                        if params.contains_key("version") && entry.contains_key("version") {
                            notes.changed(
                                &top.origin,
                                &format!("provider \"{local}\""),
                                &id,
                                "the block's own version argument was replaced by the one in \
                                 required_providers, which is where the constraint belongs",
                            );
                        }
                        params.extend(entry.clone());
                    }
                }
                push(
                    &mut notes,
                    &mut resources,
                    top,
                    &format!("provider \"{local}\""),
                    id,
                    "provider".to_owned(),
                    params,
                );
            }
            ("output", [n]) => {
                let subject = format!("output \"{n}\"");
                for key in top.body.keys() {
                    match key.as_str() {
                        "value" => {}
                        "sensitive" => lost(
                            &mut notes,
                            &subject,
                            "a document's output has a name and a value. `sensitive` was left \
                             out, so the value is shown in plans and logs unless what it reads \
                             is itself marked sensitive",
                        ),
                        other => lost(
                            &mut notes,
                            &subject,
                            &format!(
                                "a document's output has a name and a value, so {other:?} was \
                                 left out"
                            ),
                        ),
                    }
                }
                let Some(value) = top.body.get("value") else {
                    lost(
                        &mut notes,
                        &subject,
                        "the output has no value and was left out",
                    );
                    continue;
                };
                let value = match addresses.rewrite(value) {
                    Value::String(s) => s,
                    other => format!("${{{}}}", to_expression(&other)),
                };
                for attribute in &top.directives {
                    lost(
                        &mut notes,
                        &subject,
                        &format!(
                            "{attribute:?} uses a %{{ }} template directive, which compiles to \
                             literal text; rewrite it as an expression"
                        ),
                    );
                }
                outputs.push(ImportedOutput {
                    name: (*n).to_owned(),
                    value,
                });
            }
            ("terraform", []) => {
                for (key, value) in &top.body {
                    match (key.as_str(), value) {
                        ("required_providers", _) => {}
                        ("backend", Value::Map(backends)) => {
                            for (r#type, body) in backends {
                                let id = unique_id(r#type, &mut taken);
                                if id != *r#type {
                                    lost(
                                        &mut notes,
                                        &format!("backend \"{type}\""),
                                        &format!(
                                            "a backend resource's id is the backend's type, and \
                                             {type:?} is the name of something else in this \
                                             configuration. Rename that, then add the backend \
                                             back"
                                        ),
                                    );
                                    taken.remove(&id);
                                    continue;
                                }
                                let params = match body {
                                    Value::Map(map) => map.clone(),
                                    _ => BTreeMap::new(),
                                };
                                push(
                                    &mut notes,
                                    &mut resources,
                                    top,
                                    &format!("backend \"{type}\""),
                                    id,
                                    "backend".to_owned(),
                                    params,
                                );
                            }
                        }
                        ("encryption", Value::Map(body)) if flavour.encryption => {
                            let id = unique_id("encryption", &mut taken);
                            push(
                                &mut notes,
                                &mut resources,
                                top,
                                "encryption",
                                id,
                                "encryption".to_owned(),
                                body.clone(),
                            );
                        }
                        ("required_version", _) => lost(
                            &mut notes,
                            "terraform.required_version",
                            &format!(
                                "the constraint {:?} on the tool's own version has nowhere to \
                                 go in a document; pin the {} binary Meridian runs instead",
                                value.as_str().unwrap_or_default(),
                                flavour.command
                            ),
                        ),
                        ("cloud", _) => lost(
                            &mut notes,
                            "terraform.cloud",
                            "a cloud block has no counterpart. Meridian runs the tool itself \
                             and keeps its state in the working directory, or in a backend \
                             resource",
                        ),
                        (other, _) => lost(
                            &mut notes,
                            &format!("terraform.{other}"),
                            "this setting has no counterpart in a document and was left out",
                        ),
                    }
                }
            }
            (kind @ ("moved" | "import" | "removed"), _) => lost(
                &mut notes,
                &format!("{kind} block"),
                &format!(
                    "a {kind} block changes what the state records and has no counterpart in a \
                     document. Do it once with the tool before Meridian takes over, then it is \
                     not needed"
                ),
            ),
            ("check", _) => lost(
                &mut notes,
                &format!("check {:?}", labels.first().copied().unwrap_or_default()),
                "a check block has no counterpart in a document and was left out",
            ),
            ("ephemeral", [t, n]) => lost(
                &mut notes,
                &format!("ephemeral \"{t}\" \"{n}\""),
                "this target has no ephemeral resources, so the block was left out",
            ),
            (kind, _) => lost(
                &mut notes,
                &format!("{kind} block"),
                "this block has no counterpart in a document and was left out",
            ),
        }
    }

    // A provider that is required but never configured still has to be
    // declared somewhere for its source and version to be written.
    for (local, (entry, origin)) in &required {
        if configured.contains(local) || entry.is_empty() {
            continue;
        }
        let free = !taken.contains(local);
        let id = if free {
            taken.insert(local.clone());
            local.clone()
        } else {
            unique_id(&format!("{local}_provider"), &mut taken)
        };
        let mut params = entry.clone();
        if id != *local {
            params.insert("name".to_owned(), Value::String(local.clone()));
        }
        notes.info(
            origin,
            &format!("required_providers.{local}"),
            &id,
            "the configuration requires this provider without configuring it, so this resource \
             carries only its source and version and compiles to an empty provider block",
        );
        resources.push(ImportedResource {
            id,
            r#type: "provider".to_owned(),
            params,
            origin: origin.clone(),
            ..ImportedResource::default()
        });
    }

    for (from, to) in notifies {
        if let Some(r) = resources.iter_mut().find(|r| r.id == from) {
            let notify = ImportedNotify {
                resource: to,
                action: "replace".to_owned(),
            };
            if !r.notifies.contains(&notify) {
                r.notifies.push(notify);
            }
        }
    }
    Ok(Imported {
        documents: vec![ImportedDocument {
            kind: DocumentKind::Infrastructure,
            name,
            target: flavour.target.to_owned(),
            resources,
            outputs,
            ..ImportedDocument::default()
        }],
        notes: notes.into_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_split_into_its_names_and_the_rest() {
        assert_eq!(
            split_names("aws_instance.web[0].id", 2),
            Some((vec!["aws_instance", "web"], "[0].id"))
        );
        assert_eq!(
            split_names("data.aws_ami.ubuntu.id", 3),
            Some((vec!["data", "aws_ami", "ubuntu"], ".id"))
        );
        assert_eq!(
            split_names("var.region", 2),
            Some((vec!["var", "region"], ""))
        );
        assert_eq!(split_names("var", 2), None);
    }

    #[test]
    fn an_address_is_replaced_only_where_it_stands_whole() {
        assert_eq!(
            replace_address(
                "concat(var.a, var.ab, xvar.a, var.a.b)",
                "var.a",
                "var.a_var"
            ),
            "concat(var.a_var, var.ab, xvar.a, var.a_var.b)"
        );
    }

    #[test]
    fn a_value_is_written_back_as_an_expression() {
        let value = Value::Map(BTreeMap::from([
            ("id".to_owned(), Value::String("${web.id}".to_owned())),
            (
                "names".to_owned(),
                Value::List(vec![Value::String("a-${x.y}".to_owned()), Value::Int(2)]),
            ),
        ]));
        assert_eq!(
            to_expression(&value),
            "{\"id\" = web.id, \"names\" = [\"a-${x.y}\", 2]}"
        );
    }
}
