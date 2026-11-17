//! Stage three: the constraints that are Terraform's and not Meridian's.

use std::collections::BTreeSet;

use meridian_sdk::{CompileError, Value};

use crate::{refuse_at, Config, Kind};

/// A name Terraform accepts for a block or an output: a letter or underscore,
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
    for block in &config.blocks {
        if !is_identifier(&block.name) {
            return Err(refuse_at(
                &block.name,
                &block.pos,
                "invalid-name",
                format!(
                    "resource id {:?} cannot be a Terraform name: a name starts with a letter \
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
                            "invalid-param",
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
                        "duplicate-provider",
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
                        "duplicate-backend",
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
                        "missing-param",
                        "resource type \"module\" requires param \"source\" on this target"
                            .to_owned(),
                    ));
                }
            }
            Kind::Local => {
                if !block.body.contains_key("value") {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        "missing-param",
                        "resource type \"local\" requires param \"value\" on this target"
                            .to_owned(),
                    ));
                }
                if let Some(extra) = block.body.keys().find(|k| *k != "value") {
                    return Err(refuse_at(
                        &block.name,
                        &block.pos,
                        "invalid-param",
                        format!(
                            "a local value has one param, \"value\"; {extra:?} has nowhere to go"
                        ),
                    ));
                }
            }
            Kind::Variable | Kind::Data | Kind::Managed => {}
        }
    }

    let mut outputs = BTreeSet::new();
    for (name, _, pos) in &config.outputs {
        if !is_identifier(name) {
            return Err(refuse_at(
                "",
                pos,
                "invalid-name",
                format!("output name {name:?} cannot be a Terraform name"),
            ));
        }
        if !outputs.insert(name) {
            return Err(refuse_at(
                "",
                pos,
                "duplicate-output",
                format!("output {name:?} is declared twice"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, NAME};
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
                block(Kind::Data, "aws_ami", "ubuntu", &[]),
                block(Kind::Module, "", "vpc", &[("source", text("./vpc"))]),
                block(Kind::Managed, "aws_instance", "web", &[]),
            ],
            required_providers: Default::default(),
            outputs: vec![("address".to_owned(), text("x"), Position::default())],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Config);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "1web", |c| {
                c.blocks[4].name = "1web".to_owned()
            }),
            ("invalid-param", "aws", |c| {
                c.blocks[0]
                    .body
                    .insert("alias".to_owned(), Value::Bool(true));
            }),
            ("duplicate-provider", "again", |c| {
                c.blocks.push(block(Kind::Provider, "aws", "again", &[]));
            }),
            ("duplicate-provider", "east2", |c| {
                c.blocks.push(block(
                    Kind::Provider,
                    "aws",
                    "east",
                    &[("alias", text("east"))],
                ));
                c.blocks.push(block(
                    Kind::Provider,
                    "aws",
                    "east2",
                    &[("alias", text("east"))],
                ));
            }),
            ("duplicate-backend", "other", |c| {
                c.blocks.push(block(Kind::Backend, "local", "other", &[]));
            }),
            ("missing-param", "vpc", |c| {
                c.blocks[3].body.clear();
            }),
            ("missing-param", "vpc", |c| {
                c.blocks[3]
                    .body
                    .insert("source".to_owned(), Value::Bool(true));
            }),
            ("invalid-name", "", |c| {
                c.outputs[0].0 = "the address".to_owned()
            }),
            ("duplicate-output", "", |c| {
                let again = c.outputs[0].clone();
                c.outputs.push(again);
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
    fn a_name_is_a_terraform_identifier() {
        for good in ["a", "_", "_a", "web", "web_1", "web-1", "Web", "a-", "a_"] {
            assert!(is_identifier(good), "{good:?}");
        }
        for bad in [
            "", "1", "1web", "-web", "web.1", "web 1", "web/1", "wéb", "web\n", "${x}", "a.b",
        ] {
            assert!(!is_identifier(bad), "{bad:?}");
        }
    }

    /// Every kind of block is held to the naming rule, not only resources.
    #[test]
    fn every_kind_of_block_needs_a_valid_name() {
        for index in 0..config().blocks.len() {
            let mut c = config();
            c.blocks[index].name = "not valid".to_owned();
            let err = validate(&c).expect_err("a name with a space");
            assert_eq!(err.rule, "invalid-name", "block {index}");
            assert_eq!(err.pos.line, 5, "block {index}");
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Config);
        let table: &[(&str, Change)] = &[
            ("one provider twice, told apart by alias", |c| {
                c.blocks.push(block(
                    Kind::Provider,
                    "aws",
                    "east",
                    &[("alias", text("east"))],
                ));
                c.blocks.push(block(
                    Kind::Provider,
                    "aws",
                    "west",
                    &[("alias", text("west"))],
                ));
            }),
            ("two different providers", |c| {
                c.blocks
                    .push(block(Kind::Provider, "azurerm", "azurerm", &[]));
            }),
            ("the same alias on two different providers", |c| {
                c.blocks.push(block(
                    Kind::Provider,
                    "google",
                    "g",
                    &[("alias", text("east"))],
                ));
                c.blocks.push(block(
                    Kind::Provider,
                    "azurerm",
                    "a",
                    &[("alias", text("east"))],
                ));
            }),
            ("two modules", |c| {
                c.blocks
                    .push(block(Kind::Module, "", "dns", &[("source", text("./dns"))]));
            }),
            ("no backend at all", |c| {
                c.blocks.retain(|b| b.kind != Kind::Backend)
            }),
            ("no outputs", |c| c.outputs.clear()),
            ("two outputs with different names", |c| {
                c.outputs
                    .push(("id".to_owned(), text("y"), Position::default()));
            }),
            ("nothing at all", |c| *c = Config::default()),
        ];
        for (what, change) in table {
            let mut c = config();
            change(&mut c);
            assert_eq!(validate(&c), Ok(()), "{what}");
        }
    }
}
