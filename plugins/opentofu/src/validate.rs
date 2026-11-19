//! Stage three: the constraints that are OpenTofu's and not Meridian's.

use std::collections::BTreeSet;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, Value};

use crate::{refuse_at, Config, Kind};

/// A name OpenTofu accepts for a block or an output: a letter or underscore,
/// then letters, digits, underscores and hyphens.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn validate(config: &Config) -> Result<(), CompileError> {
    let mut providers = BTreeSet::new();
    let mut backends = 0;
    let mut encryptions = 0;
    for block in &config.blocks {
        if !is_identifier(&block.name) {
            return Err(refuse_at(
                &block.name,
                &block.pos,
                &rules::INVALID_NAME,
                format!(
                    "resource id {:?} cannot be an OpenTofu name: a name starts with a letter \
                     or underscore and holds only letters, digits, underscores and hyphens",
                    block.name
                ),
            ));
        }
        match block.kind {
            Kind::Provider => {
                let alias = match block.body.get("alias") {
                    None => String::new(),
                    Some(Value::String(alias)) => alias.clone(),
                    Some(_) => {
                        return Err(refuse_at(
                            &block.name,
                            &block.pos,
                            &rules::INVALID_PARAM,
                            "a provider's alias must be a string".to_owned(),
                        ))
                    }
                };
                if !providers.insert((block.r#type.clone(), alias.clone())) {
                    let msg = if alias.is_empty() {
                        format!(
                            "provider {:?} is configured twice; give all but one an alias",
                            block.r#type
                        )
                    } else {
                        format!(
                            "provider {:?} has two configurations with the alias {alias:?}",
                            block.r#type
                        )
                    };
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::DUPLICATE_RESOURCE,
                        msg,
                    ));
                }
            }
            Kind::Backend => {
                backends += 1;
                if backends > 1 {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::DUPLICATE_RESOURCE,
                        "a configuration stores its state in one backend; this document declares two"
                            .to_owned(),
                    ));
                }
            }
            Kind::Module => {
                if !matches!(block.body.get("source"), Some(Value::String(_))) {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::MISSING_PARAM,
                        "resource type \"module\" requires param \"source\" on this target"
                            .to_owned(),
                    ));
                }
            }
            Kind::Encryption => {
                encryptions += 1;
                if encryptions > 1 {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::DUPLICATE_RESOURCE,
                        "a configuration has one encryption block; this document declares two"
                            .to_owned(),
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
                if let Some(extra) = block.body.keys().find(|k| *k != "value") {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        &rules::INVALID_PARAM,
                        format!(
                            "a local value has one param, \"value\"; {extra:?} has nowhere to go"
                        ),
                    ));
                }
            }
            Kind::Variable | Kind::Data | Kind::Ephemeral | Kind::Managed => {}
        }
    }

    let mut outputs = BTreeSet::new();
    for output in &config.outputs {
        if !is_identifier(&output.name) {
            return Err(refuse_at(
                "",
                &output.pos,
                &rules::INVALID_NAME,
                format!("output name {:?} cannot be an OpenTofu name", output.name),
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
    use crate::{Block, OutputBlock, NAME};
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

    fn output(name: &str) -> OutputBlock {
        OutputBlock {
            name: name.to_owned(),
            value: text("x"),
            sensitive: false,
            pos: Position::default(),
        }
    }

    fn config() -> Config {
        Config {
            blocks: vec![
                block(
                    Kind::Provider,
                    "aws",
                    "aws",
                    &[("region", text("eu-west-1"))],
                ),
                block(Kind::Backend, "s3", "state", &[("bucket", text("tf"))]),
                block(Kind::Encryption, "", "enc", &[]),
                block(Kind::Variable, "", "size", &[]),
                block(Kind::Local, "", "tags", &[("value", text("x"))]),
                block(Kind::Data, "aws_ami", "ubuntu", &[]),
                block(Kind::Ephemeral, "random_password", "pw", &[]),
                block(Kind::Module, "", "vpc", &[("source", text("./vpc"))]),
                block(Kind::Managed, "aws_instance", "web", &[]),
            ],
            required_providers: Default::default(),
            imports: Vec::new(),
            outputs: vec![output("address")],
        }
    }

    fn at<'a>(c: &'a mut Config, name: &str) -> &'a mut Block {
        c.blocks.iter_mut().find(|b| b.name == name).expect(name)
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Config);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "1web", |c| {
                at(c, "web").name = "1web".to_owned()
            }),
            ("invalid-param", "aws", |c| {
                at(c, "aws").body.insert("alias".to_owned(), Value::Int(1));
            }),
            ("duplicate-resource", "again", |c| {
                c.blocks.push(block(Kind::Provider, "aws", "again", &[]));
            }),
            ("duplicate-resource", "east2", |c| {
                for name in ["east", "east2"] {
                    c.blocks.push(block(
                        Kind::Provider,
                        "aws",
                        name,
                        &[("alias", text("east"))],
                    ));
                }
            }),
            ("duplicate-resource", "other", |c| {
                c.blocks.push(block(Kind::Backend, "local", "other", &[]));
            }),
            ("duplicate-resource", "other", |c| {
                c.blocks.push(block(Kind::Encryption, "", "other", &[]));
            }),
            ("missing-param", "vpc", |c| at(c, "vpc").body.clear()),
            ("missing-param", "vpc", |c| {
                at(c, "vpc").body.insert("source".to_owned(), Value::Int(1));
            }),
            ("missing-param", "tags", |c| at(c, "tags").body.clear()),
            ("invalid-param", "tags", |c| {
                at(c, "tags").body.insert("type".to_owned(), text("map"));
            }),
            ("invalid-name", "", |c| {
                c.outputs[0].name = "the address".to_owned()
            }),
            ("duplicate-output", "", |c| {
                c.outputs.push(output("address"))
            }),
        ];

        assert_eq!(validate(&config()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = config();
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
        for index in 0..config().blocks.len() {
            let mut c = config();
            c.blocks[index].name = "not valid".to_owned();
            let err = validate(&c).expect_err("a name with a space");
            assert_eq!(err.rule, "invalid-name", "block {index}");
            assert_eq!(err.pos.line, 5, "block {index}");
        }
        for good in ["a", "_", "web_1", "web-1", "Web"] {
            assert!(is_identifier(good), "{good:?}");
        }
        for bad in ["", "1", "-web", "web.1", "web 1", "wéb", "${x}"] {
            assert!(!is_identifier(bad), "{bad:?}");
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Config);
        let table: &[(&str, Change)] = &[
            ("one provider twice, told apart by alias", |c| {
                for name in ["east", "west"] {
                    c.blocks
                        .push(block(Kind::Provider, "aws", name, &[("alias", text(name))]));
                }
            }),
            ("a local whose value is not text", |c| {
                at(c, "tags").body.insert("value".to_owned(), Value::Null);
            }),
            ("two locals, two variables", |c| {
                c.blocks
                    .push(block(Kind::Local, "", "more", &[("value", text("y"))]));
                c.blocks.push(block(Kind::Variable, "", "count", &[]));
            }),
            ("no backend and no encryption", |c| {
                c.blocks
                    .retain(|b| !matches!(b.kind, Kind::Backend | Kind::Encryption));
            }),
            ("a sensitive output", |c| c.outputs[0].sensitive = true),
            ("nothing at all", |c| *c = Config::default()),
        ];
        for (what, change) in table {
            let mut c = config();
            change(&mut c);
            assert_eq!(validate(&c), Ok(()), "{what}");
        }
    }
}
