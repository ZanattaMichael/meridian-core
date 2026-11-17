//! `meridian lint`: things that compile but are probably not what the
//! author meant.
//!
//! A lint is never an error. The compiler decides what is wrong; a lint
//! points at something that is allowed and likely to hurt later: a file mode
//! that YAML read as a number, a password written into the document, a
//! command that runs on every apply.
//!
//! A rule can be silenced where it is deliberate, with a comment:
//!
//! ```yaml
//! # meridian-lint: disable=literal-secret
//! password: not-a-real-one
//! ```
//!
//! The comment covers its own line and the line below it.
//! `# meridian-lint: disable-file=rule` covers the whole file, and `all`
//! stands for every rule.

use std::collections::{BTreeMap, HashMap, HashSet};

use meridian_ir::{Document, Resource, Value};

use crate::catalog::Catalog;
use crate::check::{Diagnostic, Fix, Parsed, ParsedDoc, Severity, Workspace};
use crate::syntax::{line_kind, LineKind, Span};

/// One lint rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub code: &'static str,
    pub severity: Severity,
    /// What the rule looks for, in one sentence.
    pub summary: &'static str,
    /// Why it matters and what to do instead.
    pub explanation: &'static str,
}

/// Every rule, in the order the documentation lists them.
pub const RULES: &[Rule] = &[
    Rule {
        code: "unquoted-mode",
        severity: Severity::Warning,
        summary: "A `mode` param is written as a number.",
        explanation: "YAML reads `mode: 0644` as the number 644, not as the permissions \
                      0644, and the leading zero is lost. Write it as a quoted string: \
                      `mode: \"0644\"`. `meridian lint --fix` adds the quotes.",
    },
    Rule {
        code: "literal-secret",
        severity: Severity::Warning,
        summary: "A param that looks like a secret holds a literal value.",
        explanation: "A password, token or key written into a document ends up in version \
                      control and in the compiled output. Declare it under `spec.secrets` and \
                      read it as `${{ secret.<name> }}`, or supply it at apply time through \
                      the tool's own secret mechanism.",
    },
    Rule {
        code: "duplicate-resource",
        severity: Severity::Warning,
        summary: "Two resources of one type manage the same thing.",
        explanation: "Two `package` resources named `nginx`, or two `file` resources with \
                      one path, give the tool two answers for one thing. Most tools refuse \
                      this only at apply time, and some apply whichever comes last.",
    },
    Rule {
        code: "redundant-dependency",
        severity: Severity::Info,
        summary: "A `dependsOn` entry adds nothing.",
        explanation: "The entry repeats another, or names a resource that one of the other \
                      dependencies already depends on. The order is the same without it.",
    },
    Rule {
        code: "useless-if-missing",
        severity: Severity::Warning,
        summary: "`ifMissing: skip` on an edge to a resource that is always there.",
        explanation: "`ifMissing` only matters when a `when` condition can remove the \
                      resource the edge names. That resource has no `when`, so the edge \
                      can never be missing and the policy does nothing.",
    },
    Rule {
        code: "unguarded-exec",
        severity: Severity::Info,
        summary: "An `exec` runs on every apply.",
        explanation: "The command has no `creates` or `removes` guard and nothing notifies \
                      it, so it runs every time the document is applied and the plan is \
                      never in sync. Add a guard, or trigger it from the resource whose \
                      change it follows with `notifies`.",
    },
    Rule {
        code: "unreferenceable-id",
        severity: Severity::Warning,
        summary: "A resource id cannot be used in a `${...}` reference.",
        explanation: "A reference names a resource by letters, digits, `_` and `-`. An id \
                      with a dot, a space or anything else cannot be written as \
                      `${id.param}`, because the reference would be read as a different \
                      path.",
    },
    Rule {
        code: "unsafe-document-name",
        severity: Severity::Warning,
        summary: "`metadata.name` holds characters that do not belong in a file name.",
        explanation: "The name becomes a directory under `--out` and under the state \
                      directory. Keep it to letters, digits, `.`, `_` and `-`.",
    },
    Rule {
        code: "unused-resource-set-host",
        severity: Severity::Info,
        summary: "`targetHost` gives fields its `from` does not read.",
        explanation: "With `from: inline` only `host` is read; with `from: infrastructure` \
                      only `ref` and `output` are. The other fields are ignored, which \
                      usually means `from` is not what was intended.",
    },
];

/// The rule called `code`.
pub fn rule(code: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.code == code)
}

const SOURCE: &str = "meridian-lint";

/// Runs every rule over every document that parsed, leaving out what the
/// author silenced. Sorted by file and position.
pub fn lint(workspace: &Workspace, catalog: &Catalog) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for file in &workspace.files {
        let silenced = Silenced::read(&file.file.lines);
        for parsed in &file.docs {
            let mut found = Vec::new();
            let mut ctx = Ctx {
                file,
                parsed,
                catalog,
                out: &mut found,
            };
            ctx.document();
            for r in parsed.doc.resources() {
                ctx.resource(r);
            }
            ctx.across_resources();
            out.extend(
                found
                    .into_iter()
                    .filter(|d| !silenced.covers(&d.code, d.span.start.line)),
            );
        }
    }
    out.sort_by(|a, b| (&a.file, a.span.start, &a.code).cmp(&(&b.file, b.span.start, &b.code)));
    out
}

/// The `# meridian-lint:` comments of a file.
struct Silenced {
    file: HashSet<String>,
    lines: HashMap<usize, HashSet<String>>,
}

impl Silenced {
    fn read(lines: &[String]) -> Silenced {
        let mut silenced = Silenced {
            file: HashSet::new(),
            lines: HashMap::new(),
        };
        for (i, line) in lines.iter().enumerate() {
            let Some(at) = line.find("# meridian-lint:") else {
                continue;
            };
            let directive = line[at + "# meridian-lint:".len()..].trim();
            let (whole_file, codes) = if let Some(codes) = directive.strip_prefix("disable-file=") {
                (true, codes)
            } else if let Some(codes) = directive.strip_prefix("disable=") {
                (false, codes)
            } else {
                continue;
            };
            let codes = codes
                .split([',', ' '])
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned);
            if whole_file {
                silenced.file.extend(codes);
            } else {
                let codes: HashSet<String> = codes.collect();
                silenced.lines.entry(i).or_default().extend(codes.clone());
                // A comment on a line of its own covers the next line.
                if line_kind(line) == LineKind::Comment {
                    silenced.lines.entry(i + 1).or_default().extend(codes);
                }
            }
        }
        silenced
    }

    fn covers(&self, code: &str, line: usize) -> bool {
        let has = |set: &HashSet<String>| set.contains(code) || set.contains("all");
        has(&self.file) || self.lines.get(&line).is_some_and(has)
    }
}

struct Ctx<'a> {
    file: &'a Parsed,
    parsed: &'a ParsedDoc,
    catalog: &'a Catalog,
    out: &'a mut Vec<Diagnostic>,
}

/// Names that suggest a param holds a secret.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "access_key",
];

/// Whether a param's value is sensitive by its name. `lock_passwd` and the
/// like are settings about a secret, not the secret.
fn names_a_secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("lock_") || lower.ends_with("_file") || lower.ends_with("_path") {
        return false;
    }
    SECRET_WORDS.iter().any(|word| lower.contains(word))
}

impl Ctx<'_> {
    fn report(&mut self, code: &str, span: Span, message: String) {
        self.report_with(code, span, message, None);
    }

    fn report_with(&mut self, code: &str, span: Span, message: String, fix: Option<Fix>) {
        let rule = rule(code).expect("a reported rule is listed");
        self.out.push(Diagnostic {
            file: self.file.name.clone(),
            span,
            severity: rule.severity,
            code: code.to_owned(),
            message,
            source: SOURCE.to_owned(),
            fix,
        });
    }

    fn at(&self, r: &Resource, sub: &str) -> Span {
        self.file.resource_span(self.parsed.index, &r.id, sub)
    }

    fn document(&mut self) {
        let doc = &self.parsed.doc;
        let name = doc.name();
        if !name.is_empty()
            && !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            let span = self.file.span_of(self.parsed.index, "metadata.name", 0, 0);
            self.report(
                "unsafe-document-name",
                span,
                format!(
                    "the name {name:?} becomes a directory name; keep it to letters, digits, \
                     `.`, `_` and `-`"
                ),
            );
        }
        if let Document::ResourceSet(rs) = doc {
            if let Some(host) = &rs.spec.target_host {
                let unused: Vec<&str> = match host.from.as_str() {
                    "inline" => [("ref", &host.r#ref), ("output", &host.output)]
                        .into_iter()
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(k, _)| k)
                        .collect(),
                    "infrastructure" => [("host", &host.host)]
                        .into_iter()
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(k, _)| k)
                        .collect(),
                    _ => Vec::new(),
                };
                for field in unused {
                    let span = self.file.span_of(
                        self.parsed.index,
                        &format!("spec.targetHost.{field}"),
                        0,
                        0,
                    );
                    self.report(
                        "unused-resource-set-host",
                        span,
                        format!("`{field}` is not read with `from: {}`", host.from),
                    );
                }
            }
        }
    }

    fn resource(&mut self, r: &Resource) {
        if !r.id.is_empty()
            && !r
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            self.report(
                "unreferenceable-id",
                self.at(r, "id"),
                format!(
                    "the id {:?} cannot be written in a `${{...}}` reference; use letters, \
                     digits, `_` and `-`",
                    r.id
                ),
            );
        }

        for (name, value) in &r.params {
            if name == "mode" {
                if let Value::Int(_) | Value::Uint(_) = value {
                    let span = self.at(r, "params.mode");
                    // Quote exactly what the author typed, zeros included.
                    let typed = self
                        .file
                        .file
                        .lines
                        .get(span.start.line)
                        .and_then(|l| l.get(span.start.col..span.end.col))
                        .unwrap_or_default()
                        .to_owned();
                    let fix = (!typed.is_empty()).then(|| Fix {
                        title: format!("Quote the mode: \"{typed}\""),
                        span,
                        text: format!("\"{typed}\""),
                    });
                    self.report_with(
                        "unquoted-mode",
                        span,
                        format!(
                            "mode {typed} is read as a number, not as permissions; write it \
                             as a quoted string"
                        ),
                        fix,
                    );
                }
            }
            if names_a_secret(name) {
                if let Value::String(text) = value {
                    if !text.is_empty() && !text.contains("${") {
                        self.report(
                            "literal-secret",
                            self.at(r, &format!("params.{name}")),
                            format!(
                                "param {name:?} looks like a secret and its value is written \
                                 in the document"
                            ),
                        );
                    }
                }
            }
        }

        // Dependencies that repeat, or that another dependency implies.
        let doc = &self.parsed.doc;
        let by_id: BTreeMap<&str, &Resource> = doc
            .resources()
            .iter()
            .map(|other| (other.id.as_str(), other))
            .collect();
        let reaches = |from: &str, wanted: &str| {
            let mut seen = HashSet::new();
            let mut stack = vec![from];
            while let Some(id) = stack.pop() {
                if !seen.insert(id) {
                    continue;
                }
                for dep in by_id.get(id).map_or(&[][..], |r| &r.depends_on) {
                    if !dep.when.is_empty() {
                        continue;
                    }
                    if dep.resource == wanted {
                        return true;
                    }
                    stack.push(&dep.resource);
                }
            }
            false
        };
        for (j, dep) in r.depends_on.iter().enumerate() {
            // An entry with its own `when` is there for the runs where the
            // other path is not.
            if !dep.when.is_empty() {
                continue;
            }
            let earlier = r.depends_on[..j]
                .iter()
                .any(|d| d.resource == dep.resource && d.when.is_empty());
            let implied = r
                .depends_on
                .iter()
                .enumerate()
                .find(|(k, other)| {
                    *k != j
                        && other.when.is_empty()
                        && other.resource != dep.resource
                        && reaches(&other.resource, &dep.resource)
                })
                .map(|(_, other)| other.resource.clone());
            let message = if earlier {
                Some(format!("{:?} is already listed", dep.resource))
            } else {
                implied.map(|via| {
                    format!(
                        "{:?} already comes first, because {via:?} depends on it",
                        dep.resource
                    )
                })
            };
            if let Some(message) = message {
                self.report(
                    "redundant-dependency",
                    self.at(r, &format!("dependsOn[{j}].resource")),
                    message,
                );
            }
        }

        let conditional = |id: &str| by_id.get(id).is_some_and(|r| !r.when.is_empty());
        for (j, dep) in r.depends_on.iter().enumerate() {
            if dep.if_missing.skips() && !conditional(&dep.resource) {
                self.report(
                    "useless-if-missing",
                    self.at(r, &format!("dependsOn[{j}].ifMissing")),
                    format!("{:?} has no `when`, so it is never missing", dep.resource),
                );
            }
        }
        for (j, n) in r.notifies.iter().enumerate() {
            if n.if_missing.skips() && !conditional(&n.resource) {
                self.report(
                    "useless-if-missing",
                    self.at(r, &format!("notifies[{j}].ifMissing")),
                    format!("{:?} has no `when`, so it is never missing", n.resource),
                );
            }
        }
    }

    fn across_resources(&mut self) {
        let doc = &self.parsed.doc;
        let target = self.catalog.target(doc.target());

        // The same thing managed twice. What identifies a thing is the
        // first of these params the type has.
        let mut seen: HashMap<(&str, &str, String), &str> = HashMap::new();
        let mut reports = Vec::new();
        for r in doc.resources() {
            let Some((param, value)) =
                ["name", "path"]
                    .iter()
                    .find_map(|p| match r.params.get(*p) {
                        Some(Value::String(text)) if !text.contains("${") => {
                            Some((*p, text.clone()))
                        }
                        _ => None,
                    })
            else {
                continue;
            };
            // Only types the target describes fully are known to be
            // identified this way.
            let closed = target
                .and_then(|t| t.resource(&r.r#type))
                .is_some_and(|s| !s.open && s.find_param(param).is_some_and(|p| p.required));
            if !closed {
                continue;
            }
            match seen.get(&(r.r#type.as_str(), param, value.clone())) {
                Some(first) => reports.push((
                    self.at(r, &format!("params.{param}")),
                    format!(
                        "{} {value:?} is already managed by resource {first:?}",
                        r.r#type
                    ),
                )),
                None => {
                    seen.insert((r.r#type.as_str(), param, value), r.id.as_str());
                }
            }
        }
        for (span, message) in reports {
            self.report("duplicate-resource", span, message);
        }

        // Commands with nothing to stop them running again.
        let notified: HashSet<&str> = doc
            .resources()
            .iter()
            .flat_map(|r| r.notifies.iter().map(|n| n.resource.as_str()))
            .collect();
        let mut reports = Vec::new();
        for r in doc.resources() {
            let guarded = target
                .and_then(|t| t.resource(&r.r#type))
                .is_some_and(|s| !s.open && s.find_param("creates").is_some());
            if r.r#type == "exec"
                && guarded
                && !r.params.contains_key("creates")
                && !r.params.contains_key("removes")
                && !notified.contains(r.id.as_str())
                && r.runtime_when.is_empty()
            {
                reports.push((
                    self.at(r, "id"),
                    format!(
                        "exec {:?} has no `creates` or `removes` and nothing notifies it, so \
                         it runs on every apply",
                        r.id
                    ),
                ));
            }
        }
        for (span, message) in reports {
            self.report("unguarded-exec", span, message);
        }
    }
}

/// Applies every fix in `diagnostics` that belongs to `name` to `text`.
/// Returns the new text and how many fixes were applied.
pub fn apply_fixes(name: &str, text: &str, diagnostics: &[Diagnostic]) -> (String, usize) {
    let mut fixes: Vec<&Fix> = diagnostics
        .iter()
        .filter(|d| d.file == name)
        .filter_map(|d| d.fix.as_ref())
        .collect();
    // From the end of the file back, so an edit never moves a later span.
    fixes.sort_by_key(|f| std::cmp::Reverse(f.span.start));
    fixes.dedup_by_key(|f| f.span.start);
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let mut applied = 0;
    for fix in fixes {
        if fix.span.start.line != fix.span.end.line {
            continue;
        }
        let Some(line) = lines.get_mut(fix.span.start.line) else {
            continue;
        };
        if fix.span.end.col <= line.len() && line.is_char_boundary(fix.span.start.col) {
            line.replace_range(fix.span.start.col..fix.span.end.col, &fix.text);
            applied += 1;
        }
    }
    (lines.join("\n"), applied)
}
