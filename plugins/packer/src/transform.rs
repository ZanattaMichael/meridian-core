//! Stage one: each resource becomes a block, and every reference between
//! resources becomes Packer's own address.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::outputs;
use crate::{refuse, refuse_at, Block, Kind, OutputTemplate, Template};

/// The names Packer itself gives meaning to at the start of an expression. A
/// reference that starts with one is Packer's to resolve, not Meridian's.
const PACKER_NAMESPACES: &[&str] = &["var", "local", "data", "source", "build", "path", "packer"];

/// The arguments of a provisioner or a post-processor that list sources.
const SOURCE_FILTERS: &[&str] = &["only", "except"];

/// The resource type prefixes that take one of Packer's types after a dot.
const PREFIXES: &[(&str, Kind)] = &[
    ("source", Kind::Source),
    ("provisioner", Kind::Provisioner),
    ("post-processor", Kind::PostProcessor),
    ("error-cleanup-provisioner", Kind::ErrorCleanup),
    ("data", Kind::Data),
];

/// What part of a template a resource type names, and Packer's type inside it.
fn classify(r: &Resource) -> Result<(Kind, String), CompileError> {
    let t = r.r#type.as_str();
    match t {
        "plugin" => return Ok((Kind::Plugin, String::new())),
        "variable" => return Ok((Kind::Variable, String::new())),
        "local" => return Ok((Kind::Local, String::new())),
        _ => {}
    }
    if let Some((prefix, packer_type)) = t.split_once('.') {
        if let Some((_, kind)) = PREFIXES.iter().find(|(p, _)| *p == prefix) {
            if is_packer_type(packer_type) {
                return Ok((*kind, packer_type.to_owned()));
            }
        }
    }
    Err(refuse(
        r,
        &rules::UNSUPPORTED_RESOURCE_TYPE,
        format!(
            "{t:?} is not part of a Packer build. Use source.<builder>, such as \
             source.amazon-ebs, or one of: provisioner.<type>, post-processor.<type>, \
             error-cleanup-provisioner.<type>, data.<type>, variable, local, plugin"
        ),
    ))
}

/// A builder, provisioner, post-processor or data source type: lower-case
/// words joined by hyphens, as in `amazon-ebs` and `shell-local`.
fn is_packer_type(t: &str) -> bool {
    t.split('-').all(|word| {
        !word.is_empty()
            && word
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    }) && t.starts_with(|c: char| c.is_ascii_lowercase())
}

/// How one resource of the document is read from another.
enum Address {
    /// Packer's own address, to which a path may be appended.
    Readable(String),
    /// A part of the build that has nothing to read.
    Opaque(Kind),
}

struct Addresses(BTreeMap<String, Address>);

/// Maps the tree onto blocks. It does no ordering work.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Template, CompileError> {
    let mut kinds = Vec::with_capacity(graph.resources.len());
    let mut addresses = Addresses(BTreeMap::new());
    let mut sources = BTreeMap::new();
    for r in &graph.resources {
        let (kind, packer_type) = classify(r)?;
        let address = match kind {
            Kind::Variable => Address::Readable(format!("var.{}", r.id)),
            Kind::Local => Address::Readable(format!("local.{}", r.id)),
            Kind::Data => Address::Readable(format!("data.{packer_type}.{}", r.id)),
            other => Address::Opaque(other),
        };
        if kind == Kind::Source {
            sources.insert(r.id.clone(), packer_type.clone());
        }
        addresses.0.insert(r.id.clone(), address);
        kinds.push((kind, packer_type));
    }

    let mut template = Template {
        name: graph.name.clone(),
        ..Template::default()
    };
    for (r, (kind, packer_type)) in graph.resources.iter().zip(kinds) {
        check_meta(r)?;
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

        if matches!(kind, Kind::Provisioner | Kind::PostProcessor) {
            for key in SOURCE_FILTERS {
                if let Some(Value::List(items)) = body.get_mut(*key) {
                    name_sources(items, &sources);
                }
            }
        }

        template.blocks.push(Block {
            kind,
            r#type: packer_type,
            name: r.id.clone(),
            body,
            refs,
            pos: r.pos.clone(),
        });
    }

    for output in &graph.outputs {
        let checked = outputs::check(&output.value, &sources).map_err(|msg| {
            refuse_at(
                "",
                &output.pos,
                &rules::INVALID_REFERENCE,
                format!("output {:?}: {msg}", output.name),
            )
        })?;
        template.outputs.push(OutputTemplate {
            name: output.name.clone(),
            template: checked,
            pos: output.pos.clone(),
        });
    }
    Ok(template)
}

/// In `only` and `except`, a source may be named by its id alone. Packer wants
/// `<builder>.<name>`, so an entry that is the id of a source is completed.
fn name_sources(items: &mut [Value], sources: &BTreeMap<String, String>) {
    for item in items {
        if let Value::String(name) = item {
            if let Some(builder) = sources.get(name.as_str()) {
                *name = format!("{builder}.{name}");
            }
        }
    }
}

/// The parts of a resource that mean something to other tools and nothing to
/// Packer.
fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a Packer \
                 build runs every step it is given. To run a step for some sources only, use \
                 the step's own \"only\" or \"except\" param",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" has no meaning on this target: a build only creates images and \
             never removes one, so remove the resource from the document"
                .to_owned(),
        )),
        other => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            format!(
                "resource type {:?} does not accept state {other:?} on this target \
                 (accepted: present)",
                r.r#type
            ),
        )),
    }
}

/// Rewrites every `${id.path}` in a value to Packer's address for `id`,
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
            // In Packer's JSON syntax a string is a template, so literal text
            // that looks like one is escaped to stay literal.
            Part::Text(literal) => out.push_str(&literal.replace("${", "$${").replace("%{", "%%{")),
            Part::Expr(expression) => {
                let (name, rest) = expr::head(&expression);
                match addresses.0.get(name) {
                    Some(Address::Readable(address)) => {
                        refs.insert(name.to_owned());
                        out.push_str(&format!("${{{address}{rest}}}"));
                    }
                    Some(Address::Opaque(Kind::Source)) => {
                        return Err(format!(
                            "${{{expression}}} refers to the source {name:?}, which has nothing \
                             to read while it is being built. Inside a provisioner, Packer's own \
                             ${{source.name}} and ${{build.ID}} describe the source being built; \
                             what a source produced can be read in the document's outputs"
                        ))
                    }
                    Some(Address::Opaque(kind)) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is a {} and has no \
                             attributes to read",
                            kind.noun()
                        ))
                    }
                    None if expr::is_path(&expression) && !PACKER_NAMESPACES.contains(&name) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is not a resource in \
                             this document"
                        ))
                    }
                    // Anything else is Packer's own expression language and
                    // is handed over as written.
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
    fn a_packer_type_is_lower_case_words_joined_by_hyphens() {
        for (t, want) in [
            ("amazon-ebs", true),
            ("shell", true),
            ("shell-local", true),
            ("googlecompute", true),
            ("azure-arm", true),
            ("hyperv-vmcx", true),
            ("s3", true),
            ("", false),
            ("Amazon-ebs", false),
            ("amazon--ebs", false),
            ("amazon-", false),
            ("-ebs", false),
            ("amazon_ebs", false),
            ("amazon.ebs", false),
            ("9p", false),
        ] {
            assert_eq!(is_packer_type(t), want, "{t}");
        }
    }

    #[test]
    fn a_source_id_in_a_filter_is_completed_and_anything_else_is_kept() {
        let sources = BTreeMap::from([("ubuntu".to_owned(), "amazon-ebs".to_owned())]);
        let mut items = vec![
            Value::String("ubuntu".into()),
            Value::String("docker.app".into()),
            Value::Int(3),
        ];
        name_sources(&mut items, &sources);
        assert_eq!(
            items,
            [
                Value::String("amazon-ebs.ubuntu".into()),
                Value::String("docker.app".into()),
                Value::Int(3),
            ]
        );
    }
}
