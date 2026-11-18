//! Stage one: the document's settings become the runner's, each resource
//! becomes a DSC resource instance, and every reference becomes the runner's
//! own accessor.

use std::collections::BTreeMap;

use meridian_sdk::expr::{self, Part};
use meridian_sdk::{CompileError, Position, ResourceGraph, Value, Warning};

use crate::{refuse, refuse_at, Configuration, Instance, Settings, NAME};

const ENGINES: [&str; 3] = ["Auto", "DscV2", "DscV3"];
const TARGETS: [&str; 3] = ["Local", "SSH", "WinRM"];

/// The keys of `spec.settings` this target reads, in the order they are
/// listed to a reader.
pub(crate) const SETTINGS_KEYS: [&str; 3] = ["credential", "engine", "target"];

/// The resource type that carried the settings before `spec.settings`
/// existed. Still read, with a warning that says where to move it.
pub(crate) const RUNNER_TYPE: &str = "runner";

/// Whether a type is shaped like a DSC resource: `Module/Resource`.
fn is_dsc_type(t: &str) -> bool {
    let part = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    };
    t.split_once('/')
        .is_some_and(|(module, resource)| part(module) && part(resource))
}

pub(crate) fn transform(
    graph: &ResourceGraph,
    warnings: &mut Vec<Warning>,
) -> Result<Configuration, CompileError> {
    let mut configuration = Configuration {
        node: graph.name.clone(),
        host: graph.host.clone(),
        settings: Settings {
            engine: "Auto".to_owned(),
            ..Settings::default()
        },
        instances: Vec::new(),
    };
    let mut runner_seen = false;
    // The generation of DSC the document asked for, as the engine that runs
    // it. The host only hands over a version this target declared.
    let asked = match graph.target_version.split('.').next() {
        Some("2") => Some("DscV2"),
        Some("3") => Some("DscV3"),
        _ => None,
    };
    if let Some(engine) = asked {
        configuration.settings.engine = engine.to_owned();
    }

    // The settings proper: spec.settings, which the host has already checked
    // against the keys this target lists.
    let mut given: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, value) in &graph.settings {
        let text = read_setting(name, value, &mut configuration.settings, |rule, msg| {
            refuse_at("", &Position::default(), rule, msg)
        })?;
        given.insert(name.as_str(), text);
    }
    if let (Some(engine), true) = (asked, graph.settings.contains_key("engine")) {
        if configuration.settings.engine != engine {
            return Err(refuse_at(
                "",
                &Position::default(),
                "conflicting-engine",
                format!(
                    "the document's targetVersion {:?} means the engine {engine}, and \
                     spec.settings.engine asks for {}. Say it once: drop one of the two",
                    graph.target_version, configuration.settings.engine
                ),
            ));
        }
    }

    let ids: Vec<&str> = graph
        .resources
        .iter()
        .filter(|r| r.r#type != RUNNER_TYPE)
        .map(|r| r.id.as_str())
        .collect();

    for r in &graph.resources {
        if r.r#type == RUNNER_TYPE {
            if runner_seen {
                return Err(refuse(
                    r,
                    "duplicate-runner",
                    "a document is run one way; this one has two resources of type \"runner\". \
                     Write the settings once, under spec.settings"
                        .to_owned(),
                ));
            }
            runner_seen = true;
            // The runner resource is settings, not something DSC applies, so
            // there is no moment a condition on it could be checked.
            if !r.runtime_when.is_empty() {
                return Err(refuse(
                    r,
                    "unsupported-runtime-condition",
                    format!(
                        "resource carries runtimeWhen {:?}, which a \"runner\" cannot express: \
                         it says how the configuration is run and is not applied to the node. \
                         Put the condition on the DSC resources it should guard",
                        r.runtime_when
                    ),
                ));
            }
            warnings.push(Warning {
                target: NAME.to_owned(),
                resource: r.id.clone(),
                msg: format!(
                    "a resource of type \"runner\" is the old way to say how the configuration \
                     is run. Move its params ({}) to spec.settings; the resource type will be \
                     removed in the next apiVersion",
                    r.params
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
            for (name, value) in &r.params {
                let before = configuration.settings.clone();
                let text = read_setting(name, value, &mut configuration.settings, |rule, msg| {
                    refuse(r, rule, msg)
                })?;
                if let Some(earlier) = given.get(name.as_str()) {
                    if !earlier.eq_ignore_ascii_case(text) {
                        return Err(refuse(
                            r,
                            "conflicting-settings",
                            format!(
                                "spec.settings.{name} is {earlier:?} and the \"runner\" \
                                 resource says {text:?}. Say it once, under spec.settings"
                            ),
                        ));
                    }
                    configuration.settings = before;
                }
            }
            if let Some(engine) = asked {
                if configuration.settings.engine != engine {
                    return Err(refuse(
                        r,
                        "conflicting-engine",
                        format!(
                            "the document's targetVersion {:?} means the engine {engine}, and \
                             the runner asks for {}. Say it once: drop one of the two",
                            graph.target_version, configuration.settings.engine
                        ),
                    ));
                }
            }
            continue;
        }
        if !is_dsc_type(&r.r#type) {
            return Err(refuse(
                r,
                "unsupported-resource-type",
                format!(
                    "{:?} is not a DSC resource. Write the module and the resource, such as \
                     PSDscResources/Service or Microsoft.Windows/Registry. How the \
                     configuration is run goes under spec.settings, not in a resource",
                    r.r#type
                ),
            ));
        }
        if !r.state.is_empty() {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "state {:?} is not written on this target: a DSC resource carries its \
                     desired state in its own properties, such as Ensure or _exist, so set it \
                     in params",
                    r.state
                ),
            ));
        }

        let mut reads = Vec::new();
        let mut properties = BTreeMap::new();
        for (key, value) in &r.params {
            let rewritten = rewrite(value, &ids, &r.id, &mut reads)
                .map_err(|msg| refuse(r, "invalid-reference", format!("param {key:?}: {msg}")))?;
            properties.insert(key.clone(), rewritten);
        }
        reads.sort();
        reads.dedup();
        configuration.instances.push(Instance {
            id: r.id.clone(),
            r#type: r.r#type.clone(),
            properties,
            pre_condition: r.runtime_when.clone(),
            reads,
            depends_on: Vec::new(),
            notify: Vec::new(),
            pos: r.pos.clone(),
        });
    }

    if let Some(output) = graph.outputs.first() {
        return Err(refuse_at(
            "",
            &output.pos,
            "unsupported-output",
            format!(
                "output {:?}: this target reports no outputs. DscPipelineRunner keeps what a \
                 resource returns inside its own run",
                output.name
            ),
        ));
    }

    // Running on another machine has to be asked for: the runner cannot
    // guess between WinRM and SSH, and getting it wrong configures nothing.
    let local = matches!(graph.host.as_str(), "" | "localhost" | "127.0.0.1" | "::1");
    if configuration.settings.target.is_empty() {
        if !local {
            return Err(CompileError {
                target: NAME.to_owned(),
                rule: "missing-param".to_owned(),
                msg: format!(
                    "the document's host is {:?}, and nothing says how the runner reaches it. \
                     Set spec.settings.target to WinRM or SSH, or to Local when Meridian runs \
                     on the host itself",
                    graph.host
                ),
                ..CompileError::default()
            });
        }
        configuration.settings.target = "Local".to_owned();
    }
    Ok(configuration)
}

/// Reads one setting into the runner's, whether it was written under
/// `spec.settings` or on the old `runner` resource, and gives back its text.
/// A setting that does not name an engine leaves the one already chosen.
fn read_setting<'v>(
    name: &str,
    value: &'v Value,
    settings: &mut Settings,
    refuse: impl Fn(&str, String) -> CompileError,
) -> Result<&'v str, CompileError> {
    let Value::String(text) = value else {
        return Err(refuse(
            "invalid-setting",
            format!("setting {name:?} must be a string"),
        ));
    };
    let one_of = |choices: &[&str]| -> Result<String, CompileError> {
        choices
            .iter()
            .find(|c| c.eq_ignore_ascii_case(text))
            .map(|c| (*c).to_owned())
            .ok_or_else(|| {
                refuse(
                    "invalid-setting",
                    format!(
                        "setting {name:?} cannot be {text:?} (accepted: {})",
                        choices.join(", ")
                    ),
                )
            })
    };
    match name {
        "engine" => settings.engine = one_of(&ENGINES)?,
        "target" => settings.target = one_of(&TARGETS)?,
        "credential" => settings.credential = text.clone(),
        _ => {
            return Err(refuse(
                "unsupported-setting",
                format!(
                    "this target does not take a setting named {name:?} (accepted: {})",
                    SETTINGS_KEYS.join(", ")
                ),
            ))
        }
    }
    Ok(text)
}

/// Replaces every reference in a value with the runner's accessor for it,
/// and records which resources were read.
fn rewrite(
    value: &Value,
    ids: &[&str],
    own: &str,
    reads: &mut Vec<String>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => Value::String(rewrite_string(s, ids, own, reads)?),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| rewrite(item, ids, own, reads))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, ids, own, reads)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

/// Text the runner would otherwise expand: it treats every property as a
/// PowerShell expandable string, where `$` starts a variable or an
/// expression and a backtick escapes.
fn literal(text: &str) -> String {
    text.replace('`', "``").replace('$', "`$")
}

fn rewrite_string(
    text: &str,
    ids: &[&str],
    own: &str,
    reads: &mut Vec<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for part in expr::parse(text)? {
        match part {
            Part::Text(text) => out.push_str(&literal(&text)),
            Part::Expr(expression) => {
                let (name, path) = expr::head(&expression);
                if !ids.contains(&name) {
                    return Err(format!(
                        "${{{expression}}} refers to {name:?}, which is not a resource in this document"
                    ));
                }
                if name == own {
                    return Err(format!(
                        "${{{expression}}} reads the resource it is written in"
                    ));
                }
                let plain = path.starts_with('.')
                    && !path.ends_with('.')
                    && !path.contains("..")
                    && path
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._[]".contains(c));
                if !plain {
                    return Err(format!(
                        "${{{expression}}} must read a property of the resource, such as \
                         ${{{name}.Id}}"
                    ));
                }
                out.push_str(&format!("$((reference '{name}'){path})"));
                reads.push(name.to_owned());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_is_a_module_and_a_resource() {
        for good in [
            "PSDscResources/Service",
            "Microsoft.Windows/Registry",
            "Microsoft.DSC.Transitional/RunCommandOnSet",
        ] {
            assert!(is_dsc_type(good), "{good}");
        }
        for bad in ["Service", "package", "a/b/c", "/Service", "Module/"] {
            assert!(!is_dsc_type(bad), "{bad}");
        }
    }

    #[test]
    fn text_is_kept_from_powershell() {
        assert_eq!(literal("C:\\Program Files"), "C:\\Program Files");
        assert_eq!(literal("costs $5"), "costs `$5");
        assert_eq!(literal("a `tick`"), "a ``tick``");
    }
}
