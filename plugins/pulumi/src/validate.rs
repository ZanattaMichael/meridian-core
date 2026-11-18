//! Stage three: the constraints that are Pulumi's and not Meridian's.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::{CompileError, Position};

use crate::transform::PULUMI_NAMESPACE;
use crate::{refuse_at, Program};

/// A name the rest of a program can refer to inside `${}`: a letter or
/// underscore, then letters, digits, underscores and hyphens.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A project name: letters, digits, underscores, hyphens and periods.
fn is_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

/// A stack name, which may carry an organisation and project in front:
/// `acme/web/prod`.
fn is_stack_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').count() <= 3
        && name.split('/').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
        })
}

/// A provider setting's name: `<package>:<key>`.
fn is_setting_name(name: &str) -> bool {
    name.split_once(':').is_some_and(|(package, key)| {
        let word = |w: &str, extra: &str| {
            !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        word(package, "-_") && word(key, "-_.")
    })
}

pub(crate) fn validate(program: &Program) -> Result<(), CompileError> {
    if !is_project_name(&program.project) {
        return Err(refuse_at(
            "",
            &Position::default(),
            &rules::INVALID_NAME,
            format!(
                "the document's name {:?} cannot be a Pulumi project name: a project name holds \
                 only letters, digits, underscores, hyphens and periods",
                program.project
            ),
        ));
    }

    if let Some((id, pos)) = &program.settings.declared {
        if !is_stack_name(&program.settings.stack) {
            return Err(refuse_at(
                id,
                pos,
                &rules::INVALID_NAME,
                format!(
                    "{:?} cannot be a Pulumi stack name: a stack name holds only letters, \
                     digits, underscores, hyphens and periods, with an organisation and \
                     project in front at most",
                    program.settings.stack
                ),
            ));
        }
    }

    // Resources, function calls and configuration values share one set of
    // names in a program.
    let mut names: BTreeMap<&str, &str> = BTreeMap::new();
    for entry in &program.entries {
        let ok = if entry.addressable() {
            is_name(&entry.name)
        } else {
            is_setting_name(&entry.name)
        };
        if !ok {
            let what = if entry.name == entry.id {
                format!("resource id {:?}", entry.id)
            } else {
                format!("name {:?}", entry.name)
            };
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                &rules::INVALID_NAME,
                format!(
                    "{what} cannot be a name in a Pulumi program: a name starts with a letter \
                     or underscore and holds only letters, digits, underscores and hyphens"
                ),
            ));
        }
        if entry.name == PULUMI_NAMESPACE {
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                &rules::INVALID_NAME,
                "the name \"pulumi\" is Pulumi's own: ${pulumi.stack} and its like would no \
                 longer be readable"
                    .to_owned(),
            ));
        }
        if let Some(first) = names.insert(&entry.name, &entry.id) {
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "the name {:?} is already taken by resource {first:?}; resources, function \
                     calls and configuration values share one set of names in a Pulumi program",
                    entry.name
                ),
            ));
        }
    }

    let mut outputs = BTreeMap::new();
    for (name, _, pos) in &program.outputs {
        if name.is_empty() {
            return Err(refuse_at(
                "",
                pos,
                &rules::INVALID_NAME,
                "an output needs a name".to_owned(),
            ));
        }
        if outputs.insert(name, ()).is_some() {
            return Err(refuse_at(
                "",
                pos,
                &rules::DUPLICATE_OUTPUT,
                format!("output {name:?} is declared twice"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_pulumis_rules() {
        assert!(is_name("web_vm"));
        assert!(is_name("my-bucket"));
        assert!(is_name("_private"));
        assert!(!is_name("9lives"));
        assert!(!is_name("web.vm"));
        assert!(!is_name(""));

        assert!(is_project_name("web-vm"));
        assert!(is_project_name("web.vm_2"));
        assert!(!is_project_name("web vm"));
        assert!(!is_project_name(""));

        assert!(is_stack_name("dev"));
        assert!(is_stack_name("acme/web/prod"));
        assert!(!is_stack_name("a/b/c/d"));
        assert!(!is_stack_name("prod eu"));
        assert!(!is_stack_name("acme//prod"));

        assert!(is_setting_name("aws:region"));
        assert!(is_setting_name("azure-native:location"));
        assert!(!is_setting_name("aws:"));
        assert!(!is_setting_name("aws:region:extra"));
    }
}

#[cfg(test)]
mod rule_tables {
    use super::*;
    use crate::{Entry, Kind, Settings, NAME};
    use meridian_sdk::Value;

    fn entry(kind: Kind, id: &str, name: &str) -> Entry {
        Entry {
            kind,
            id: id.to_owned(),
            name: name.to_owned(),
            token: "aws:s3:Bucket".to_owned(),
            body: Default::default(),
            options: Default::default(),
            refs: Default::default(),
            pos: Position {
                line: 12,
                ..Position::default()
            },
        }
    }

    fn output(name: &str) -> (String, Value, Position) {
        (name.to_owned(), Value::Null, Position::default())
    }

    fn program() -> Program {
        Program {
            project: "site".to_owned(),
            settings: Settings {
                stack: "acme/site/prod".to_owned(),
                backend: String::new(),
                encryption: String::new(),
                declared: Some((
                    "stack".to_owned(),
                    Position {
                        line: 12,
                        ..Position::default()
                    },
                )),
            },
            entries: vec![
                entry(Kind::Config, "region", "aws:region"),
                entry(Kind::Config, "size", "size"),
                entry(Kind::Invoke, "ami", "ami"),
                entry(Kind::Managed, "bucket", "bucket"),
            ],
            outputs: vec![output("url")],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Program);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "", |p| p.project = "my site".to_owned()),
            ("invalid-name", "", |p| p.project.clear()),
            ("invalid-name", "", |p| p.project = "a".repeat(101)),
            ("invalid-name", "stack", |p| {
                p.settings.stack = "a/b/c/d".to_owned();
            }),
            ("invalid-name", "stack", |p| p.settings.stack.clear()),
            ("invalid-name", "bucket", |p| {
                p.entries[3].name = "my.bucket".to_owned();
            }),
            ("invalid-name", "ami", |p| {
                p.entries[2].name = "9ami".to_owned()
            }),
            ("invalid-name", "region", |p| {
                p.entries[0].name = "aws:".to_owned();
            }),
            ("invalid-name", "region", |p| {
                p.entries[0].name = "aws:region:extra".to_owned();
            }),
            // Only a configuration value may carry a package in front.
            ("invalid-name", "bucket", |p| {
                p.entries[3].name = "aws:bucket".to_owned();
            }),
            ("invalid-name", "bucket", |p| {
                p.entries[3].name = "pulumi".to_owned();
            }),
            ("duplicate-resource", "bucket", |p| {
                p.entries[3].name = "ami".to_owned();
            }),
            ("duplicate-resource", "again", |p| {
                p.entries.push(entry(Kind::Config, "again", "aws:region"));
            }),
            ("duplicate-resource", "bucket", |p| {
                p.entries[3].name = "size".to_owned();
            }),
            ("invalid-name", "", |p| p.outputs[0].0.clear()),
            ("duplicate-output", "", |p| p.outputs.push(output("url"))),
        ];

        assert_eq!(validate(&program()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = program();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 12, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Program);
        let table: &[(&str, Change)] = &[
            // A stack name nobody declared is the runner's default, and is
            // not this document's to get wrong.
            ("an undeclared stack is not checked", |p| {
                p.settings.declared = None;
                p.settings.stack = "not a stack".to_owned();
            }),
            ("a stack with no organisation", |p| {
                p.settings.stack = "dev".to_owned()
            }),
            ("one key under two packages", |p| {
                p.entries.push(entry(Kind::Config, "r2", "gcp:region"));
            }),
            ("an output named like a resource", |p| {
                p.outputs.push(output("bucket"))
            }),
            ("a program with nothing in it", |p| {
                p.entries.clear();
                p.outputs.clear();
            }),
            ("a project name of 100 characters", |p| {
                p.project = "a".repeat(100)
            }),
        ];
        for (what, change) in table {
            let mut p = program();
            change(&mut p);
            assert_eq!(validate(&p), Ok(()), "{what}");
        }
    }
}
