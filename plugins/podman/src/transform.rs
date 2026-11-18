//! Stage one: each resource becomes a unit, and every reference between
//! resources becomes the name Quadlet, Podman or systemd knows the other by.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{holds_secret, CompileError, Resource, ResourceGraph, SecretName, Value};

use crate::keys::{self, Lookup};
use crate::{refuse, refuse_at, secret_reference, Kind, OutputPart, OutputValue, Unit, Units};

/// The params that are sections of the unit file and not keys of its own
/// section.
const SECTIONS: &[(&str, &str)] = &[
    ("install", "Install"),
    ("quadlet", "Quadlet"),
    ("service", "Service"),
    ("unit", "Unit"),
];

/// What the rest of the document can call one resource.
struct Names {
    kind: Kind,
    file: String,
    service: String,
    /// `None` when only systemd will know it, because the name is built from
    /// a specifier such as `%H`.
    runtime: Option<String>,
}

type NameTable = BTreeMap<String, Names>;

/// Maps the tree onto units. It does no ordering work.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Units, CompileError> {
    let mut names = NameTable::new();
    for r in &graph.resources {
        let Some(kind) = Kind::from_type(&r.r#type) else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_RESOURCE_TYPE,
                format!(
                    "{:?} is not something Podman runs. This target accepts: {}",
                    r.r#type,
                    Kind::ALL.map(Kind::as_str).join(", ")
                ),
            ));
        };
        names.insert(r.id.clone(), names_of(r, kind)?);
    }

    let mut units = Units {
        document: graph.name.clone(),
        ..Units::default()
    };
    for r in &graph.resources {
        check_meta(r)?;
        units.units.push(unit(r, &names, &graph.secrets)?);
    }

    for output in &graph.outputs {
        let parts = output_parts(&output.value, &names).map_err(|msg| {
            refuse_at(
                "",
                &output.pos,
                &rules::INVALID_REFERENCE,
                format!("output {:?}: {msg}", output.name),
            )
        })?;
        units.outputs.push(OutputValue {
            name: output.name.clone(),
            parts,
            pos: output.pos.clone(),
        });
    }
    Ok(units)
}

/// A param that must be one plain string when it is given at all.
fn text_param<'a>(r: &'a Resource, key: &str) -> Result<Option<&'a str>, CompileError> {
    match r.params.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s)),
        Some(_) => Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!("param {key:?} must be a string that is not empty"),
        )),
    }
}

fn names_of(r: &Resource, kind: Kind) -> Result<Names, CompileError> {
    let service = match text_param(r, "ServiceName")? {
        Some(name) => format!("{name}.service"),
        None => kind.service(&r.id),
    };
    let default = || Some(format!("systemd-{}", r.id));
    // A name systemd or Meridian still has to fill in is not known here.
    let fixed = |name: &str| (!name.contains('%') && !name.contains("${")).then(|| name.to_owned());
    let runtime = match kind {
        Kind::Container => text_param(r, "ContainerName")?.map_or_else(default, fixed),
        Kind::Pod => text_param(r, "PodName")?.map_or_else(default, fixed),
        Kind::Volume => text_param(r, "VolumeName")?.map_or_else(default, fixed),
        Kind::Network => text_param(r, "NetworkName")?.map_or_else(default, fixed),
        Kind::Image => text_param(r, "Image")?.and_then(fixed),
        Kind::Build => match r.params.get("ImageTag") {
            Some(Value::String(tag)) => fixed(tag),
            Some(Value::List(tags)) => tags.first().and_then(Value::as_str).and_then(fixed),
            _ => None,
        },
        Kind::Kube => None,
    };
    Ok(Names {
        kind,
        file: format!("{}.{}", r.id, kind.as_str()),
        service,
        runtime,
    })
}

/// The parts of a resource that mean something to other tools and nothing to
/// Quadlet.
fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a unit's \
                 start conditions are systemd's fixed Condition*= tests, not an expression. \
                 Write the one you need under the unit param, for example \
                 unit: {{ConditionPathExists: /etc/app.conf}}",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" | "running" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" has no meaning on this target: a unit that is no longer in the \
             document is stopped and removed by the next apply, so remove the resource from \
             the document"
                .to_owned(),
        )),
        other => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            format!(
                "resource type {:?} does not accept state {other:?} on this target \
                 (accepted: present, running)",
                r.r#type
            ),
        )),
    }
}

fn unit(r: &Resource, names: &NameTable, secrets: &[SecretName]) -> Result<Unit, CompileError> {
    let me = &names[&r.id];
    let kind = me.kind;
    let mut unit = Unit {
        id: r.id.clone(),
        kind,
        service: me.service.clone(),
        runtime_name: me.runtime.clone(),
        body: BTreeMap::new(),
        sections: BTreeMap::new(),
        linked: BTreeSet::new(),
        pos: r.pos.clone(),
    };
    let (environment, secret_lines) = split_secrets(r, kind, secrets)?;

    for (key, value) in &r.params {
        // A container's `Environment` without its secrets, which became
        // `Secret=` lines instead.
        let value = match &environment {
            Some(rest) if key == "Environment" => rest,
            _ => value,
        };
        if matches!(value, Value::Map(m) if m.is_empty()) && key == "Environment" {
            continue;
        }
        let invalid =
            |msg: String| refuse(r, &rules::INVALID_PARAM, format!("param {key:?}: {msg}"));
        let bad_reference = |msg: String| {
            refuse(
                r,
                &rules::INVALID_REFERENCE,
                format!("param {key:?}: {msg}"),
            )
        };

        if let Some((_, section)) = SECTIONS.iter().find(|(param, _)| param == key) {
            let Value::Map(entries) = value else {
                return Err(invalid(format!(
                    "the [{section}] section is written as a map of its keys, not {}",
                    value.describe()
                )));
            };
            let mut rendered = BTreeMap::new();
            for (name, entry) in entries {
                let lines = render(entry, names, &mut unit.linked).map_err(|e| match e {
                    Failed::Value(msg) => invalid(format!("{name}: {msg}")),
                    Failed::Reference(msg) => bad_reference(format!("{name}: {msg}")),
                })?;
                if !lines.is_empty() {
                    rendered.insert(name.clone(), lines);
                }
            }
            // An empty map still counts as said: `install: {}` is how a
            // document asks for no [Install] section at all.
            unit.sections.insert(section, rendered);
            continue;
        }

        match keys::lookup(kind, key) {
            Lookup::Miscapitalised(right) => {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "param {key:?} is Quadlet's {right:?} in another capitalisation; \
                         Quadlet reads keys exactly, so write {right:?}"
                    ),
                ))
            }
            Lookup::Unknown if !key.starts_with(|c: char| c.is_ascii_uppercase()) => {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "param {key:?} is not a Quadlet key of a {} unit, which all start with \
                         a capital letter (Image, Volume), nor one of the other sections of \
                         the file: unit, service, install, quadlet",
                        kind.as_str()
                    ),
                ))
            }
            // An unknown key that could be a newer Podman's is kept, and
            // warned about once the whole unit is known.
            Lookup::Known | Lookup::Unknown => {}
        }

        let lines = render(value, names, &mut unit.linked).map_err(|e| match e {
            Failed::Value(msg) => invalid(msg),
            Failed::Reference(msg) => bad_reference(msg),
        })?;
        if !lines.is_empty() {
            unit.body.insert(key.clone(), lines);
        }
    }
    unit.linked.remove(&r.id);
    if !secret_lines.is_empty() {
        unit.body
            .entry("Secret".to_owned())
            .or_default()
            .extend(secret_lines);
    }

    // Something that runs should still be running after a reboot. Quadlet
    // units cannot be enabled with systemctl, so the unit says so itself.
    if kind.is_workload() && !unit.sections.contains_key("Install") {
        unit.section_mut("Install")
            .insert("WantedBy".to_owned(), vec!["default.target".to_owned()]);
    }
    Ok(unit)
}

/// Takes the secrets out of a container's `Environment` map: each becomes a
/// `Secret=<podman secret>,type=env,target=<variable>` line, and the map
/// comes back without them. A secret anywhere else in the unit is refused,
/// because a unit file is installed on the host for anyone who can read the
/// directory, and Quadlet has no other key that reads a Podman secret into
/// a setting.
fn split_secrets(
    r: &Resource,
    kind: Kind,
    secrets: &[SecretName],
) -> Result<(Option<Value>, Vec<String>), CompileError> {
    let mut lines = Vec::new();
    let mut environment = None;
    for (key, value) in &r.params {
        if !holds_secret(value) {
            continue;
        }
        if kind == Kind::Container && key == "Environment" {
            if let Value::Map(entries) = value {
                let mut rest = BTreeMap::new();
                for (variable, entry) in entries {
                    match entry {
                        Value::Secret(slot) => {
                            let name = secrets
                                .iter()
                                .find(|s| s.secret == *slot)
                                .map(|s| s.name.as_str())
                                .unwrap_or(meridian_sdk::SENSITIVE);
                            lines.push(secret_reference(name, variable));
                        }
                        other if holds_secret(other) => {
                            return Err(refuse(
                                r,
                                &rules::UNSUPPORTED_SECRET_PLACE,
                                format!(
                                    "param \"Environment\": {variable} holds a secret inside a \
                                     value; a variable's value is one string, so write the secret \
                                     as the whole value"
                                ),
                            ))
                        }
                        other => {
                            rest.insert(variable.clone(), other.clone());
                        }
                    }
                }
                environment = Some(Value::Map(rest));
                continue;
            }
        }
        let where_else = if kind == Kind::Container {
            "put it in Environment as a map, where it becomes a Podman secret the container \
             reads into the variable"
        } else {
            "only a container's Environment can take one, as a Podman secret the container \
             reads into a variable"
        };
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_SECRET_PLACE,
            format!(
                "param {key:?} of the {} holds a secret, and a unit file is written onto the \
                 host as it stands, where a secret cannot go; {where_else}",
                kind.as_str()
            ),
        ));
    }
    Ok((environment, lines))
}

/// Why a value could not be written.
enum Failed {
    Value(String),
    Reference(String),
}

/// A value as the lines of a unit file: one for a scalar, one per item for a
/// list, one `name=value` per entry for a map.
fn render(
    value: &Value,
    names: &NameTable,
    linked: &mut BTreeSet<String>,
) -> Result<Vec<String>, Failed> {
    match value {
        Value::List(items) => {
            let mut lines = Vec::new();
            for item in items {
                match item {
                    Value::List(_) => {
                        return Err(Failed::Value(
                            "a list inside a list has no meaning in a unit file".to_owned(),
                        ))
                    }
                    other => lines.extend(render(other, names, linked)?),
                }
            }
            Ok(lines)
        }
        Value::Map(entries) => {
            let mut lines = Vec::new();
            for (name, entry) in entries {
                if matches!(entry, Value::List(_) | Value::Map(_)) {
                    return Err(Failed::Value(format!(
                        "{name}: a map's values are written as name=value, so each must be a \
                         single value, not {}",
                        entry.describe()
                    )));
                }
                let text = scalar(entry, names, linked)?;
                lines.push(word(&format!("{name}={}", literal(&text))));
            }
            Ok(lines)
        }
        other => Ok(vec![scalar(other, names, linked)?]),
    }
}

fn scalar(
    value: &Value,
    names: &NameTable,
    linked: &mut BTreeSet<String>,
) -> Result<String, Failed> {
    let text = match value {
        Value::Null => {
            return Err(Failed::Value(
                "it has no value; leave the key out to leave it unset".to_owned(),
            ))
        }
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Uint(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::String(s) => rewrite(s, names, linked).map_err(Failed::Reference)?,
        // A plugin is never sent a secret as a value; one would be written as its mask.
        Value::Secret(_) => meridian_sdk::SENSITIVE.to_owned(),
        Value::List(_) | Value::Map(_) => unreachable!("render handles collections"),
    };
    if text.contains(['\n', '\r']) {
        return Err(Failed::Value(
            "a value in a unit file is one line, and this one has a line break".to_owned(),
        ));
    }
    Ok(text)
}

/// Makes text mean itself to systemd, which otherwise expands `%h` and
/// `${VAR}` in what Quadlet hands it.
fn literal(text: &str) -> String {
    text.replace('%', "%%").replace('$', "$$")
}

/// One word of a setting that systemd splits on spaces, quoted when it would
/// otherwise be split or lose a character.
fn word(text: &str) -> String {
    if !text.contains([' ', '\t', '"', '\'', '\\']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// What `${name...}` may read from a resource.
const ATTRIBUTES: &str = "its unit file (${ID}), ${ID.name} and ${ID.service}";

/// One `${...}` of a param or an output, resolved as far as a compile can.
fn resolve(expression: &str, names: &NameTable) -> Result<Resolved, String> {
    let (name, rest) = expr::head(expression);
    let Some(target) = names.get(name) else {
        return Err(if expr::is_path(expression) {
            format!(
                "${{{expression}}} refers to {name:?}, which is not a resource in this \
                 document. To hand ${{{expression}}} to systemd as written, escape it: \
                 $${{{expression}}}"
            )
        } else {
            format!(
                "${{{expression}}} is not a reference to a resource. To hand it to systemd \
                 as written, escape it: $${{{expression}}}"
            )
        });
    };
    match rest {
        "" => Ok(Resolved::File(name.to_owned(), target.file.clone())),
        ".service" => Ok(Resolved::Text(target.service.clone())),
        ".name" => match &target.runtime {
            Some(runtime) => Ok(Resolved::Text(runtime.clone())),
            None if target.kind == Kind::Kube => Err(format!(
                "${{{expression}}}: a kube unit has no single name; its pods are named by \
                 its YAML"
            )),
            None => Err(format!(
                "${{{expression}}}: the name of {name:?} is not known until systemd fills \
                 it in, or it has none set"
            )),
        },
        _ => {
            if let Some(path) = rest.strip_prefix(".inspect.") {
                return inspect(expression, name, target, path);
            }
            Err(format!(
                "${{{expression}}} reads {:?}, which this target does not know. A resource \
                 offers {}; an output may also read ${{{name}.inspect.<Field>}}",
                rest.trim_start_matches('.'),
                ATTRIBUTES.replace("ID", name)
            ))
        }
    }
}

enum Resolved {
    /// The other resource's unit file, which also orders this unit after it.
    File(String, String),
    Text(String),
    Inspect(OutputPart),
}

fn inspect(expression: &str, name: &str, target: &Names, path: &str) -> Result<Resolved, String> {
    let noun = match target.kind {
        Kind::Container => "container",
        Kind::Pod => "pod",
        Kind::Volume => "volume",
        Kind::Network => "network",
        Kind::Image | Kind::Build => "image",
        Kind::Kube => {
            return Err(format!(
                "${{{expression}}}: a kube unit is not one thing Podman can inspect"
            ))
        }
    };
    let field = |f: &str| {
        f.starts_with(|c: char| c.is_ascii_alphabetic())
            && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if path.is_empty() || !path.split('.').all(field) {
        return Err(format!(
            "${{{expression}}}: what follows .inspect. is a path of field names from \
             `podman {noun} inspect`, such as NetworkSettings.IPAddress"
        ));
    }
    let Some(runtime) = &target.runtime else {
        return Err(format!(
            "${{{expression}}}: the name of {name:?} is not known until systemd fills it in, \
             so it cannot be inspected"
        ));
    };
    Ok(Resolved::Inspect(OutputPart::Inspect {
        noun,
        name: runtime.clone(),
        path: path.to_owned(),
    }))
}

/// Rewrites every `${id...}` in a param's text.
fn rewrite(text: &str, names: &NameTable, linked: &mut BTreeSet<String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for part in expr::parse(text)? {
        match part {
            Part::Text(literal) => out.push_str(&literal),
            Part::Expr(expression) => match resolve(&expression, names)? {
                Resolved::File(id, file) => {
                    linked.insert(id);
                    out.push_str(&file);
                }
                Resolved::Text(text) => out.push_str(&text),
                Resolved::Inspect(_) => {
                    return Err(format!(
                        "${{{expression}}} is read from Podman once the thing exists, so only \
                         an output can carry it; a unit file is written before anything runs"
                    ))
                }
            },
        }
    }
    Ok(out)
}

/// An output's value, as text known now and values to read after an apply.
fn output_parts(text: &str, names: &NameTable) -> Result<Vec<OutputPart>, String> {
    let mut parts: Vec<OutputPart> = Vec::new();
    let push_text = |parts: &mut Vec<OutputPart>, text: &str| match parts.last_mut() {
        Some(OutputPart::Text(last)) => last.push_str(text),
        _ => parts.push(OutputPart::Text(text.to_owned())),
    };
    for part in expr::parse(text)? {
        match part {
            Part::Text(literal) => push_text(&mut parts, &literal),
            Part::Expr(expression) => match resolve(&expression, names)? {
                Resolved::File(_, file) => push_text(&mut parts, &file),
                Resolved::Text(text) => push_text(&mut parts, &text),
                Resolved::Inspect(part) => parts.push(part),
            },
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_quoted_only_when_systemd_would_split_or_change_it() {
        assert_eq!(word("A=1"), "A=1");
        assert_eq!(word("A=two words"), "\"A=two words\"");
        assert_eq!(word("A=say \"hi\""), "\"A=say \\\"hi\\\"\"");
        assert_eq!(word("A=c:\\dir"), "\"A=c:\\\\dir\"");
        assert_eq!(word("A=it's"), "\"A=it's\"");
    }

    #[test]
    fn literal_text_survives_systemds_expansion() {
        assert_eq!(literal("100%"), "100%%");
        assert_eq!(literal("pa$$word ${HOME}"), "pa$$$$word $${HOME}");
    }
}
