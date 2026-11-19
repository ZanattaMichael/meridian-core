//! Stage three: the constraints that are Chef's and not Meridian's.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, Value};

use crate::{refuse_at, Block, Cookbook, NAME};

/// The property Chef identifies the managed thing by, for the resources
/// where two blocks managing one thing would fight each other.
fn identity(block: &Block) -> Option<(&'static str, &Value)> {
    let property = match block.resource.as_str() {
        "package" => "package_name",
        "service" => "service_name",
        "file" | "directory" => "path",
        "link" => "target_file",
        "user" => "username",
        // A membership block shares its group with the block that creates it.
        "group" if !block.name.contains(':') => "group_name",
        _ => return None,
    };
    Some((property, block.properties.get(property)?))
}

pub(crate) fn validate(cookbook: &Cookbook) -> Result<(), CompileError> {
    if cookbook.blocks.is_empty() {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: rules::EMPTY_DOCUMENT.name.to_owned(),
            hint: rules::EMPTY_DOCUMENT.hint.to_owned(),
            msg: "there is nothing to converge: the document declares no resources".to_owned(),
            ..CompileError::default()
        });
    }
    // Chef merges or warns about two blocks of one name; neither is what the
    // author of two resources meant.
    let mut seen: BTreeMap<(&str, String), &str> = BTreeMap::new();
    for block in &cookbook.blocks {
        let Some((property, value)) = identity(block) else {
            continue;
        };
        let kind = if block.resource == "directory" {
            "file"
        } else {
            block.resource.as_str()
        };
        if let Some(previous) = seen.insert((kind, value.canonical()), &block.resource_id) {
            return Err(refuse_at(
                &block.resource_id,
                &block.pos,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "manages the same {kind} as resource {previous:?}: both set {property} to {}",
                    value.canonical()
                ),
            ));
        }
    }
    for block in &cookbook.blocks {
        if block.only_if.contains('\n') {
            return Err(refuse_at(
                &block.resource_id,
                &block.pos,
                &rules::INVALID_RUNTIME_CONDITION,
                "runtimeWhen must be one line of Ruby on this target".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::Position;

    fn block(resource: &'static str, id: &str, property: &str, value: &str) -> Block {
        Block {
            resource: resource.to_owned(),
            name: id.to_owned(),
            properties: [(property.to_owned(), Value::String(value.to_owned()))].into(),
            resource_id: id.to_owned(),
            pos: Position {
                line: 4,
                ..Position::default()
            },
            ..Block::default()
        }
    }

    fn cookbook() -> Cookbook {
        Cookbook {
            secrets: Vec::new(),
            name: "web".to_owned(),
            document: "web".to_owned(),
            blocks: vec![
                block("package", "pkg", "package_name", "nginx"),
                block("service", "svc", "service_name", "nginx"),
                block("file", "conf", "path", "/etc/nginx/nginx.conf"),
                block("directory", "root", "path", "/srv/www"),
                block("user", "deploy", "username", "deploy"),
                block("group", "ops", "group_name", "ops"),
            ],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        let twice = |resource: &'static str, property: &'static str, value: &'static str| {
            move |c: &mut Cookbook| c.blocks.push(block(resource, "again", property, value))
        };
        type Break = Box<dyn Fn(&mut Cookbook)>;
        let table: Vec<(&str, &str, Break)> = vec![
            ("empty-document", "", Box::new(|c| c.blocks.clear())),
            (
                "duplicate-resource",
                "again",
                Box::new(twice("package", "package_name", "nginx")),
            ),
            (
                "duplicate-resource",
                "again",
                Box::new(twice("service", "service_name", "nginx")),
            ),
            (
                "duplicate-resource",
                "again",
                Box::new(twice("file", "path", "/etc/nginx/nginx.conf")),
            ),
            // A file and a directory at one path are the same thing twice.
            (
                "duplicate-resource",
                "again",
                Box::new(twice("file", "path", "/srv/www")),
            ),
            (
                "duplicate-resource",
                "again",
                Box::new(twice("directory", "path", "/etc/nginx/nginx.conf")),
            ),
            (
                "duplicate-resource",
                "again",
                Box::new(twice("user", "username", "deploy")),
            ),
            (
                "duplicate-resource",
                "again",
                Box::new(twice("group", "group_name", "ops")),
            ),
            (
                "invalid-runtime-condition",
                "svc",
                Box::new(|c| c.blocks[1].only_if = "true\nfalse".to_owned()),
            ),
        ];

        assert_eq!(validate(&cookbook()), Ok(()));
        for (rule, resource, break_it) in &table {
            let mut broken = cookbook();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 4, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Cookbook);
        let table: &[(&str, Change)] = &[
            ("a package and a service of one name", |_| {}),
            ("a user and a group of one name", |c| {
                c.blocks.push(block("group", "g", "group_name", "deploy"));
            }),
            ("a membership block shares its group", |c| {
                let mut member = block("group", "m", "group_name", "ops");
                member.name = "ops:deploy".to_owned();
                c.blocks.push(member);
            }),
            ("two blocks of a resource with no identity", |c| {
                c.blocks.push(block("execute", "one", "command", "true"));
                c.blocks.push(block("execute", "two", "command", "true"));
            }),
            ("a block that lacks its identifying property", |c| {
                for id in ["one", "two"] {
                    let mut b = block("package", id, "version", "1.0");
                    b.name = id.to_owned();
                    c.blocks.push(b);
                }
            }),
            ("a one-line condition", |c| {
                c.blocks[0].only_if = "::File.exist?('/etc/debian_version')".to_owned();
            }),
        ];
        for (what, change) in table {
            let mut c = cookbook();
            change(&mut c);
            assert_eq!(validate(&c), Ok(()), "{what}");
        }
    }
}
