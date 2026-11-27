//! Stage one: each resource becomes an element of the Compose file, and every
//! reference becomes the name it stands for.

use std::collections::BTreeMap;

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, SecretName, Value};

use crate::{
    refuse, refuse_at, secret_reference, secret_variable, Composition, Element, OutputPart,
    Project, Section,
};

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Composition, CompileError> {
    let mut composition = Composition {
        project: Project {
            name: graph.name.clone(),
            host: daemon_of(&graph.host),
            wait: true,
            ..Project::default()
        },
        ..Composition::default()
    };
    let mut project_seen = false;

    let sections: BTreeMap<&str, Section> = graph
        .resources
        .iter()
        .filter_map(|r| {
            let section = Section::ALL
                .into_iter()
                .find(|s| s.type_name() == r.r#type)?;
            Some((r.id.as_str(), section))
        })
        .collect();

    for r in &graph.resources {
        check_meta(r)?;
        if r.r#type == "project" {
            if project_seen {
                return Err(refuse(
                    r,
                    &rules::DUPLICATE_RESOURCE,
                    "a document is one Compose project; this one has two resources of type \"project\""
                        .to_owned(),
                ));
            }
            project_seen = true;
            read_project(r, &mut composition.project)?;
            continue;
        }
        let Some(section) = sections.get(r.id.as_str()).copied() else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_RESOURCE_TYPE,
                format!(
                    "{:?} is not something a Compose file declares (supported: config, network, \
                     secret, service, volume, and \"project\" for where it runs)",
                    r.r#type
                ),
            ));
        };
        // `adopt: replace` is the only adoption this target takes: Docker
        // keeps no record of what it manages and cannot import an existing
        // container by an identifier, so a run may only replace it.
        let adopt = if r.adopt.is_empty() {
            false
        } else {
            if r.adopt != "replace" {
                return Err(refuse(
                    r,
                    &rules::INVALID_ADOPT,
                    format!(
                        "adopt {:?}: docker takes only `adopt: replace`; it keeps no record of \
                         what it manages and cannot import a container by an id",
                        r.adopt
                    ),
                ));
            }
            if section != Section::Service {
                return Err(refuse(
                    r,
                    &rules::INVALID_ADOPT,
                    format!(
                        "adopt: replace on {:?} is a {}; only a service (a container) can be \
                         taken over by replacing it",
                        r.id,
                        section.type_name()
                    ),
                ));
            }
            true
        };
        let mut body = BTreeMap::new();
        for (key, value) in &r.params {
            // A secret element's `value` is the secret itself: Compose reads
            // it from the variable the runner's environment file sets.
            if section == Section::Secret && key == "value" {
                let Value::Secret(slot) = value else {
                    return Err(refuse(
                        r,
                        &rules::INVALID_PARAM,
                        "param \"value\" of a secret is the secret's value, so it must be a \
                         secret, ${{ secret.<name> }}; to read a file or a variable of your \
                         own, write file or environment instead"
                            .to_owned(),
                    ));
                };
                let name = secret_name(*slot, &graph.secrets);
                body.insert(
                    "environment".to_owned(),
                    Value::String(secret_variable(&name)),
                );
                continue;
            }
            let rewritten = rewrite(value, &sections, &graph.secrets).map_err(|msg| {
                refuse(
                    r,
                    &rules::INVALID_REFERENCE,
                    format!("param {key:?}: {msg}"),
                )
            })?;
            body.insert(key.clone(), rewritten);
        }
        composition.elements.push(Element {
            id: r.id.clone(),
            section,
            body,
            pos: r.pos.clone(),
            adopt,
        });
    }

    for output in &graph.outputs {
        let fail = |msg: String| {
            refuse_at(
                "",
                &output.pos,
                &rules::INVALID_REFERENCE,
                format!("output {:?}: {msg}", output.name),
            )
        };
        let mut parts = Vec::new();
        for part in expr::parse(&output.value).map_err(&fail)? {
            let expression = match part {
                Part::Text(text) => {
                    parts.push(OutputPart::Text(text));
                    continue;
                }
                Part::Expr(expression) => expression,
            };
            let (name, path) = expr::head(&expression);
            if sections.get(name) != Some(&Section::Service) {
                return Err(fail(format!(
                    "${{{expression}}} refers to {name:?}, which is not a service in this document"
                )));
            }
            let service = name.to_owned();
            if let Some(port) = path.strip_prefix(".port.") {
                let (number, protocol) = port.split_once('/').unwrap_or((port, ""));
                if number.parse::<u16>().is_err() || !matches!(protocol, "" | "tcp" | "udp") {
                    return Err(fail(format!(
                        "${{{expression}}} must name a container port, such as ${{{name}.port.80}}"
                    )));
                }
                parts.push(OutputPart::Port {
                    service,
                    port: port.to_owned(),
                });
            } else if is_inspect_path(path) {
                parts.push(OutputPart::Inspect {
                    service,
                    path: path.to_owned(),
                });
            } else {
                return Err(fail(format!(
                    "${{{expression}}} must be ${{{name}.port.<number>}} for a published port, or \
                     a path into docker inspect such as ${{{name}.State.Status}}"
                )));
            }
        }
        composition.outputs.push((output.name.clone(), parts));
    }
    Ok(composition)
}

/// Whether a path is `.field` and `[index]` steps only, which is all the
/// runner walks.
fn is_inspect_path(path: &str) -> bool {
    path.starts_with('.')
        && !path.ends_with('.')
        && !path.contains("..")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-[]".contains(c))
}

/// The daemon a document's target host stands for. A bare host name is
/// reached over SSH; the machine Meridian runs on needs no address.
fn daemon_of(host: &str) -> String {
    match host {
        "" | "localhost" | "127.0.0.1" | "::1" => String::new(),
        address if address.contains("://") => address.to_owned(),
        host => format!("ssh://{host}"),
    }
}

fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a Compose \
                 file is declared state, with no condition for Docker to evaluate",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" | "running" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" is not written on this target: remove the resource from the \
             document, and bringing the project up removes what the document no longer declares"
                .to_owned(),
        )),
        other => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            format!(
                "resource type {:?} does not accept state {other:?} on this target (accepted: present, running)",
                r.r#type
            ),
        )),
    }
}

fn read_project(r: &Resource, project: &mut Project) -> Result<(), CompileError> {
    let invalid = |msg: String| refuse(r, &rules::INVALID_PARAM, msg);
    for (name, value) in &r.params {
        match (name.as_str(), value) {
            ("name", Value::String(text)) => project.name = text.clone(),
            ("host", Value::String(text)) => project.host = daemon_of(text),
            ("context", Value::String(text)) => project.context = text.clone(),
            ("directory", Value::String(text)) if text.starts_with(['/', '~']) => {
                project.directory = text.clone();
            }
            ("directory", _) => {
                return Err(invalid(
                    "param \"directory\" of the project must be an absolute path: it is the \
                     directory Compose resolves the file's relative paths against"
                        .to_owned(),
                ))
            }
            ("name" | "host" | "context", _) => {
                return Err(invalid(format!(
                    "param {name:?} of the project must be a string"
                )))
            }
            ("profiles", Value::List(items)) => {
                for item in items {
                    let Value::String(profile) = item else {
                        return Err(invalid("every profile must be a string".to_owned()));
                    };
                    project.profiles.push(profile.clone());
                }
            }
            ("profiles", _) => {
                return Err(invalid(
                    "param \"profiles\" of the project must be a list".to_owned(),
                ))
            }
            ("wait", Value::Bool(wait)) => project.wait = *wait,
            ("wait", _) => {
                return Err(invalid(
                    "param \"wait\" of the project must be true or false".to_owned(),
                ))
            }
            ("wait_timeout", Value::Int(seconds)) if *seconds > 0 => {
                project.wait_timeout = seconds.unsigned_abs();
            }
            ("wait_timeout", _) => {
                return Err(invalid(
                    "param \"wait_timeout\" of the project must be a whole number of seconds, \
                     greater than zero"
                        .to_owned(),
                ))
            }
            _ => {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "resource type \"project\" does not accept param {name:?} on this \
                         target (accepted: context, directory, host, name, profiles, wait, \
                         wait_timeout)"
                    ),
                ))
            }
        }
    }
    Ok(())
}

/// Replaces every reference in a value with the name it stands for.
fn rewrite(
    value: &Value,
    sections: &BTreeMap<&str, Section>,
    secrets: &[SecretName],
) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => Value::String(rewrite_string(s, sections)?),
        // Written as Compose's own interpolation, which `literal` must not
        // escape, so a secret does not go through `rewrite_string`.
        Value::Secret(slot) => Value::String(secret_reference(&secret_name(*slot, secrets))),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| rewrite(item, sections, secrets))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, sections, secrets)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

/// The declared name of a secret's slot. A slot the graph does not name
/// cannot be handed over, so it is written as the mask.
fn secret_name(slot: meridian_sdk::SecretRef, secrets: &[SecretName]) -> String {
    secrets
        .iter()
        .find(|s| s.secret == slot)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| meridian_sdk::SENSITIVE.to_owned())
}

/// A `$` that Compose would otherwise take for a variable of its own.
fn literal(text: &str) -> String {
    text.replace('$', "$$")
}

fn rewrite_string(text: &str, sections: &BTreeMap<&str, Section>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for part in expr::parse(text)? {
        match part {
            Part::Text(text) => out.push_str(&literal(&text)),
            Part::Expr(expression) => {
                let (name, path) = expr::head(&expression);
                if !sections.contains_key(name) {
                    return Err(format!(
                        "${{{expression}}} refers to {name:?}, which is not an element of this document"
                    ));
                }
                if !path.is_empty() {
                    return Err(format!(
                        "${{{expression}}} is only known once the project is up. Inside params a \
                         reference can be ${{{name}}}, the element's name; a published port or a \
                         path into docker inspect can be read in an output"
                    ));
                }
                out.push_str(name);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_host_is_a_daemon_reached_over_ssh() {
        assert_eq!(daemon_of(""), "");
        assert_eq!(daemon_of("localhost"), "");
        assert_eq!(daemon_of("web01.example.com"), "ssh://web01.example.com");
        assert_eq!(daemon_of("deploy@web01"), "ssh://deploy@web01");
        assert_eq!(daemon_of("tcp://10.0.0.5:2376"), "tcp://10.0.0.5:2376");
    }

    #[test]
    fn an_inspect_path_is_fields_and_indexes() {
        assert!(is_inspect_path(".State.Status"));
        assert!(is_inspect_path(".Mounts[0].Source"));
        assert!(!is_inspect_path("State"));
        assert!(!is_inspect_path(".State."));
        assert!(!is_inspect_path(".a..b"));
    }
}
