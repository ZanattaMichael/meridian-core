//! Stage one: each resource becomes a block, and every reference between
//! resources becomes Terraform's own address.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, refuse_at, Block, Config, Kind};

/// The names Terraform itself gives meaning to at the start of an expression.
/// A reference that starts with one is Terraform's to resolve, not Meridian's.
const TERRAFORM_NAMESPACES: &[&str] = &[
    "var",
    "local",
    "path",
    "terraform",
    "each",
    "count",
    "self",
    "data",
    "module",
];

/// What kind of block a resource type names, and the Terraform type inside it.
fn classify(r: &Resource) -> Result<(Kind, String), CompileError> {
    let t = r.r#type.as_str();
    Ok(match t {
        "provider" => {
            // A provider is named by its `name` param when one id is not
            // enough, as with two aliased configurations of one provider.
            let name = match r.params.get("name") {
                Some(Value::String(name)) => name.clone(),
                Some(_) => {
                    return Err(refuse(
                        r,
                        "invalid-param",
                        "a provider's name must be a string".to_owned(),
                    ))
                }
                None => r.id.clone(),
            };
            (Kind::Provider, name)
        }
        "backend" => (Kind::Backend, r.id.clone()),
        "module" => (Kind::Module, String::new()),
        "variable" => (Kind::Variable, String::new()),
        "local" => (Kind::Local, String::new()),
        _ => {
            if let Some(data_type) = t.strip_prefix("data.") {
                if is_resource_type(data_type) {
                    return Ok((Kind::Data, data_type.to_owned()));
                }
            } else if is_resource_type(t) {
                return Ok((Kind::Managed, t.to_owned()));
            }
            return Err(refuse(
                r,
                "unsupported-resource-type",
                format!(
                    "{t:?} is not a Terraform resource type. Use a provider's own type, such as \
                     aws_instance, or one of: provider, backend, module, variable, local, \
                     data.<type>"
                ),
            ));
        }
    })
}

/// `<provider>_<type>`: lower-case words joined by underscores, at least two.
fn is_resource_type(t: &str) -> bool {
    let mut words = t.split('_');
    let first = words.next().unwrap_or("");
    let mut rest = 0;
    let word = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    if !word(first) || !first.starts_with(|c: char| c.is_ascii_lowercase()) {
        return false;
    }
    for w in words {
        if !word(w) {
            return false;
        }
        rest += 1;
    }
    rest > 0
}

/// How each resource of the document is addressed from another.
struct Addresses(BTreeMap<String, Option<String>>);

/// Maps the tree onto blocks. It does no ordering work.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Config, CompileError> {
    let mut kinds = Vec::with_capacity(graph.resources.len());
    let mut addresses = Addresses(BTreeMap::new());
    for r in &graph.resources {
        let (kind, tf_type) = classify(r)?;
        let address = match kind {
            Kind::Managed => Some(format!("{tf_type}.{}", r.id)),
            Kind::Data => Some(format!("data.{tf_type}.{}", r.id)),
            Kind::Module => Some(format!("module.{}", r.id)),
            Kind::Variable => Some(format!("var.{}", r.id)),
            Kind::Local => Some(format!("local.{}", r.id)),
            Kind::Provider | Kind::Backend => None,
        };
        addresses.0.insert(r.id.clone(), address);
        kinds.push((kind, tf_type));
    }

    let mut config = Config::default();
    for (r, (kind, tf_type)) in graph.resources.iter().zip(kinds) {
        check_meta(r)?;
        let mut refs = BTreeSet::new();
        let mut body = BTreeMap::new();
        for (name, value) in &r.params {
            let rewritten = rewrite(value, &addresses, &mut refs)
                .map_err(|msg| refuse(r, "invalid-reference", format!("param {name:?}: {msg}")))?;
            body.insert(name.clone(), rewritten);
        }
        refs.remove(&r.id);

        if kind == Kind::Provider {
            body.remove("name");
            let mut required = BTreeMap::new();
            for key in ["source", "version"] {
                if let Some(value) = body.remove(key) {
                    required.insert(key.to_owned(), value);
                }
            }
            if !required.is_empty() {
                config
                    .required_providers
                    .insert(tf_type.clone(), Value::Map(required));
            }
        }

        config.blocks.push(Block {
            kind,
            r#type: tf_type,
            name: r.id.clone(),
            body,
            refs,
            pos: r.pos.clone(),
        });
    }

    for output in &graph.outputs {
        let mut refs = BTreeSet::new();
        let value = rewrite(&Value::String(output.value.clone()), &addresses, &mut refs).map_err(
            |msg| {
                refuse_at(
                    "",
                    &output.pos,
                    "invalid-reference",
                    format!("output {:?}: {msg}", output.name),
                )
            },
        )?;
        config
            .outputs
            .push((output.name.clone(), value, output.pos.clone()));
    }
    Ok(config)
}

/// The parts of a resource that mean something to other tools and nothing to
/// Terraform.
fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            "unsupported-runtime-condition",
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: \
                 Terraform computes its whole plan before it changes anything, so there is \
                 no apply-time conditional to compile it into",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => {}
        "absent" => {
            return Err(refuse(
                r,
                "unsupported-state",
                "state \"absent\" has no meaning on this target: Terraform destroys whatever \
                 its configuration no longer declares, so remove the resource from the document"
                    .to_owned(),
            ))
        }
        other => {
            return Err(refuse(
                r,
                "unsupported-state",
                format!(
                    "resource type {:?} does not accept state {other:?} on this target \
                     (accepted: present)",
                    r.r#type
                ),
            ))
        }
    }
    Ok(())
}

/// Rewrites every `${id.path}` in a value to Terraform's address for `id`,
/// recording which resources were referred to.
fn rewrite(
    value: &Value,
    addresses: &Addresses,
    refs: &mut BTreeSet<String>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => Value::String(rewrite_string(s, addresses, refs)?),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| rewrite(item, addresses, refs))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, addresses, refs)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn rewrite_string(
    text: &str,
    addresses: &Addresses,
    refs: &mut BTreeSet<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for part in expr::parse(text)? {
        match part {
            // In Terraform's JSON syntax a string is a template, so literal
            // text that looks like one is escaped to stay literal.
            Part::Text(literal) => out.push_str(&literal.replace("${", "$${").replace("%{", "%%{")),
            Part::Expr(expression) => {
                let (name, rest) = expr::head(&expression);
                match addresses.0.get(name) {
                    Some(Some(address)) => {
                        refs.insert(name.to_owned());
                        out.push_str(&format!("${{{address}{rest}}}"));
                    }
                    Some(None) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is a provider or backend \
                             and has no attributes to read"
                        ))
                    }
                    None if expr::is_path(&expression) && !TERRAFORM_NAMESPACES.contains(&name) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is not a resource in this document"
                        ))
                    }
                    // Anything else is Terraform's own expression language
                    // and is handed over as written.
                    None => out.push_str(&format!("${{{expression}}}")),
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_type_is_a_provider_prefix_and_a_name() {
        for (t, want) in [
            ("aws_instance", true),
            ("azurerm_resource_group", true),
            ("terraform_data", true),
            ("null_resource", true),
            ("compute_instance", true),
            ("instance", false),
            ("AWS_instance", false),
            ("aws__instance", false),
            ("aws_", false),
            ("_x", false),
            ("aws-instance", false),
            ("9aws_x", false),
        ] {
            assert_eq!(is_resource_type(t), want, "{t}");
        }
    }
}
