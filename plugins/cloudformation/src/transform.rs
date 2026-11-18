//! Stage one: each resource becomes a template resource, and every reference
//! between resources becomes an intrinsic function.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Position, Resource, ResourceGraph, Value};
use meridian_yaml::Yaml;

use crate::{
    logical_id, plain, refuse, refuse_at, secret_key, secret_parameter, Entry, Settings, Template,
};

/// Keys that are attributes of a resource and not properties of its type.
const ATTRIBUTES: &[&str] = &[
    "Condition",
    "CreationPolicy",
    "DeletionPolicy",
    "Metadata",
    "UpdatePolicy",
    "UpdateReplacePolicy",
];

/// `Vendor::Service::Type`, or `Custom::Name`.
fn is_resource_type(t: &str) -> bool {
    let parts: Vec<&str> = t.split("::").collect();
    let word = |w: &&str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric());
    (parts.len() == 3 || (parts.len() == 2 && parts[0] == "Custom")) && parts.iter().all(word)
}

/// Document id to logical id, for every resource that is in the template.
pub(crate) type Logical = BTreeMap<String, String>;

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Template, CompileError> {
    let mut template = Template {
        document: graph.name.clone(),
        settings: Settings {
            name: graph.name.clone(),
            ..Settings::default()
        },
        ..Template::default()
    };

    let mut logical = Logical::new();
    let mut settings_seen = false;
    for r in &graph.resources {
        check_meta(r)?;
        if r.r#type == "stack" {
            if settings_seen {
                return Err(refuse(
                    r,
                    &rules::DUPLICATE_RESOURCE,
                    "a document deploys one stack; this one has two resources of type \"stack\""
                        .to_owned(),
                ));
            }
            settings_seen = true;
            read_settings(r, &mut template.settings)?;
            continue;
        }
        if !is_resource_type(&r.r#type) {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_RESOURCE_TYPE,
                format!(
                    "{:?} is not a CloudFormation resource type. Use AWS's own type, such as \
                     AWS::EC2::Instance, or \"stack\" for the deployment's settings",
                    r.r#type
                ),
            ));
        }
        let Some(id) = logical_id(&r.id) else {
            return Err(refuse(
                r,
                &rules::INVALID_NAME,
                format!(
                    "resource id {:?} has no letters or digits to make a logical id from",
                    r.id
                ),
            ));
        };
        logical.insert(r.id.clone(), id);
    }

    // Each secret is a parameter, referred to in a param by its key. A
    // parameter's name is letters and digits, so two secrets, or a secret and
    // a resource, can come down to the same one; that is refused rather than
    // silently merged.
    let mut parameters = BTreeSet::new();
    for secret in &graph.secrets {
        let Some(parameter) = secret_parameter(&secret.name) else {
            return Err(refuse_at(
                "",
                &Position::default(),
                &rules::INVALID_NAME,
                format!(
                    "secret {:?} has no letters or digits to make a parameter name from",
                    secret.name
                ),
            ));
        };
        if let Some((id, _)) = logical.iter().find(|(_, l)| **l == parameter) {
            return Err(refuse_at(
                "",
                &Position::default(),
                &rules::INVALID_NAME,
                format!(
                    "secret {:?} would be the parameter {parameter}, which is also what {id:?} \
                     is called in the template; rename one of them",
                    secret.name
                ),
            ));
        }
        logical.insert(secret_key(&secret.name), parameter.clone());
        parameters.insert(secret_key(&secret.name));
        template.parameters.push(parameter);
    }

    for r in graph.resources.iter().filter(|r| r.r#type != "stack") {
        let mut refs = BTreeSet::new();
        let mut properties = Vec::new();
        let mut attributes = Vec::new();
        for (name, value) in &r.params {
            let rewritten = rewrite(value, &logical, &mut refs).map_err(|msg| {
                refuse(
                    r,
                    &rules::INVALID_REFERENCE,
                    format!("param {name:?}: {msg}"),
                )
            })?;
            if ATTRIBUTES.contains(&name.as_str()) {
                attributes.push((name.clone(), rewritten));
            } else {
                properties.push((name.clone(), rewritten));
            }
        }
        refs.remove(&r.id);
        // A parameter is not a resource to order after.
        refs.retain(|r| !parameters.contains(r));
        template.entries.push(Entry {
            id: r.id.clone(),
            logical: logical[&r.id].clone(),
            r#type: r.r#type.clone(),
            properties,
            attributes,
            depends_on: Vec::new(),
            refs,
            pos: r.pos.clone(),
        });
    }

    for output in &graph.outputs {
        let mut refs = BTreeSet::new();
        let value =
            rewrite(&Value::String(output.value.clone()), &logical, &mut refs).map_err(|msg| {
                refuse_at(
                    "",
                    &output.pos,
                    &rules::INVALID_REFERENCE,
                    format!("output {:?}: {msg}", output.name),
                )
            })?;
        let Some(id) = logical_id(&output.name) else {
            return Err(refuse_at(
                "",
                &output.pos,
                &rules::INVALID_NAME,
                format!(
                    "output name {:?} has no letters or digits to make a logical id from",
                    output.name
                ),
            ));
        };
        template
            .outputs
            .push((output.name.clone(), id, value, output.pos.clone()));
    }
    Ok(template)
}

fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a \
                 CloudFormation condition is decided from the template's parameters before \
                 anything is deployed, not from the state of what it deploys to",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" has no meaning on this target: CloudFormation deletes whatever \
             its template no longer declares, so remove the resource from the document"
                .to_owned(),
        )),
        other => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            format!(
                "resource type {:?} does not accept state {other:?} on this target (accepted: present)",
                r.r#type
            ),
        )),
    }
}

fn read_settings(r: &Resource, settings: &mut Settings) -> Result<(), CompileError> {
    for (name, value) in &r.params {
        match (name.as_str(), value) {
            ("name", Value::String(s)) => settings.name = s.clone(),
            ("region", Value::String(s)) => settings.region = s.clone(),
            ("capabilities", Value::List(items)) => {
                for item in items {
                    let Value::String(capability) = item else {
                        return Err(refuse(
                            r,
                            &rules::INVALID_PARAM,
                            "every capability must be a string, such as CAPABILITY_NAMED_IAM"
                                .to_owned(),
                        ));
                    };
                    settings.capabilities.push(capability.clone());
                }
            }
            ("name" | "region", _) => {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    format!("param {name:?} of the stack must be a string"),
                ))
            }
            ("capabilities", _) => {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    "param \"capabilities\" of the stack must be a list".to_owned(),
                ))
            }
            _ => {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "resource type \"stack\" does not accept param {name:?} on this target \
                         (accepted: capabilities, name, region)"
                    ),
                ))
            }
        }
    }
    Ok(())
}

/// Rewrites every `${id}` and `${id.Attribute}` in a value to the intrinsic
/// function that reads it, recording which resources were referred to.
fn rewrite(value: &Value, logical: &Logical, refs: &mut BTreeSet<String>) -> Result<Yaml, String> {
    Ok(match value {
        Value::String(s) => rewrite_string(s, logical, refs)?,
        Value::List(items) => Yaml::Seq(
            items
                .iter()
                .map(|item| rewrite(item, logical, refs))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Yaml::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, logical, refs)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => plain(other),
    })
}

/// One `${...}` resolved: the logical name to `Ref`, and the attribute to
/// read from it, if any.
struct Target {
    name: String,
    attribute: String,
}

fn resolve(
    expression: &str,
    logical: &Logical,
    refs: &mut BTreeSet<String>,
) -> Result<Target, String> {
    // `AWS::Region` and its kind are pseudo parameters, CloudFormation's own.
    if let Some(pseudo) = expression.strip_prefix("AWS::") {
        if !pseudo.is_empty() && pseudo.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(Target {
                name: expression.to_owned(),
                attribute: String::new(),
            });
        }
    }
    let (name, rest) = expr::head(expression);
    let Some(id) = logical.get(name) else {
        return Err(format!(
            "${{{expression}}} refers to {name:?}, which is not a resource in this document"
        ));
    };
    let attribute = match rest.strip_prefix('.') {
        Some(attribute) if !attribute.is_empty() => attribute,
        None if rest.is_empty() => "",
        _ => {
            return Err(format!(
                "${{{expression}}} is not a reference this target can read: write ${{{name}}} \
                 for the resource itself or ${{{name}.Attribute}} for one of its attributes"
            ))
        }
    };
    refs.insert(name.to_owned());
    Ok(Target {
        name: id.clone(),
        attribute: attribute.to_owned(),
    })
}

fn rewrite_string(
    text: &str,
    logical: &Logical,
    refs: &mut BTreeSet<String>,
) -> Result<Yaml, String> {
    let parts = expr::parse(text)?;
    match parts.as_slice() {
        [] => Ok(Yaml::str("")),
        [Part::Text(literal)] => Ok(Yaml::str(literal)),
        // The whole value is one reference: Ref or Fn::GetAtt, which keep the
        // attribute's own type instead of making a string of it.
        [Part::Expr(expression)] => {
            let target = resolve(expression, logical, refs)?;
            Ok(if target.attribute.is_empty() {
                Yaml::Map(vec![("Ref".to_owned(), Yaml::str(target.name))])
            } else {
                Yaml::Map(vec![(
                    "Fn::GetAtt".to_owned(),
                    Yaml::Seq(vec![Yaml::str(target.name), Yaml::str(target.attribute)]),
                )])
            })
        }
        // References inside longer text: one Fn::Sub.
        mixed => {
            let mut template = String::new();
            for part in mixed {
                match part {
                    // `${!` is how Fn::Sub writes a literal `${`.
                    Part::Text(literal) => template.push_str(&literal.replace("${", "${!")),
                    Part::Expr(expression) => {
                        let target = resolve(expression, logical, refs)?;
                        template.push_str("${");
                        template.push_str(&target.name);
                        if !target.attribute.is_empty() {
                            template.push('.');
                            template.push_str(&target.attribute);
                        }
                        template.push('}');
                    }
                }
            }
            Ok(Yaml::Map(vec![("Fn::Sub".to_owned(), Yaml::str(template))]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_type_has_three_parts_or_is_custom() {
        for (t, want) in [
            ("AWS::EC2::Instance", true),
            ("Custom::Thing", true),
            ("Alexa::ASK::Skill", true),
            ("aws_instance", false),
            ("AWS::EC2", false),
            ("AWS::EC2::", false),
            ("AWS::EC2::Instance::X", false),
        ] {
            assert_eq!(is_resource_type(t), want, "{t}");
        }
    }

    #[test]
    fn ids_become_logical_ids() {
        assert_eq!(logical_id("web_vm").as_deref(), Some("WebVm"));
        assert_eq!(logical_id("web-vm-2").as_deref(), Some("WebVm2"));
        assert_eq!(logical_id("WebVm").as_deref(), Some("WebVm"));
        assert_eq!(logical_id("___"), None);
    }
}
