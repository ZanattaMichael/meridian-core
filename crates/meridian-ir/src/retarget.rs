//! Moving a document from one target to another, as text.
//!
//! `meridian retarget` changes `spec.target` and, when the new tool imports
//! what exists, writes an `adopt:` line under each resource the old tool
//! already manages, holding the identifier that tool reported. The file is
//! rewritten as text, so comments, ordering and layout survive; anything the
//! rewrite cannot place exactly is reported and left for the author.

use std::collections::BTreeMap;

use crate::node::{load_yaml, Node, NodeKind};

/// What to change.
pub struct Retarget<'a> {
    /// The target the documents move to.
    pub to: &'a str,
    /// Whether to write `adopt:` lines: true when the new target imports
    /// what exists.
    pub adopt: bool,
    /// By document name, the identifier of each resource the old tool
    /// manages, by resource id.
    pub identifiers: &'a BTreeMap<String, BTreeMap<String, String>>,
}

/// What changed in one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// `metadata.name`.
    pub name: String,
    /// The target it moved from.
    pub from: String,
    /// The resources given an `adopt:` line.
    pub adopted: Vec<String>,
    /// What the author has to look at, one line each.
    pub notes: Vec<String>,
}

/// The rewritten file and what changed in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retargeted {
    pub text: String,
    pub moved: Vec<Moved>,
}

/// One change to the text: replace `start..end` (bytes) with `with`.
struct Edit {
    start: usize,
    end: usize,
    with: String,
}

/// Rewrites every Infrastructure and ResourceSet document in `text` to
/// `retarget.to`. Refuses JSON (its layout cannot be kept), a file with no
/// document that has a target, and a document already on `retarget.to`.
pub fn retarget(file: &str, text: &str, retarget: &Retarget) -> Result<Retargeted, String> {
    if !valid_target_name(retarget.to) {
        return Err(format!("{:?} is not a target name", retarget.to));
    }
    if matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'[')) {
        return Err(format!(
            "{file} is JSON; retarget rewrites YAML only, so its comments and layout survive"
        ));
    }
    let documents = load_yaml(file, text).map_err(|e| e.to_string())?;
    let lines = Lines::new(text);
    let mut edits = Vec::new();
    let mut moved = Vec::new();
    for document in &documents {
        let kind = scalar(document.child("kind")).unwrap_or_default();
        if kind != "Infrastructure" && kind != "ResourceSet" {
            continue;
        }
        let name = scalar(document.child("metadata").and_then(|m| m.child("name")))
            .unwrap_or_default()
            .to_owned();
        let Some(spec) = document.child("spec") else {
            continue;
        };
        let Some(target) = spec.child("target") else {
            continue;
        };
        let from = scalar(Some(target)).unwrap_or_default().to_owned();
        if from == retarget.to {
            return Err(format!("{name} is already on {from}"));
        }
        edits.push(replace_scalar(&lines, target, retarget.to)?);
        let mut done = Moved {
            name: name.clone(),
            from: from.clone(),
            ..Moved::default()
        };
        for key in ["targetVersion", "settings"] {
            if spec.child(key).is_some() {
                done.notes.push(format!(
                    "spec.{key} was written for {from}; check it fits {}",
                    retarget.to
                ));
            }
        }
        if retarget.adopt {
            let found = retarget.identifiers.get(&name);
            let resources = match spec.child("resources").map(|r| &r.kind) {
                Some(NodeKind::Seq(items)) => items.as_slice(),
                _ => &[],
            };
            let mut written = Vec::new();
            for item in resources {
                let id = scalar(item.child("id")).unwrap_or_default();
                written.push(id.to_owned());
                if item.child("adopt").is_some() {
                    continue;
                }
                if scalar(item.child("state")) == Some("absent") {
                    continue;
                }
                let Some(identifier) = found.and_then(|f| f.get(id)) else {
                    continue;
                };
                match insert_adopt(&lines, item, identifier) {
                    Some(edit) => {
                        edits.push(edit);
                        done.adopted.push(id.to_owned());
                    }
                    None => done.notes.push(format!(
                        "{id} is written in flow style; add `adopt: {identifier:?}` to it by hand"
                    )),
                }
            }
            for (id, identifier) in found.into_iter().flatten() {
                if !written.iter().any(|w| w == id) {
                    done.notes.push(format!(
                        "{from} manages {id} ({identifier}), which is not a resource \
                         written in this document; a module or composite may make it"
                    ));
                }
            }
        }
        moved.push(done);
    }
    if moved.is_empty() {
        return Err(format!(
            "{file} has no Infrastructure or ResourceSet document with a spec.target"
        ));
    }
    edits.sort_by_key(|e| e.start);
    let mut out = String::with_capacity(text.len() + 64 * edits.len());
    let mut at = 0;
    for edit in edits {
        out.push_str(&text[at..edit.start]);
        out.push_str(&edit.with);
        at = edit.end;
    }
    out.push_str(&text[at..]);
    Ok(Retargeted { text: out, moved })
}

/// A target name as `spec.target` takes it.
fn valid_target_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn scalar(node: Option<&Node>) -> Option<&str> {
    match &node?.kind {
        NodeKind::Scalar { raw, .. } => Some(raw),
        _ => None,
    }
}

/// The file by line, to turn a node's 1-based line and character column
/// into a byte offset.
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines { text, starts }
    }

    /// The byte offset of a node, and the rest of its line.
    fn at(&self, node: &Node) -> Option<(usize, &'a str)> {
        let start = *self.starts.get(node.line.checked_sub(1)?)?;
        let line = self.text[start..].split('\n').next().unwrap_or("");
        let skip: usize = line
            .chars()
            .take(node.column.checked_sub(1)?)
            .map(char::len_utf8)
            .sum();
        Some((start + skip, &line[skip..]))
    }

    /// The whole line a node is on, with its end (after the newline).
    fn line_end(&self, node: &Node) -> Option<usize> {
        let next = self.starts.get(node.line)?;
        Some(*next)
    }
}

/// Replaces a single-line scalar with `with`, keeping its quotes.
fn replace_scalar(lines: &Lines, node: &Node, with: &str) -> Result<Edit, String> {
    let (start, rest) = lines
        .at(node)
        .ok_or("spec.target could not be found in the text")?;
    let len = match rest.as_bytes().first() {
        Some(&quote @ (b'"' | b'\'')) => rest[1..]
            .find(quote as char)
            .map(|end| end + 2)
            .ok_or("spec.target is a quoted value over several lines")?,
        _ => rest
            .find([',', '}', ']', '\r'])
            .or_else(|| rest.find(" #"))
            .unwrap_or(rest.len())
            .min(rest.trim_end().len()),
    };
    let quoted = matches!(rest.as_bytes().first(), Some(b'"' | b'\''));
    Ok(Edit {
        start,
        end: start + len,
        with: if quoted {
            format!("\"{with}\"")
        } else {
            with.to_owned()
        },
    })
}

/// An `adopt:` line under a block-style resource item, after its `id` line
/// and at the same indent. `None` for a flow-style item.
fn insert_adopt(lines: &Lines, item: &Node, identifier: &str) -> Option<Edit> {
    let NodeKind::Map(entries) = &item.kind else {
        return None;
    };
    let (key, value) = entries
        .iter()
        .find(|(k, _)| matches!(&k.kind, NodeKind::Scalar { raw, .. } if raw == "id"))?;
    let (_, rest) = lines.at(item)?;
    if rest.starts_with('{') || value.line != key.line {
        return None;
    }
    let end = lines.line_end(key).unwrap_or(lines.text.len());
    let newline = if lines.text[..end].ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let indent = " ".repeat(key.column.saturating_sub(1));
    Some(Edit {
        start: end,
        end,
        with: format!("{newline}{indent}adopt: {}\n", quote(identifier)),
    })
}

/// A YAML double-quoted string holding `text` exactly as Meridian will read
/// it: `${` is written `$${`, so an identifier is never taken for a
/// reference.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' if chars.peek() == Some(&'{') => out.push_str("$$"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(document: &str, found: &[(&str, &str)]) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut map = BTreeMap::new();
        map.insert(
            document.to_owned(),
            found
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        );
        map
    }

    const DOC: &str = "\
# The network.
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: net
spec:
  target: \"terraform\" # moved soon
  resources:
    - id: vpc
      type: aws_vpc
      params: {cidr_block: 10.0.0.0/16}
    - type: aws_subnet
      id: sub
      adopt: subnet-kept
    -   id: gone
        type: aws_eip
        state: absent
    - {id: flow, type: aws_eip}
";

    #[test]
    fn the_target_changes_and_each_managed_resource_is_adopted() {
        let found = ids(
            "net",
            &[
                ("vpc", "vpc-123"),
                ("sub", "subnet-9"),
                ("gone", "eip-1"),
                ("flow", "eip-2"),
                ("made", "x-1"),
            ],
        );
        let out = retarget(
            "net.yaml",
            DOC,
            &Retarget {
                to: "opentofu",
                adopt: true,
                identifiers: &found,
            },
        )
        .unwrap();
        assert!(out.text.contains("  target: \"opentofu\" # moved soon\n"));
        assert!(out
            .text
            .contains("    - id: vpc\n      adopt: \"vpc-123\"\n      type: aws_vpc\n"));
        assert!(out.text.contains("      adopt: subnet-kept\n"));
        assert!(!out.text.contains("subnet-9"));
        assert!(!out.text.contains("eip-1"));
        assert!(out.text.starts_with("# The network.\n"));
        let moved = &out.moved[0];
        assert_eq!(moved.from, "terraform");
        assert_eq!(moved.adopted, ["vpc"]);
        assert!(moved.notes.iter().any(|n| n.contains("flow style")));
        assert!(moved.notes.iter().any(|n| n.contains("made")));
        // The result is still a document Meridian reads.
        crate::parse("net.yaml", out.text.as_bytes()).unwrap();
    }

    #[test]
    fn without_adoption_only_the_target_changes() {
        let found = ids("net", &[("vpc", "vpc-123")]);
        let out = retarget(
            "net.yaml",
            DOC,
            &Retarget {
                to: "bicep",
                adopt: false,
                identifiers: &found,
            },
        )
        .unwrap();
        assert_eq!(out.text, DOC.replace("\"terraform\"", "\"bicep\""));
    }

    #[test]
    fn an_identifier_is_written_so_it_reads_back_exactly() {
        assert_eq!(quote("a\"b\\c${d}"), "\"a\\\"b\\\\c$${d}\"");
    }

    #[test]
    fn json_a_file_with_no_target_and_the_same_target_are_refused() {
        let none = BTreeMap::new();
        let to = |to| Retarget {
            to,
            adopt: true,
            identifiers: &none,
        };
        assert!(
            retarget("a.json", "{\"kind\": \"Infrastructure\"}", &to("x"))
                .unwrap_err()
                .contains("JSON")
        );
        assert!(retarget("a.yaml", "kind: Workflow\n", &to("x"))
            .unwrap_err()
            .contains("no Infrastructure"));
        assert!(retarget("a.yaml", DOC, &to("terraform"))
            .unwrap_err()
            .contains("already on"));
        assert!(retarget("a.yaml", DOC, &to("Not A Name")).is_err());
    }

    #[test]
    fn a_plain_target_at_the_end_of_the_file_is_replaced() {
        let text = "kind: ResourceSet\nmetadata: {name: web}\nspec:\n  target: ansible";
        let none = BTreeMap::new();
        let out = retarget(
            "a.yaml",
            text,
            &Retarget {
                to: "salt",
                adopt: false,
                identifiers: &none,
            },
        )
        .unwrap();
        assert_eq!(out.text, text.replace("ansible", "salt"));
    }
}
