//! Writing an imported document in Meridian's own syntax.
//!
//! This is the one place that knows how an imported document is spelled. An
//! importer returns data; when the language changes, this module changes and
//! every importer keeps working.
//!
//! What it adds to the data is the importer's notes, as comments. A note
//! that leaves the operator something to do is written with [`TODO_MARKER`]
//! beside the resource it is about, or at the top of the document when it is
//! about no resource, so nothing the source said is lost without a line in
//! the file saying so.

use std::collections::BTreeSet;

use meridian_sdk::{
    DocumentKind, ImportNote, Imported, ImportedDocument, ImportedResource, NoteLevel, Value,
};
use meridian_yaml::Yaml;

/// What marks a line of an imported document that needs the operator's
/// attention. Searching a file for it finds every one.
pub const TODO_MARKER: &str = "TODO(meridian-import)";

/// The host written for a document whose source named none. It is not a
/// valid host name on purpose, so the document cannot be applied by accident.
pub const HOST_PLACEHOLDER: &str = "CHANGE-ME.invalid";

/// The widest a comment line is written.
const WIDTH: usize = 78;

/// One imported document, as the text of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    /// The document's `metadata.name`.
    pub name: String,
    /// A file name for it, unique among the documents of one import.
    pub file_name: String,
    pub text: String,
    /// How many `TODO` markers the text holds.
    pub todos: usize,
}

/// Writes every document of an import. `source` is how the header describes
/// where the documents came from, such as `./infra` or `playbook.yml`.
///
/// A note that names no document is written at the top of the first one.
pub fn render_all(imported: &Imported, source: &str) -> Vec<Rendered> {
    let mut names = BTreeSet::new();
    imported
        .documents
        .iter()
        .enumerate()
        .map(|(index, document)| {
            let notes: Vec<&ImportNote> = imported
                .notes
                .iter()
                .filter(|n| n.document == document.name || (index == 0 && n.document.is_empty()))
                .collect();
            let (text, todos) = render(document, &notes, source);
            Rendered {
                name: document.name.clone(),
                file_name: format!(
                    "{}.yaml",
                    crate::unique_id(&crate::slug(&document.name, '-'), &mut names)
                ),
                text,
                todos,
            }
        })
        .collect()
}

fn value_node(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(i) => Yaml::Int(*i),
        Value::Uint(u) => Yaml::Uint(*u),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::str(s),
        Value::List(items) => Yaml::Seq(items.iter().map(value_node).collect()),
        Value::Map(map) => Yaml::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), value_node(value)))
                .collect(),
        ),
        // An importer reads text and has no secret table, so it cannot make
        // one of these. Should one arrive, it is written as every other
        // rendering of a value writes it.
        Value::Secret(_) => Yaml::str(meridian_sdk::SENSITIVE),
    }
}

fn pair(key: &str, value: Yaml) -> (String, Yaml) {
    (key.to_owned(), value)
}

fn resource_node(r: &ImportedResource) -> Yaml {
    let mut fields = vec![
        pair("id", Yaml::str(&r.id)),
        pair("type", Yaml::str(&r.r#type)),
    ];
    if !r.state.is_empty() {
        fields.push(pair("state", Yaml::str(&r.state)));
    }
    fields.push(pair(
        "params",
        Yaml::Map(
            r.params
                .iter()
                .map(|(key, value)| (key.clone(), value_node(value)))
                .collect(),
        ),
    ));
    if !r.depends_on.is_empty() {
        fields.push(pair(
            "dependsOn",
            Yaml::Seq(
                r.depends_on
                    .iter()
                    .map(|id| Yaml::Map(vec![pair("resource", Yaml::str(id))]))
                    .collect(),
            ),
        ));
    }
    if !r.notifies.is_empty() {
        fields.push(pair(
            "notifies",
            Yaml::Seq(
                r.notifies
                    .iter()
                    .map(|n| {
                        Yaml::Map(vec![
                            pair("resource", Yaml::str(&n.resource)),
                            pair("action", Yaml::str(&n.action)),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    if !r.runtime_when.is_empty() {
        fields.push(pair("runtimeWhen", Yaml::str(&r.runtime_when)));
    }
    Yaml::Map(fields)
}

/// Appends `text` as comment lines at `indent`, wrapped to [`WIDTH`]. The
/// first line starts with `first`, the rest are aligned under it.
fn comment(out: &mut String, indent: usize, first: &str, text: &str) {
    let pad = " ".repeat(indent);
    let room = WIDTH.saturating_sub(indent + 2).max(30);
    let mut lead = first.to_owned();
    for paragraph in text.lines() {
        let mut line = lead.clone();
        let mut empty = true;
        for word in paragraph.split_whitespace() {
            if !empty && line.chars().count() + 1 + word.chars().count() > room {
                out.push_str(&format!("{pad}# {}\n", line.trim_end()));
                line = "  ".to_owned();
                empty = true;
            }
            if !empty {
                line.push(' ');
            }
            line.push_str(word);
            empty = false;
        }
        out.push_str(&format!("{pad}# {}\n", line.trim_end()));
        lead = "  ".to_owned();
    }
}

/// Writes one note as a comment, and says whether it was a `TODO`.
fn note(out: &mut String, indent: usize, n: &ImportNote) -> bool {
    let todo = n.level.needs_attention();
    let first = match n.level {
        NoteLevel::Info => String::new(),
        level => format!("{TODO_MARKER}: {level}: "),
    };
    comment(out, indent, &first, &n.to_string());
    todo
}

fn indented(text: &str, by: usize) -> String {
    let pad = " ".repeat(by);
    text.lines()
        .map(|line| {
            if line.is_empty() {
                "\n".to_owned()
            } else {
                format!("{pad}{line}\n")
            }
        })
        .collect()
}

/// Writes one document and counts its `TODO` markers. `notes` are the notes
/// that belong to it.
pub fn render(document: &ImportedDocument, notes: &[&ImportNote], source: &str) -> (String, usize) {
    let ids: BTreeSet<&str> = document.resources.iter().map(|r| r.id.as_str()).collect();
    // A note about a resource the document does not hold still has to be
    // written somewhere, so it goes to the top.
    let (beside, above): (Vec<&ImportNote>, Vec<&ImportNote>) = notes
        .iter()
        .partition(|n| !n.resource.is_empty() && ids.contains(n.resource.as_str()));

    let mut body = String::new();
    let mut todos = 0;

    let no_host = document.kind == DocumentKind::ResourceSet && document.host.is_empty();
    let mut spec = vec![pair("target", Yaml::str(&document.target))];
    if !document.target_version.is_empty() {
        spec.push(pair("targetVersion", Yaml::str(&document.target_version)));
    }
    if document.kind == DocumentKind::ResourceSet {
        let host = if no_host {
            HOST_PLACEHOLDER
        } else {
            &document.host
        };
        spec.push(pair(
            "targetHost",
            Yaml::Map(vec![
                pair("from", Yaml::str("inline")),
                pair("host", Yaml::str(host)),
            ]),
        ));
    }
    let head = Yaml::Map(vec![
        pair("apiVersion", Yaml::str("meridian/v1")),
        pair("kind", Yaml::str(document.kind.as_str())),
        pair(
            "metadata",
            Yaml::Map(vec![pair("name", Yaml::str(&document.name))]),
        ),
        pair("spec", Yaml::Map(spec)),
    ]);
    body.push_str(&meridian_yaml::write(&head));

    if document.resources.is_empty() {
        body.push_str("  resources: []\n");
    } else {
        body.push_str("  resources:\n");
    }
    for (index, r) in document.resources.iter().enumerate() {
        if index > 0 {
            body.push('\n');
        }
        if !r.origin.is_empty() {
            comment(&mut body, 4, "", &format!("From {}", r.origin));
        }
        for n in beside.iter().filter(|n| n.resource == r.id) {
            if note(&mut body, 4, n) {
                todos += 1;
            }
        }
        let text = meridian_yaml::write(&Yaml::Seq(vec![resource_node(r)]));
        body.push_str(&indented(&text, 4));
    }

    if !document.outputs.is_empty() {
        let outputs = Yaml::Map(vec![pair(
            "outputs",
            Yaml::Seq(
                document
                    .outputs
                    .iter()
                    .map(|o| {
                        Yaml::Map(vec![
                            pair("name", Yaml::str(&o.name)),
                            pair("value", Yaml::str(&o.value)),
                        ])
                    })
                    .collect(),
            ),
        )]);
        body.push('\n');
        body.push_str(&indented(&meridian_yaml::write(&outputs), 2));
    }

    let mut top = String::new();
    if no_host {
        todos += 1;
        comment(
            &mut top,
            0,
            &format!("{TODO_MARKER}: {}: ", NoteLevel::Untranslated),
            &format!(
                "the source does not name the host this configures. Replace \
                 {HOST_PLACEHOLDER} under spec.targetHost with the host, or take it from an \
                 Infrastructure document's output."
            ),
        );
    }
    for n in &above {
        if note(&mut top, 0, n) {
            todos += 1;
        }
    }

    let mut out = String::new();
    comment(
        &mut out,
        0,
        "",
        &format!("Imported from {source} by `meridian import`."),
    );
    if todos > 0 {
        out.push_str("#\n");
        comment(
            &mut out,
            0,
            "",
            &format!(
                "{} to look at before this document is applied. Each is marked with \
                 {TODO_MARKER}, here or beside the resource it is about.",
                if todos == 1 {
                    "There is 1 thing".to_owned()
                } else {
                    format!("There are {todos} things")
                }
            ),
        );
    }
    if !top.is_empty() {
        out.push_str("#\n");
        out.push_str(&top);
    }
    out.push_str(&body);
    (out, todos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{ImportedNotify, ImportedOutput};
    use std::collections::BTreeMap;

    fn resource(id: &str, r#type: &str) -> ImportedResource {
        ImportedResource {
            id: id.to_owned(),
            r#type: r#type.to_owned(),
            ..ImportedResource::default()
        }
    }

    #[test]
    fn a_document_is_written_with_its_notes_beside_what_they_are_about() {
        let mut pkg = resource("nginx", "package");
        pkg.params = BTreeMap::from([("name".to_owned(), Value::String("nginx".to_owned()))]);
        pkg.origin = "site.yml:4".to_owned();
        pkg.notifies = vec![ImportedNotify {
            resource: "nginx_svc".to_owned(),
            action: "restart".to_owned(),
        }];
        let mut svc = resource("nginx_svc", "service");
        svc.state = "running".to_owned();
        svc.depends_on = vec!["nginx".to_owned()];
        svc.runtime_when = "ansible_os_family == 'Debian'".to_owned();
        let document = ImportedDocument {
            kind: DocumentKind::ResourceSet,
            name: "web".to_owned(),
            target: "ansible".to_owned(),
            host: "web01".to_owned(),
            resources: vec![pkg, svc],
            ..ImportedDocument::default()
        };
        let notes = [
            ImportNote {
                level: NoteLevel::Untranslated,
                origin: "site.yml:2".to_owned(),
                subject: "play \"web\"".to_owned(),
                msg: "become is not carried over".to_owned(),
                document: "web".to_owned(),
                resource: String::new(),
            },
            ImportNote {
                level: NoteLevel::Changed,
                origin: "site.yml:4".to_owned(),
                subject: "task \"Install nginx\"".to_owned(),
                msg: "apt became the generic package type".to_owned(),
                document: "web".to_owned(),
                resource: "nginx".to_owned(),
            },
            ImportNote {
                level: NoteLevel::Info,
                origin: String::new(),
                subject: String::new(),
                msg: "the id is the task's name".to_owned(),
                document: "web".to_owned(),
                resource: "nginx_svc".to_owned(),
            },
        ];
        let notes: Vec<&ImportNote> = notes.iter().collect();
        let (text, todos) = render(&document, &notes, "site.yml");
        assert_eq!(todos, 2);
        assert_eq!(
            text,
            "\
# Imported from site.yml by `meridian import`.
#
# There are 2 things to look at before this document is applied. Each is
#   marked with TODO(meridian-import), here or beside the resource it is
#   about.
#
# TODO(meridian-import): not translated: site.yml:2: play \"web\": become is not
#   carried over
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  resources:
    # From site.yml:4
    # TODO(meridian-import): changed: site.yml:4: task \"Install nginx\": apt
    #   became the generic package type
    - id: nginx
      type: package
      params:
        name: nginx
      notifies:
        - resource: nginx_svc
          action: restart

    # the id is the task's name
    - id: nginx_svc
      type: service
      state: running
      params: {}
      dependsOn:
        - resource: nginx
      runtimeWhen: ansible_os_family == 'Debian'
"
        );
    }

    #[test]
    fn a_resource_set_with_no_host_gets_a_placeholder_and_a_marker() {
        let document = ImportedDocument {
            kind: DocumentKind::ResourceSet,
            name: "web".to_owned(),
            target: "dsc".to_owned(),
            ..ImportedDocument::default()
        };
        let (text, todos) = render(&document, &[], "web.ps1");
        assert_eq!(todos, 1);
        assert!(text.contains("host: CHANGE-ME.invalid"), "{text}");
        assert!(text.contains(TODO_MARKER), "{text}");
        assert!(text.contains("resources: []"), "{text}");
    }

    #[test]
    fn outputs_follow_the_resources_and_file_names_are_unique() {
        let document = ImportedDocument {
            name: "Web VM".to_owned(),
            target: "terraform".to_owned(),
            outputs: vec![ImportedOutput {
                name: "ip".to_owned(),
                value: "${vm.public_ip}".to_owned(),
            }],
            ..ImportedDocument::default()
        };
        let imported = Imported {
            documents: vec![document.clone(), document],
            notes: Vec::new(),
        };
        let rendered = render_all(&imported, "main.tf");
        assert_eq!(rendered[0].file_name, "web-vm.yaml");
        assert_eq!(rendered[1].file_name, "web-vm_2.yaml");
        assert!(
            rendered[0]
                .text
                .ends_with("  outputs:\n    - name: ip\n      value: ${vm.public_ip}\n"),
            "{}",
            rendered[0].text
        );
    }
}
