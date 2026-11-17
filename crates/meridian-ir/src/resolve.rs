//! The resolve stage: everything in a document that Meridian decides before
//! a plugin sees it.
//!
//! In order: the document's variables and named conditions are worked out,
//! `spec.when` keeps or skips the document, every `when` on a resource, an
//! edge or an output is decided, and every interpolation is placed. What
//! comes out holds no condition and no interpolation, so a target compiles
//! plain declarations and cannot decide any of this differently from the
//! next target.
//!
//! [`check`] is the half of this that needs no data, and is what
//! `meridian validate` runs: every expression reads, every name is one this
//! place can read, every `var.<name>` and `cond.<name>` is declared, and no
//! two of them read each other in a circle.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use meridian_expr::{has_interpolation, Expr, Names, Scope, Template};
use meridian_secret::{Masker, SecretTable, SecretValue};
use meridian_value::Value;

use crate::condition::{self, Data};
use crate::error::{Error, ErrorList, Position};
use crate::types::{Document, Output, Secret};

/// Something a false condition took out of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// What was removed, as a reader would name it: `resource "cert"`.
    pub subject: String,
    /// The condition that was false, as written.
    pub condition: String,
}

/// What resolving a document gives.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// The document, holding only what will be compiled, and what its
    /// conditions removed on the way.
    Kept {
        document: Box<Document>,
        removed: Vec<Removed>,
    },
    /// `spec.when` was false: the document is not part of this run.
    Skipped { condition: String },
}

/// The secrets a document reads, for one run of one stage.
///
/// A document's values hold only which secret is meant; the secrets are in
/// the table here. Dropping this wipes every one of them, so it lives exactly
/// as long as the stage that needs them.
#[derive(Debug)]
pub struct Secrets {
    table: Arc<SecretTable>,
    values: BTreeMap<String, Value>,
}

impl Secrets {
    /// The document's secrets by name, none of them fetched. This is all
    /// `validate` and `compile` ever have: a secret used in a value is
    /// described, as `(secret name)`, and one a condition must decide on is
    /// an error that says a secret is not fetched there.
    pub fn declared(doc: &Document) -> Secrets {
        let table = Arc::new(SecretTable::new());
        let values = doc
            .secrets()
            .iter()
            .map(|s| (s.name.clone(), Value::Secret(table.declare(&s.name))))
            .collect();
        Secrets { table, values }
    }

    /// Gives the secret `name` its value, fetched just now.
    pub fn hold(&mut self, name: &str, value: SecretValue) {
        match self.values.get(name) {
            Some(Value::Secret(at)) => {
                self.table.hold(*at, value);
            }
            _ => {
                let at = self.table.insert(value);
                self.values.insert(name.to_owned(), Value::Secret(at));
            }
        }
    }

    /// Records that the optional secret `name` was not found: it reads as
    /// null.
    pub fn absent(&mut self, name: &str) {
        self.values.insert(name.to_owned(), Value::Null);
    }

    /// The names of the fetched secrets whose memory the operating system
    /// would not lock, so that they may reach swap. Usually none.
    pub fn unlocked(&self) -> Vec<String> {
        self.values
            .iter()
            .filter(|(_, value)| match value {
                Value::Secret(at) => self
                    .table
                    .with(*at, |held| !held.protection().locked)
                    .unwrap_or(false),
                _ => false,
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// A filter for what a tool prints, holding every secret fetched so far
    /// in each form a tool is likely to print it. A secret too short to
    /// search for is left out.
    pub fn masker(&self) -> Masker {
        let mut masker = Masker::new();
        for value in self.values.values() {
            if let Value::Secret(at) = value {
                // One that is not fetched has nothing to search for.
                let _ = self.table.with(*at, |held| masker.add(held));
            }
        }
        masker
    }

    /// The table the values point into.
    pub fn table(&self) -> &Arc<SecretTable> {
        &self.table
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        // Values made from these may still be about, in a report or a plan;
        // they name slots that now hold nothing.
        self.table.wipe_all();
    }
}

/// The names an expression in a document can read.
pub(crate) fn names() -> Names {
    Names::new()
        .allow("data")
        .allow("var")
        .allow("cond")
        .allow("secret")
        .refuse(
            "facts",
            "facts are not available: Meridian does not gather facts from a node yet, so a \
             condition can only read data.<key>. To decide on the node while applying, use \
             runtimeWhen on a target that supports it",
        )
        .hint(
            "an expression reads data.<key>, var.<name>, cond.<name> or secret.<name>; text is \
             quoted",
        )
}

/// The scope before any variable or condition is known.
pub(crate) fn scope(data: &Data) -> Scope {
    Scope::new()
        .with("data", Value::Map(data.clone()))
        .with_unset_hint(
            "data",
            "give it with --data or --set, or test for it with defined({path})",
        )
}

/// [`scope`], with the document's secrets to read as `secret.<name>`.
fn scope_with(data: &Data, secrets: &Secrets) -> Scope {
    scope(data)
        .with("secret", Value::Map(secrets.values.clone()))
        .with_secrets(Arc::clone(&secrets.table))
        .with_unset_hint(
            "secret",
            "an optional secret that was not found is null: test for it with defined({path}) \
             or give a default with ??",
        )
}

/// Reads a condition. It is written bare; wrapped in an interpolation it
/// means the same.
///
/// Only a condition that starts with the opening is read as wrapped. One
/// that holds the opening further in holds it inside a quoted text, as in
/// `data.motd == '${{'`, where it is three characters like any others.
pub(crate) fn condition(text: &str) -> Result<Expr, String> {
    let wrapped = |text: &str| match Template::parse(text.trim())?.sole_source() {
        Some(source) => Expr::parse_condition(source),
        None => Err(
            "a condition is one expression, written bare; an interpolation cannot be mixed \
             with other text here"
                .to_owned(),
        ),
    };
    if text.trim_start().starts_with("${{") {
        return wrapped(text);
    }
    // One that does not read as an expression and holds an interpolation
    // was most likely meant as one: say that, not what the parser tripped on.
    Expr::parse_condition(text).or_else(|bare| {
        if has_interpolation(text) {
            wrapped(text).and(Err(bare))
        } else {
            Err(bare)
        }
    })
}

/// Whether `text` is a usable name after `var.` or `cond.`.
fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `text` holds a reference only the tool can resolve.
fn has_reference(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let after = &rest[at + 2..];
        if !rest[..at].ends_with('$') && !after.starts_with('{') {
            return true;
        }
        rest = after;
    }
    false
}

/// The value of a variable, from either of its two forms: the value alone,
/// or a mapping with `value` and a `description`.
fn variable_body(value: &Value) -> Result<&Value, String> {
    let Value::Map(fields) = value else {
        return Ok(value);
    };
    let Some(body) = fields.get("value") else {
        return Ok(value);
    };
    for key in fields.keys() {
        match key.as_str() {
            "value" | "description" => {}
            "type" | "enum" | "final" | "pattern" | "min" | "max" | "sensitive" => {
                return Err(format!(
                    "{key:?} on a variable is not available yet; this build takes `value` and \
                     `description`"
                ))
            }
            other => {
                return Err(format!(
                    "unknown field {other:?} (a variable written in full takes `value` and \
                     `description`; a mapping that is itself the value must not have a key \
                     named \"value\")"
                ))
            }
        }
    }
    Ok(body)
}

/// Checks that every `var.<name>` and `cond.<name>` an expression reads is
/// declared.
fn declared(reads: &BTreeSet<(String, Option<String>)>, doc: &Document) -> Result<(), String> {
    for (root, key) in reads {
        let (known, field, noun): (Vec<&String>, _, _) = match root.as_str() {
            "var" => (doc.variables().keys().collect(), "variables", "variable"),
            "cond" => (doc.conditions().keys().collect(), "conditions", "condition"),
            "secret" => (
                doc.secrets().iter().map(|s| &s.name).collect(),
                "secrets",
                "secret",
            ),
            _ => continue,
        };
        let Some(key) = key else {
            return Err(format!(
                "{root} must be followed by the name of a {noun}, as in {root}.name"
            ));
        };
        if !known.contains(&key) {
            let have = if known.is_empty() {
                format!("spec.{field} declares none")
            } else {
                let names: Vec<&str> = known.iter().map(|k| k.as_str()).collect();
                format!("spec.{field} declares: {}", names.join(", "))
            };
            return Err(format!("{root}.{key} is not declared ({have})"));
        }
    }
    Ok(())
}

/// What is wrong with one entry of `spec.secrets`, with where.
fn secret_problems(secret: &Secret, path: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut fail = |field: &str, why: String| found.push((format!("{path}.{field}"), why));

    if secret.name.is_empty() {
        fail(
            "name",
            "a secret needs a name; it is read as secret.<name>".to_owned(),
        );
    } else if !is_name(&secret.name) {
        fail(
            "name",
            format!(
                "{:?} is not a usable name: a secret is read as secret.<name>, so its name is \
                 letters, digits and underscores, and does not start with a digit",
                secret.name
            ),
        );
    }

    let written: Vec<(&str, &str)> = [
        ("name", secret.name.as_str()),
        ("from", secret.from.as_str()),
        ("key", secret.key.as_str()),
        ("path", secret.path.as_str()),
    ]
    .into_iter()
    .chain(secret.command.iter().map(|arg| ("command", arg.as_str())))
    .collect();
    for (field, text) in written {
        if has_interpolation(text) {
            fail(
                field,
                "where a secret comes from is written out, so that it can be read without any \
                 data; it cannot be interpolated"
                    .to_owned(),
            );
        }
    }

    // What the provider needs, and the fields that mean nothing to it.
    for (field, why) in secret.source().problems() {
        fail(field, why);
    }
    found
}

/// Visits every piece of text inside a value, with the path to it. A mapping
/// key is visited too, with `is_key` set.
fn each_text<'v>(value: &'v Value, path: &str, visit: &mut dyn FnMut(&'v str, &str, bool)) {
    match value {
        Value::String(text) => visit(text, path, false),
        Value::List(items) => {
            for (i, item) in items.iter().enumerate() {
                each_text(item, &format!("{path}[{i}]"), visit);
            }
        }
        Value::Map(entries) => {
            for (key, item) in entries {
                let at = format!("{path}.{key}");
                visit(key, &at, true);
                each_text(item, &at, visit);
            }
        }
        _ => {}
    }
}

const SKELETON: &str = "cannot be interpolated: it is part of the document's skeleton, which \
                        must be checkable without any data";

/// One variable or named condition.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Decl {
    root: &'static str,
    name: String,
}

impl Decl {
    fn show(&self) -> String {
        format!("{}.{}", self.root, self.name)
    }
}

/// The variables and conditions a declaration reads.
fn reads_of(decl: &Decl, doc: &Document) -> BTreeSet<Decl> {
    let mut reads = BTreeSet::new();
    match decl.root {
        "var" => {
            if let Some(Ok(body)) = doc.variables().get(&decl.name).map(variable_body) {
                each_text(body, "", &mut |text, _, _| {
                    if has_interpolation(text) {
                        if let Ok(template) = Template::parse(text) {
                            reads.extend(template.reads());
                        }
                    }
                });
            }
        }
        _ => {
            if let Some(Ok(expr)) = doc.conditions().get(&decl.name).map(|c| condition(c)) {
                reads = expr.reads();
            }
        }
    }
    declarations_among(reads, doc)
}

/// The variables and named conditions among what an expression reads.
fn declarations_among(reads: BTreeSet<(String, Option<String>)>, doc: &Document) -> BTreeSet<Decl> {
    reads
        .into_iter()
        .filter_map(|(root, key)| {
            let root = match root.as_str() {
                "var" if doc.variables().contains_key(key.as_ref()?) => "var",
                "cond" if doc.conditions().contains_key(key.as_ref()?) => "cond",
                _ => return None,
            };
            Some(Decl { root, name: key? })
        })
        .collect()
}

/// Every variable and named condition `spec.when` needs, directly or through
/// another.
fn needed_by_when(doc: &Document) -> BTreeSet<Decl> {
    let mut needed = BTreeSet::new();
    let mut waiting: Vec<Decl> = condition(doc.when())
        .map(|expr| declarations_among(expr.reads(), doc))
        .unwrap_or_default()
        .into_iter()
        .collect();
    while let Some(decl) = waiting.pop() {
        if needed.insert(decl.clone()) {
            waiting.extend(reads_of(&decl, doc));
        }
    }
    needed
}

/// The order to work the declarations out in, each after what it reads; or
/// the circle that makes that impossible, closing on the name it opened with.
fn order(doc: &Document) -> Result<Vec<Decl>, Vec<Decl>> {
    let all: Vec<Decl> = doc
        .variables()
        .keys()
        .map(|name| Decl {
            root: "var",
            name: name.clone(),
        })
        .chain(doc.conditions().keys().map(|name| Decl {
            root: "cond",
            name: name.clone(),
        }))
        .collect();
    let reads: BTreeMap<&Decl, Vec<Decl>> = all
        .iter()
        .map(|d| (d, reads_of(d, doc).into_iter().collect()))
        .collect();

    let mut done: BTreeSet<&Decl> = BTreeSet::new();
    let mut out = Vec::with_capacity(all.len());
    for start in &all {
        if done.contains(start) {
            continue;
        }
        // The path being walked, each with how many of its reads were followed.
        let mut path: Vec<(&Decl, usize)> = vec![(start, 0)];
        while let Some(&mut (decl, ref mut followed)) = path.last_mut() {
            let Some(next) = reads[decl].get(*followed) else {
                done.insert(decl);
                out.push(decl.clone());
                path.pop();
                continue;
            };
            *followed += 1;
            let Some((next, _)) = reads.get_key_value(next) else {
                continue;
            };
            if done.contains(next) {
                continue;
            }
            if let Some(opens) = path.iter().position(|(d, _)| d == next) {
                let mut circle: Vec<Decl> =
                    path[opens..].iter().map(|(d, _)| (*d).clone()).collect();
                circle.push((*next).clone());
                return Err(circle);
            }
            path.push((next, 0));
        }
    }
    Ok(out)
}

/// Checks everything about a document's expressions that needs no data.
pub(crate) fn check(doc: &Document, errs: &mut ErrorList) {
    let pos = doc.position();
    let names = names();
    let mut fail =
        |pos: &Position, path: String, msg: String| errs.push(Error::new(pos.clone(), path, msg));

    let check_condition = |text: &str| -> Result<(), String> {
        let expr = condition(text)?;
        expr.check(&names)?;
        declared(&expr.reads(), doc)
    };
    let check_text = |text: &str| -> Result<(), String> {
        if !has_interpolation(text) {
            return Ok(());
        }
        let template = Template::parse(text)?;
        template.check(&names)?;
        declared(&template.reads(), doc)
    };
    // Text in a field that is shown, stored or shapes the document.
    let check_shown = |text: &str| -> Result<(), String> {
        check_text(text)?;
        if has_interpolation(text)
            && Template::parse(text)?
                .reads()
                .iter()
                .any(|(root, _)| root == "secret")
        {
            return Err(NOT_FOR_SECRETS.to_owned());
        }
        Ok(())
    };

    let mut seen = BTreeSet::new();
    for (i, secret) in doc.secrets().iter().enumerate() {
        let path = format!("spec.secrets[{i}]");
        for (at, why) in secret_problems(secret, &path) {
            fail(&secret.pos, at, why);
        }
        if !secret.name.is_empty() && !seen.insert(&secret.name) {
            fail(
                &secret.pos,
                format!("{path}.name"),
                format!("the secret {:?} is declared twice", secret.name),
            );
        }
    }

    for (name, value) in doc.variables() {
        let path = format!("spec.variables.{name}");
        if !is_name(name) {
            fail(
                pos,
                path.clone(),
                format!(
                    "{name:?} is not a usable name: a variable is read as var.<name>, so its \
                     name is letters, digits and underscores, and does not start with a digit"
                ),
            );
        }
        let body = match variable_body(value) {
            Ok(body) => body,
            Err(why) => {
                fail(pos, path, why);
                continue;
            }
        };
        let mut problems = Vec::new();
        each_text(body, &path, &mut |text, at, is_key| {
            if is_key {
                if has_interpolation(text) {
                    problems.push((at.to_owned(), format!("a mapping key {SKELETON}")));
                }
            } else if has_reference(text) {
                problems.push((
                    at.to_owned(),
                    "a variable is worked out before the tool runs, so it cannot hold a \
                     ${id.path} reference; write the reference where it is used"
                        .to_owned(),
                ));
            } else if let Err(why) = check_text(text) {
                problems.push((at.to_owned(), why));
            }
        });
        for (at, why) in problems {
            fail(pos, at, why);
        }
    }

    for (name, text) in doc.conditions() {
        let path = format!("spec.conditions.{name}");
        if !is_name(name) {
            fail(
                pos,
                path.clone(),
                format!(
                    "{name:?} is not a usable name: a condition is read as cond.<name>, so \
                     its name is letters, digits and underscores, and does not start with a \
                     digit"
                ),
            );
        }
        if text.is_empty() {
            fail(pos, path, "a named condition needs an expression".into());
        } else if let Err(why) = check_condition(text) {
            fail(pos, path, why);
        }
    }

    if let Err(circle) = order(doc) {
        let first = &circle[0];
        let names: Vec<String> = circle.iter().map(Decl::show).collect();
        let field = if first.root == "var" {
            "variables"
        } else {
            "conditions"
        };
        fail(
            pos,
            format!("spec.{field}.{}", first.name),
            format!(
                "these read each other in a circle, so none can be worked out: {}",
                names.join(" -> ")
            ),
        );
    }

    if !doc.when().is_empty() {
        if let Err(why) = check_condition(doc.when()) {
            fail(pos, "spec.when".into(), why);
        }
    }
    for (path, text) in [
        ("spec.target", doc.target()),
        ("spec.targetVersion", doc.target_version()),
    ] {
        if let Err(why) = check_shown(text) {
            fail(pos, path.into(), why);
        }
    }
    if has_interpolation(doc.name()) {
        fail(pos, "metadata.name".into(), SKELETON.into());
    }
    for (key, value) in doc.labels() {
        if has_interpolation(key) {
            fail(
                pos,
                format!("metadata.labels.{key}"),
                format!("a label name {SKELETON}"),
            );
        }
        if let Err(why) = check_shown(value) {
            fail(pos, format!("metadata.labels.{key}"), why);
        }
    }
    if let Some(th) = doc
        .as_resource_set()
        .and_then(|d| d.spec.target_host.as_ref())
    {
        if let Err(why) = check_shown(&th.host) {
            fail(&th.pos, "spec.targetHost.host".into(), why);
        }
        for (field, text) in [
            ("ref", &th.r#ref),
            ("output", &th.output),
            ("from", &th.from),
        ] {
            if has_interpolation(text) {
                fail(&th.pos, format!("spec.targetHost.{field}"), SKELETON.into());
            }
        }
    }

    for (i, r) in doc.resources().iter().enumerate() {
        let path = format!("spec.resources[{i}]");
        for (field, text) in [("id", &r.id), ("type", &r.r#type)] {
            if has_interpolation(text) {
                fail(&r.pos, format!("{path}.{field}"), SKELETON.into());
            }
        }
        if !r.when.is_empty() {
            if let Err(why) = check_condition(&r.when) {
                fail(&r.pos, format!("{path}.when"), why);
            }
        }
        if let Err(why) = check_shown(&r.state) {
            fail(&r.pos, format!("{path}.state"), why);
        }
        let mut problems = Vec::new();
        for (name, value) in &r.params {
            let at = format!("{path}.params.{name}");
            if has_interpolation(name) {
                problems.push((at.clone(), format!("a param name {SKELETON}")));
            }
            each_text(value, &at, &mut |text, at, is_key| {
                if is_key {
                    if has_interpolation(text) {
                        problems.push((at.to_owned(), format!("a mapping key {SKELETON}")));
                    }
                } else if let Err(why) = check_text(text) {
                    problems.push((at.to_owned(), why));
                }
            });
        }
        for (at, why) in problems {
            fail(&r.pos, at, why);
        }
        for (j, dep) in r.depends_on.iter().enumerate() {
            let p = format!("{path}.dependsOn[{j}]");
            if has_interpolation(&dep.resource) {
                fail(&dep.pos, format!("{p}.resource"), SKELETON.into());
            }
            if !dep.when.is_empty() {
                if let Err(why) = check_condition(&dep.when) {
                    fail(&dep.pos, format!("{p}.when"), why);
                }
            }
        }
        for (j, n) in r.notifies.iter().enumerate() {
            let p = format!("{path}.notifies[{j}]");
            for (field, text) in [("resource", &n.resource), ("action", &n.action)] {
                if has_interpolation(text) {
                    fail(&n.pos, format!("{p}.{field}"), SKELETON.into());
                }
            }
            if !n.when.is_empty() {
                if let Err(why) = check_condition(&n.when) {
                    fail(&n.pos, format!("{p}.when"), why);
                }
            }
        }
    }

    for (i, o) in doc.outputs().iter().enumerate() {
        let p = format!("spec.outputs[{i}]");
        if has_interpolation(&o.name) {
            fail(&o.pos, format!("{p}.name"), SKELETON.into());
        }
        if !o.when.is_empty() {
            if let Err(why) = check_condition(&o.when) {
                fail(&o.pos, format!("{p}.when"), why);
            }
        }
        if let Err(why) = check_shown(&o.value) {
            fail(&o.pos, format!("{p}.value"), why);
        }
    }
}

/// Places every interpolation in a value. With `literal`, what is placed can
/// never become a reference the tool resolves.
fn place(
    value: &Value,
    scope: &Scope,
    literal: bool,
    path: &str,
) -> Result<Value, (String, String)> {
    Ok(match value {
        Value::String(text) if has_interpolation(text) => {
            let template = Template::parse(text).map_err(|why| (path.to_owned(), why))?;
            let decided = if literal {
                template.eval_literal(scope)
            } else {
                template.eval(scope)
            };
            decided.map_err(|why| (path.to_owned(), why))?
        }
        Value::List(items) => Value::List(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| place(item, scope, literal, &format!("{path}[{i}]")))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(key, item)| {
                    let at = format!("{path}.{key}");
                    if has_interpolation(key) {
                        return Err((at, format!("a mapping key {SKELETON}")));
                    }
                    Ok((key.clone(), place(item, scope, literal, &at)?))
                })
                .collect::<Result<_, _>>()?,
        ),
        other => other.clone(),
    })
}

/// Why a secret is refused everywhere but a resource's `params`.
const NOT_FOR_SECRETS: &str = "a secret cannot be used here: this field is shown, stored or \
                               shapes the document. A secret can be read by a when, and placed \
                               in a resource's params";

/// Places the interpolations of a field that is text, such as `state`.
fn place_text(text: &mut String, scope: &Scope, literal: bool) -> Result<(), String> {
    if !has_interpolation(text) {
        return Ok(());
    }
    let template = Template::parse(text)?;
    let decided = if literal {
        template.eval_literal(scope)
    } else {
        template.eval(scope)
    }?;
    *text = match decided {
        Value::String(s) => s,
        value @ (Value::Bool(_) | Value::Int(_) | Value::Uint(_) | Value::Float(_)) => {
            value.canonical()
        }
        Value::Secret(_) => return Err(NOT_FOR_SECRETS.to_owned()),
        other => {
            return Err(format!(
                "this field is text, and the interpolation gives {}",
                other.describe()
            ))
        }
    };
    Ok(())
}

/// Works out the document's variables and named conditions, each after what
/// it reads, and returns the scope every other expression is decided in.
///
/// With `only`, just those are worked out: what `spec.when` needs, so that a
/// document it leaves out is not worked out any further, and fetches nothing.
fn declarations(
    doc: &Document,
    data: &Data,
    secrets: &Secrets,
    only: Option<&BTreeSet<Decl>>,
) -> Result<Scope, Error> {
    let pos = doc.position();
    let base = scope_with(data, secrets);
    if doc.variables().is_empty() && doc.conditions().is_empty() {
        return Ok(base
            .with("var", Value::Map(BTreeMap::new()))
            .with("cond", Value::Map(BTreeMap::new())));
    }
    let order = order(doc).map_err(|circle| {
        let names: Vec<String> = circle.iter().map(Decl::show).collect();
        Error::new(
            pos.clone(),
            "spec",
            format!(
                "these read each other in a circle, so none can be worked out: {}",
                names.join(" -> ")
            ),
        )
    })?;

    let mut vars: BTreeMap<String, Value> = BTreeMap::new();
    let mut conds: BTreeMap<String, Value> = BTreeMap::new();
    let scoped = |vars: &BTreeMap<String, Value>, conds: &BTreeMap<String, Value>| {
        base.clone()
            .with("var", Value::Map(vars.clone()))
            .with("cond", Value::Map(conds.clone()))
    };
    for decl in order {
        if only.is_some_and(|needed| !needed.contains(&decl)) {
            continue;
        }
        let scope = scoped(&vars, &conds);
        if decl.root == "var" {
            let path = format!("spec.variables.{}", decl.name);
            let value = &doc.variables()[&decl.name];
            let body =
                variable_body(value).map_err(|why| Error::new(pos.clone(), path.clone(), why))?;
            let value = place(body, &scope, false, &path)
                .map_err(|(at, why)| Error::new(pos.clone(), at, why))?;
            vars.insert(decl.name, value);
        } else {
            let path = format!("spec.conditions.{}", decl.name);
            let text = &doc.conditions()[&decl.name];
            let decided = condition(text)
                .and_then(|expr| expr.truth(&scope))
                .map_err(|why| Error::new(pos.clone(), path, why))?;
            conds.insert(decl.name, Value::Bool(decided));
        }
    }
    Ok(scoped(&vars, &conds))
}

/// Decides a condition in `scope`.
fn decide(text: &str, scope: &Scope) -> Result<bool, String> {
    condition(text)?.truth(scope)
}

/// Resolves a document against `data`: decides every condition and places
/// every interpolation.
///
/// The document must already be valid ([`crate::validate`]); what is checked
/// here is only what needs the data. The first thing that cannot be decided
/// is an error, never a guess.
///
/// No secret is fetched: one placed in a param is only named there, and a
/// condition that must decide on one is an error.
pub fn resolve(doc: &Document, data: &Data) -> Result<Resolution, Error> {
    resolve_with(doc, data, &Secrets::declared(doc))
}

/// [`resolve`], with the document's secrets as far as they have been
/// fetched. The resolved document's params hold only which secret is meant;
/// `secrets` must outlive every use of them.
pub fn resolve_with(doc: &Document, data: &Data, secrets: &Secrets) -> Result<Resolution, Error> {
    let pos = doc.position().clone();

    if !doc.when().is_empty() {
        let scope = declarations(doc, data, secrets, Some(&needed_by_when(doc)))?;
        let keep =
            decide(doc.when(), &scope).map_err(|why| Error::new(pos.clone(), "spec.when", why))?;
        if !keep {
            return Ok(Resolution::Skipped {
                condition: doc.when().to_owned(),
            });
        }
    }
    let scope = declarations(doc, data, secrets, None)?;

    let mut removed = Vec::new();
    let mut resources = condition::prune_in(doc.resources(), &scope, &mut removed)?;
    for r in &mut resources {
        let i = r.index;
        let path = format!("spec.resources[{i}]");
        let fail = |at: String, why: String| {
            Error::new(r.pos.clone(), at, format!("resource {:?}: {why}", r.id))
        };
        place_text(&mut r.state, &scope, false)
            .map_err(|why| fail(format!("{path}.state"), why))?;
        let mut params = BTreeMap::new();
        for (name, value) in &r.params {
            let at = format!("{path}.params.{name}");
            let value = place(value, &scope, true, &at).map_err(|(at, why)| fail(at, why))?;
            params.insert(name.clone(), value);
        }
        r.params = params;
    }

    let mut document = doc.clone();
    let placed = |text: &mut String, path: &str| {
        place_text(text, &scope, false).map_err(|why| Error::new(pos.clone(), path, why))
    };
    match &mut document {
        Document::Infrastructure(d) => {
            placed(&mut d.spec.target, "spec.target")?;
            placed(&mut d.spec.target_version, "spec.targetVersion")?;
            d.spec.resources = resources;
            d.spec.outputs = outputs(&d.spec.outputs, &scope, &mut removed)?;
            clear(
                &mut d.spec.when,
                &mut d.spec.variables,
                &mut d.spec.conditions,
            );
            d.spec.secrets.clear();
            labels(&mut d.metadata.labels, &scope, &pos)?;
        }
        Document::ResourceSet(d) => {
            placed(&mut d.spec.target, "spec.target")?;
            placed(&mut d.spec.target_version, "spec.targetVersion")?;
            d.spec.resources = resources;
            if let Some(th) = &mut d.spec.target_host {
                place_text(&mut th.host, &scope, false)
                    .map_err(|why| Error::new(th.pos.clone(), "spec.targetHost.host", why))?;
            }
            clear(
                &mut d.spec.when,
                &mut d.spec.variables,
                &mut d.spec.conditions,
            );
            d.spec.secrets.clear();
            labels(&mut d.metadata.labels, &scope, &pos)?;
        }
    }
    if document.target().is_empty() {
        return Err(Error::new(
            pos.clone(),
            "spec.target",
            "the interpolation gives empty text, and a document needs a target",
        ));
    }
    Ok(Resolution::Kept {
        document: Box::new(document),
        removed,
    })
}

/// The labels a document has once its interpolations are placed: what a
/// label selector matches against.
pub fn resolved_labels(doc: &Document, data: &Data) -> Result<BTreeMap<String, String>, Error> {
    let mut found = doc.labels().clone();
    if found.values().any(|v| has_interpolation(v)) {
        let scope = declarations(doc, data, &Secrets::declared(doc), None)?;
        labels(&mut found, &scope, doc.position())?;
    }
    Ok(found)
}

fn labels(
    labels: &mut BTreeMap<String, String>,
    scope: &Scope,
    pos: &Position,
) -> Result<(), Error> {
    for (key, value) in labels.iter_mut() {
        place_text(value, scope, false)
            .map_err(|why| Error::new(pos.clone(), format!("metadata.labels.{key}"), why))?;
    }
    Ok(())
}

/// A resolved document carries nothing left to decide.
fn clear(
    when: &mut String,
    variables: &mut BTreeMap<String, Value>,
    conditions: &mut BTreeMap<String, String>,
) {
    when.clear();
    variables.clear();
    conditions.clear();
}

fn outputs(
    outputs: &[Output],
    scope: &Scope,
    removed: &mut Vec<Removed>,
) -> Result<Vec<Output>, Error> {
    let mut kept = Vec::with_capacity(outputs.len());
    for (i, o) in outputs.iter().enumerate() {
        let path = format!("spec.outputs[{i}]");
        let fail = |at: String, why: String| {
            Error::new(o.pos.clone(), at, format!("output {:?}: {why}", o.name))
        };
        if !o.when.is_empty() {
            let keep = decide(&o.when, scope).map_err(|why| fail(format!("{path}.when"), why))?;
            if !keep {
                removed.push(Removed {
                    subject: format!("output {:?}", o.name),
                    condition: o.when.clone(),
                });
                continue;
            }
        }
        let mut o = o.clone();
        o.when.clear();
        place_text(&mut o.value, scope, true).map_err(|why| fail(format!("{path}.value"), why))?;
        kept.push(o);
    }
    Ok(kept)
}
