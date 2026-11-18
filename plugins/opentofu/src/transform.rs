//! Stage one: each resource becomes a block, and every reference between
//! resources becomes OpenTofu's own address.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{address, expression, refuse, refuse_at, Block, Config, Kind, OutputBlock};

/// The names OpenTofu itself gives meaning to at the start of an expression.
/// A reference that starts with one is OpenTofu's to resolve, not Meridian's,
/// and a resource may not take one as its id.
pub(crate) const NAMESPACES: &[&str] = &[
    "var",
    "local",
    "path",
    "terraform",
    "tofu",
    "each",
    "count",
    "self",
    "data",
    "ephemeral",
    "module",
    "key_provider",
    "method",
];

/// What kind of block a resource type names, and the OpenTofu type inside it.
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
                        &rules::INVALID_PARAM,
                        "a provider's name must be a string".to_owned(),
                    ))
                }
                None => r.id.clone(),
            };
            (Kind::Provider, name)
        }
        "backend" => (Kind::Backend, r.id.clone()),
        "encryption" => (Kind::Encryption, String::new()),
        "module" => (Kind::Module, String::new()),
        "variable" => (Kind::Variable, String::new()),
        "local" => (Kind::Local, String::new()),
        _ => {
            for (prefix, kind) in [("data.", Kind::Data), ("ephemeral.", Kind::Ephemeral)] {
                if let Some(inner) = t.strip_prefix(prefix) {
                    if is_resource_type(inner) {
                        return Ok((kind, inner.to_owned()));
                    }
                    return Err(unsupported(r));
                }
            }
            if is_resource_type(t) {
                return Ok((Kind::Managed, t.to_owned()));
            }
            return Err(unsupported(r));
        }
    })
}

fn unsupported(r: &Resource) -> CompileError {
    refuse(
        r,
        &rules::UNSUPPORTED_RESOURCE_TYPE,
        format!(
            "{:?} is not an OpenTofu resource type. Use a provider's own type, such as \
             aws_instance, or one of: provider, backend, encryption, module, variable, local, \
             data.<type>, ephemeral.<type>",
            r.r#type
        ),
    )
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

/// How each resource of the document is addressed from another, and what kind
/// of block it is for the message when it cannot be.
struct Addresses(BTreeMap<String, (Kind, Option<String>)>);

/// Maps the tree onto blocks. It does no ordering work.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Config, CompileError> {
    let mut kinds = Vec::with_capacity(graph.resources.len());
    let mut addresses = Addresses(BTreeMap::new());
    for r in &graph.resources {
        if NAMESPACES.contains(&r.id.as_str()) {
            return Err(refuse(
                r,
                &rules::RESERVED_NAME,
                format!(
                    "resource id {:?} is a name OpenTofu reserves for its own expressions, so \
                     ${{{}.…}} could not tell the two apart; give the resource another id",
                    r.id, r.id
                ),
            ));
        }
        let (kind, tofu_type) = classify(r)?;
        addresses
            .0
            .insert(r.id.clone(), (kind, address(kind, &tofu_type, &r.id)));
        kinds.push((kind, tofu_type));
    }

    let mut config = Config::default();
    // The variables the document marks sensitive: an output that reads one
    // has to say so itself.
    let mut sensitive = BTreeSet::new();
    for (r, (kind, tofu_type)) in graph.resources.iter().zip(kinds) {
        check_meta(r, kind)?;
        let mut refs = BTreeSet::new();
        let mut body = BTreeMap::new();
        for (name, value) in &r.params {
            let rewritten = rewrite(value, &addresses, &mut refs).map_err(|msg| {
                refuse(
                    r,
                    &rules::INVALID_REFERENCE,
                    format!("param {name:?}: {msg}"),
                )
            })?;
            body.insert(name.clone(), rewritten);
        }
        refs.remove(&r.id);

        match kind {
            Kind::Provider => {
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
                        .insert(tofu_type.clone(), Value::Map(required));
                }
            }
            Kind::Variable if body.get("sensitive") == Some(&Value::Bool(true)) => {
                sensitive.insert(r.id.clone());
            }
            _ => {}
        }

        if !r.adopt.is_empty() {
            if kind != Kind::Managed {
                return Err(refuse(
                    r,
                    &rules::INVALID_ADOPT,
                    format!(
                        "adopt {:?}: only a managed resource can be adopted, and {:?} is \
                         not one: OpenTofu imports what it manages, not providers, data \
                         sources, modules, variables or locals",
                        r.adopt, r.r#type
                    ),
                ));
            }
            config
                .imports
                .push((format!("{tofu_type}.{}", r.id), r.adopt.clone()));
        }

        config.blocks.push(Block {
            kind,
            r#type: tofu_type,
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
                    &rules::INVALID_REFERENCE,
                    format!("output {:?}: {msg}", output.name),
                )
            },
        )?;
        config.outputs.push(OutputBlock {
            name: output.name.clone(),
            value,
            sensitive: refs.iter().any(|id| sensitive.contains(id)),
            pos: output.pos.clone(),
        });
    }
    Ok(config)
}

/// The parts of a resource that mean something to other tools and nothing to
/// OpenTofu.
fn check_meta(r: &Resource, kind: Kind) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: \
                 OpenTofu computes its whole plan before it changes anything, so there is \
                 no apply-time conditional to compile it into",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => {}
        "absent" if kind == Kind::Managed || kind == Kind::Module => {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
                "state \"absent\" has no meaning on this target: OpenTofu destroys whatever \
                 its configuration no longer declares, so remove the resource from the document"
                    .to_owned(),
            ))
        }
        other => {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
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

/// Rewrites every `${id.path}` in a value to OpenTofu's address for `id`,
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
            // In the JSON syntax a string is a template, so literal text that
            // looks like one is escaped to stay literal.
            Part::Text(literal) => out.push_str(&literal.replace("${", "$${").replace("%{", "%%{")),
            Part::Expr(expression) => {
                let (name, rest) = expr::head(&expression);
                match addresses.0.get(name) {
                    Some((_, Some(address))) => {
                        refs.insert(name.to_owned());
                        out.push_str(&format!("${{{address}{rest}}}"));
                    }
                    Some((kind, None)) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is {} and has no \
                             attributes to read",
                            kind.described()
                        ))
                    }
                    None if expr::is_path(&expression) && !NAMESPACES.contains(&name) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is not a resource in this document"
                        ))
                    }
                    // Anything else is OpenTofu's own expression language. It
                    // is handed over as written, but for the document's own
                    // names inside it, which become addresses like any other.
                    None => {
                        let rewritten = expression::rewrite_names(&expression, |name| {
                            let (_, address) = addresses.0.get(name)?;
                            let address = address.clone()?;
                            refs.insert(name.to_owned());
                            Some(address)
                        });
                        out.push_str(&format!("${{{rewritten}}}"));
                    }
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

    #[test]
    fn every_kind_is_addressed_the_way_opentofu_addresses_it() {
        for (kind, want) in [
            (Kind::Managed, Some("aws_vpc.net")),
            (Kind::Data, Some("data.aws_vpc.net")),
            (Kind::Ephemeral, Some("ephemeral.aws_vpc.net")),
            (Kind::Module, Some("module.net")),
            (Kind::Variable, Some("var.net")),
            (Kind::Local, Some("local.net")),
            (Kind::Provider, None),
            (Kind::Backend, None),
            (Kind::Encryption, None),
        ] {
            assert_eq!(address(kind, "aws_vpc", "net").as_deref(), want, "{kind:?}");
        }
    }
}
