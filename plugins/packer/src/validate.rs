//! Stage three: the constraints that are Packer's and not Meridian's.

use std::collections::BTreeSet;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, Value};

use crate::{refuse_at, Block, Kind, Template};

/// A name Packer accepts for a block: a letter or underscore, then letters,
/// digits, underscores and hyphens.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn require_string(block: &Block, what: &str, param: &str) -> Result<(), CompileError> {
    if matches!(block.body.get(param), Some(Value::String(s)) if !s.is_empty()) {
        return Ok(());
    }
    Err(refuse_at(
        &block.name,
        &block.pos,
        &rules::MISSING_PARAM,
        format!("resource type {what:?} requires the string param {param:?} on this target"),
    ))
}

pub(crate) fn validate(template: &Template) -> Result<(), CompileError> {
    let mut cleanup: Option<&Block> = None;
    for block in &template.blocks {
        if !is_identifier(&block.name) {
            return Err(refuse_at(
                &block.name,
                &block.pos,
                &rules::INVALID_NAME,
                format!(
                    "resource id {:?} cannot be a Packer name: a name starts with a letter or \
                     underscore and holds only letters, digits, underscores and hyphens",
                    block.name
                ),
            ));
        }
        match block.kind {
            Kind::Plugin => {
                require_string(block, "plugin", "source")?;
                require_string(block, "plugin", "version")?;
                if let Some(extra) = block
                    .body
                    .keys()
                    .find(|k| !matches!(k.as_str(), "source" | "version"))
                {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::INVALID_PARAM,
                        format!(
                            "a plugin requirement takes the params \"source\" and \"version\" \
                             only, not {extra:?}"
                        ),
                    ));
                }
            }
            Kind::Local => {
                if !block.body.contains_key("value") {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::MISSING_PARAM,
                        "resource type \"local\" requires param \"value\" on this target"
                            .to_owned(),
                    ));
                }
                if let Some(extra) = block.body.keys().find(|k| k.as_str() != "value") {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::INVALID_PARAM,
                        format!("a local takes the param \"value\" only, not {extra:?}"),
                    ));
                }
            }
            Kind::ErrorCleanup => {
                if let Some(first) = cleanup {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::DUPLICATE_RESOURCE,
                        format!(
                            "a build has one error-cleanup provisioner, and {:?} is already it",
                            first.name
                        ),
                    ));
                }
                cleanup = Some(block);
            }
            Kind::Variable
            | Kind::Data
            | Kind::Source
            | Kind::Provisioner
            | Kind::PostProcessor => {}
        }
    }

    if template.of_kind(Kind::Source).next().is_none() {
        return Err(refuse_at(
            "",
            &template
                .blocks
                .first()
                .map(|b| b.pos.clone())
                .unwrap_or_default(),
            &rules::MISSING_PARAM,
            "a build needs at least one source to build from: add a resource of type \
             source.<builder>, such as source.amazon-ebs or source.docker"
                .to_owned(),
        ));
    }

    let mut outputs = BTreeSet::new();
    for output in &template.outputs {
        if output.name.is_empty() {
            return Err(refuse_at(
                "",
                &output.pos,
                &rules::INVALID_NAME,
                "an output needs a name".to_owned(),
            ));
        }
        if !outputs.insert(&output.name) {
            return Err(refuse_at(
                "",
                &output.pos,
                &rules::DUPLICATE_OUTPUT,
                format!("output {:?} is declared twice", output.name),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packer_name_starts_with_a_letter_or_underscore() {
        for (name, want) in [
            ("ubuntu", true),
            ("_x", true),
            ("ubuntu-22_04", true),
            ("22-04", false),
            ("", false),
            ("a.b", false),
            ("a b", false),
        ] {
            assert_eq!(is_identifier(name), want, "{name}");
        }
    }
}

#[cfg(test)]
mod rule_tables {
    use super::*;
    use crate::{OutputTemplate, NAME};
    use meridian_sdk::Position;

    fn block(kind: Kind, r#type: &str, name: &str, body: &[(&str, Value)]) -> Block {
        Block {
            kind,
            r#type: r#type.to_owned(),
            name: name.to_owned(),
            body: body
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            refs: Default::default(),
            pos: Position {
                line: 5,
                ..Position::default()
            },
        }
    }

    fn text(s: &str) -> Value {
        Value::String(s.to_owned())
    }

    fn output(name: &str) -> OutputTemplate {
        OutputTemplate {
            name: name.to_owned(),
            template: "x".to_owned(),
            pos: Position::default(),
        }
    }

    fn template() -> Template {
        Template {
            name: "image".to_owned(),
            blocks: vec![
                block(
                    Kind::Plugin,
                    "",
                    "docker",
                    &[
                        ("source", text("github.com/hashicorp/docker")),
                        ("version", text(">= 1.0.0")),
                    ],
                ),
                block(Kind::Variable, "", "tag", &[]),
                block(Kind::Data, "http", "release", &[]),
                block(Kind::Local, "", "stamp", &[("value", text("x"))]),
                block(Kind::Source, "docker", "ubuntu", &[]),
                block(Kind::Provisioner, "shell", "setup", &[]),
                block(Kind::ErrorCleanup, "shell", "tidy", &[]),
                block(Kind::PostProcessor, "manifest", "record", &[]),
            ],
            chains: Vec::new(),
            outputs: vec![output("image_id")],
        }
    }

    fn at<'a>(t: &'a mut Template, name: &str) -> &'a mut Block {
        t.blocks.iter_mut().find(|b| b.name == name).expect(name)
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Template);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "22-04", |t| {
                at(t, "ubuntu").name = "22-04".to_owned();
            }),
            ("missing-param", "docker", |t| {
                at(t, "docker").body.remove("source");
            }),
            ("missing-param", "docker", |t| {
                at(t, "docker").body.remove("version");
            }),
            ("missing-param", "docker", |t| {
                at(t, "docker").body.insert("version".to_owned(), text(""));
            }),
            ("missing-param", "docker", |t| {
                at(t, "docker")
                    .body
                    .insert("source".to_owned(), Value::Bool(true));
            }),
            ("invalid-param", "docker", |t| {
                at(t, "docker").body.insert("pin".to_owned(), text("x"));
            }),
            ("missing-param", "stamp", |t| at(t, "stamp").body.clear()),
            ("invalid-param", "stamp", |t| {
                at(t, "stamp").body.insert("type".to_owned(), text("x"));
            }),
            ("duplicate-resource", "again", |t| {
                t.blocks
                    .push(block(Kind::ErrorCleanup, "shell", "again", &[]));
            }),
            ("missing-param", "", |t| {
                t.blocks.retain(|b| b.kind != Kind::Source);
            }),
            ("missing-param", "", |t| t.blocks.clear()),
            ("invalid-name", "", |t| t.outputs[0].name.clear()),
            ("duplicate-output", "", |t| {
                t.outputs.push(output("image_id"))
            }),
        ];

        assert_eq!(validate(&template()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = template();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
        }
    }

    #[test]
    fn every_kind_of_block_needs_a_valid_name() {
        for index in 0..template().blocks.len() {
            let mut t = template();
            t.blocks[index].name = "not valid".to_owned();
            let err = validate(&t).expect_err("a name with a space");
            assert_eq!(err.rule, "invalid-name", "block {index}");
            assert_eq!(err.pos.line, 5, "block {index}");
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Template);
        let table: &[(&str, Change)] = &[
            ("two sources", |t| {
                t.blocks.push(block(Kind::Source, "docker", "debian", &[]));
            }),
            ("two plugins, provisioners and post-processors", |t| {
                t.blocks.push(block(
                    Kind::Plugin,
                    "",
                    "amazon",
                    &[
                        ("source", text("github.com/hashicorp/amazon")),
                        ("version", text("~> 1")),
                    ],
                ));
                t.blocks
                    .push(block(Kind::Provisioner, "file", "upload", &[]));
                t.blocks
                    .push(block(Kind::PostProcessor, "checksum", "sum", &[]));
            }),
            ("no error-cleanup provisioner", |t| {
                t.blocks.retain(|b| b.kind != Kind::ErrorCleanup);
            }),
            ("a source alone", |t| {
                t.blocks.retain(|b| b.kind == Kind::Source)
            }),
            ("no outputs", |t| t.outputs.clear()),
        ];
        for (what, change) in table {
            let mut t = template();
            change(&mut t);
            assert_eq!(validate(&t), Ok(()), "{what}");
        }
    }
}
