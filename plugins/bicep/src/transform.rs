//! Stage one: each resource becomes a `resource` statement, and every
//! reference between resources becomes a Bicep expression.

use std::collections::BTreeSet;

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, refuse_at, secret_parameter, Deployment, Expr, Piece, Statement, Template};

/// `Namespace/type@api-version`, with nested types allowed.
fn is_resource_type(t: &str) -> bool {
    let Some((name, version)) = t.split_once('@') else {
        return false;
    };
    let mut segments = name.split('/');
    let namespace = segments.next().unwrap_or("");
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    };
    let types: Vec<&str> = segments.collect();
    namespace.contains('.')
        && ok(namespace)
        && !types.is_empty()
        && types.iter().all(|s| ok(s))
        && ok(version)
        && version.starts_with(|c: char| c.is_ascii_digit())
}

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Template, CompileError> {
    let mut template = Template {
        document: graph.name.clone(),
        deployment: Deployment {
            name: graph.name.clone(),
            ..Deployment::default()
        },
        ..Template::default()
    };

    let mut symbols = BTreeSet::new();
    let mut deployment_seen = false;
    for r in &graph.resources {
        check_meta(r)?;
        if r.r#type == "deployment" {
            if deployment_seen {
                return Err(refuse(
                    r,
                    &rules::DUPLICATE_RESOURCE,
                    "a document is one deployment; this one has two resources of type \"deployment\""
                        .to_owned(),
                ));
            }
            deployment_seen = true;
            read_deployment(r, &mut template.deployment)?;
        } else if is_resource_type(&r.r#type) {
            symbols.insert(r.id.clone());
        } else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_RESOURCE_TYPE,
                format!(
                    "{:?} is not an Azure resource type. Use Azure's own type with its API \
                     version, such as Microsoft.Storage/storageAccounts@2023-01-01, or \
                     \"deployment\" for where the file is deployed",
                    r.r#type
                ),
            ));
        }
    }

    // Each secret is a parameter: a name a param may refer to, but not a
    // resource to order after.
    let parameters: BTreeSet<String> = graph
        .secrets
        .iter()
        .map(|secret| secret_parameter(&secret.name))
        .collect();
    symbols.extend(parameters.iter().cloned());
    template.parameters = parameters.iter().cloned().collect();

    for r in graph.resources.iter().filter(|r| r.r#type != "deployment") {
        let mut refs = BTreeSet::new();
        let mut body = Vec::with_capacity(r.params.len());
        for (name, value) in &r.params {
            let rewritten = rewrite(value, &symbols, &mut refs).map_err(|msg| {
                refuse(
                    r,
                    &rules::INVALID_REFERENCE,
                    format!("param {name:?}: {msg}"),
                )
            })?;
            body.push((name.clone(), rewritten));
        }
        refs.remove(&r.id);
        refs.retain(|r| !parameters.contains(r));
        // Bicep convention: name first, then location, then the rest in
        // sorted order, which is how the params arrive.
        body.sort_by_key(|(name, _)| match name.as_str() {
            "name" => 0,
            "location" => 1,
            _ => 2,
        });
        let azure_name = match r.params.get("name") {
            Some(Value::String(name)) if !expr::has_expr(name) => Some(name.clone()),
            _ => None,
        };
        template.statements.push(Statement {
            symbol: r.id.clone(),
            r#type: r.r#type.clone(),
            body,
            azure_name,
            depends_on: Vec::new(),
            refs,
            pos: r.pos.clone(),
        });
    }

    for output in &graph.outputs {
        let mut refs = BTreeSet::new();
        let value =
            rewrite(&Value::String(output.value.clone()), &symbols, &mut refs).map_err(|msg| {
                refuse_at(
                    "",
                    &output.pos,
                    &rules::INVALID_REFERENCE,
                    format!("output {:?}: {msg}", output.name),
                )
            })?;
        // An output is declared as a string. A value that is one expression
        // may be of any type, so it is converted; text already is one.
        let value = match value {
            Expr::Raw(expression) => Expr::Raw(format!("string({expression})")),
            other => other,
        };
        template
            .outputs
            .push((output.name.clone(), value, output.pos.clone()));
    }
    Ok(template)
}

fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a Bicep \
                 condition is decided when the deployment starts, not from the state of what \
                 it deploys to",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" has no meaning on this target: a deployment creates and updates \
             resources and never deletes one, so delete it in Azure and remove it from the document"
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

fn read_deployment(r: &Resource, deployment: &mut Deployment) -> Result<(), CompileError> {
    for (name, value) in &r.params {
        let slot = match name.as_str() {
            "name" => &mut deployment.name,
            "resourceGroup" => &mut deployment.resource_group,
            "subscription" => &mut deployment.subscription,
            _ => {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "resource type \"deployment\" does not accept param {name:?} on this \
                         target (accepted: name, resourceGroup, subscription)"
                    ),
                ))
            }
        };
        let Value::String(text) = value else {
            return Err(refuse(
                r,
                &rules::INVALID_PARAM,
                format!("param {name:?} of the deployment must be a string"),
            ));
        };
        *slot = text.clone();
    }
    Ok(())
}

fn rewrite(
    value: &Value,
    symbols: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
) -> Result<Expr, String> {
    Ok(match value {
        Value::Null => Expr::Raw("null".to_owned()),
        Value::Bool(b) => Expr::Raw(b.to_string()),
        Value::Int(i) => Expr::Raw(i.to_string()),
        Value::Uint(u) => Expr::Raw(u.to_string()),
        // Bicep has no literal for a number with a fraction; json() reads one.
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
            Expr::Raw(format!("{}", *f as i64))
        }
        Value::Float(f) => Expr::Raw(format!("json('{f}')")),
        Value::String(s) => rewrite_string(s, symbols, refs)?,
        // A plugin is never sent a secret as a value; one would be written as its mask.
        Value::Secret(_) => rewrite_string(meridian_sdk::SENSITIVE, symbols, refs)?,
        Value::List(items) => Expr::Array(
            items
                .iter()
                .map(|item| rewrite(item, symbols, refs))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Expr::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, symbols, refs)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

/// Checks one `${...}` and notes what it refers to. The expression is Bicep
/// as written: a document's ids are Bicep's symbolic names.
fn check(
    expression: &str,
    symbols: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
) -> Result<(), String> {
    let (name, _) = expr::head(expression);
    if symbols.contains(name) {
        refs.insert(name.to_owned());
    } else if expr::is_path(expression) {
        return Err(format!(
            "${{{expression}}} refers to {name:?}, which is not a resource in this document"
        ));
    } else {
        // A function call or other Bicep expression may still mention a
        // resource; note any it does, so ordering is not written twice.
        for symbol in symbols {
            if mentions(expression, symbol) {
                refs.insert(symbol.clone());
            }
        }
    }
    Ok(())
}

/// Whether an expression uses a symbol as a whole word.
fn mentions(expression: &str, symbol: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    expression.match_indices(symbol).any(|(at, _)| {
        let before = expression[..at].chars().next_back();
        let after = expression[at + symbol.len()..].chars().next();
        !before.is_some_and(|c| word(c) || c == '.' || c == '\'') && !after.is_some_and(word)
    })
}

fn rewrite_string(
    text: &str,
    symbols: &BTreeSet<String>,
    refs: &mut BTreeSet<String>,
) -> Result<Expr, String> {
    let parts = expr::parse(text)?;
    if let [Part::Expr(expression)] = parts.as_slice() {
        // The whole value is one expression: written bare, so it keeps its
        // own type instead of becoming a string.
        check(expression, symbols, refs)?;
        return Ok(Expr::Raw(expression.clone()));
    }
    let mut pieces = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            Part::Text(literal) => pieces.push(Piece::Literal(literal)),
            Part::Expr(expression) => {
                check(&expression, symbols, refs)?;
                pieces.push(Piece::Interpolated(expression));
            }
        }
    }
    Ok(Expr::Text(pieces))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_type_carries_its_api_version() {
        for (t, want) in [
            ("Microsoft.Storage/storageAccounts@2023-01-01", true),
            ("Microsoft.Network/virtualNetworks/subnets@2023-09-01", true),
            ("Microsoft.Web/sites@2022-09-01-preview", true),
            ("Microsoft.Storage/storageAccounts", false),
            ("storageAccounts@2023-01-01", false),
            ("Microsoft.Storage@2023-01-01", false),
            ("Microsoft.Storage/storageAccounts@latest", false),
            ("aws_instance", false),
        ] {
            assert_eq!(is_resource_type(t), want, "{t}");
        }
    }

    #[test]
    fn a_symbol_is_found_only_as_a_whole_word() {
        assert!(mentions("reference(vnet.id).x", "vnet"));
        assert!(mentions("concat(a, vnet)", "vnet"));
        assert!(!mentions("resourceGroup().vnets", "vnet"));
        assert!(!mentions("x.vnet", "vnet"));
        assert!(!mentions("'vnet'", "vnet"));
    }
}
