//! Stage three: the constraints that are Puppet's and not Meridian's.

use std::collections::{HashMap, HashSet};

use meridian_sdk::{CompileError, Value};

use crate::transform::mapping;
use crate::{refuse_declaration as fail, Attr, Declaration, Manifest, NAME};

/// Checks a manifest before anything is serialised. Every failure names the
/// offending resource and the rule it broke.
pub(crate) fn validate(manifest: &Manifest) -> Result<(), CompileError> {
    if manifest.host.is_empty() {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: "missing-host".to_owned(),
            msg: "manifest has no node to assign the class to".to_owned(),
            ..CompileError::default()
        });
    }

    // Puppet requires a title to be unique for its type across the whole
    // catalog. Meridian ids are already unique within a document, so this
    // catches a tree assembled some other way.
    let mut titles: HashMap<(&str, &str), &str> = HashMap::new();
    // The namevar check is the one that catches real documents. Two resources
    // with different ids can still name the same package or the same path,
    // and Puppet only discovers that at apply time, on the node.
    let mut names: HashMap<(&str, &str), &str> = HashMap::new();
    let declared: HashSet<&str> = manifest
        .resources
        .iter()
        .map(|d| d.resource_id.as_str())
        .collect();

    for d in &manifest.resources {
        check_title(d)?;
        if let Some(previous) = titles.insert((&d.r#type, &d.title), &d.resource_id) {
            return Err(fail(
                d,
                "duplicate-title",
                format!(
                    "a {} titled {:?} is already declared by resource {:?}; \
                     Puppet requires a title to be unique for its type across the catalog",
                    d.r#type, d.title, previous
                ),
            ));
        }

        if let Some(namevar) = namevar_of(d) {
            if let Some(previous) = names.insert((&d.r#type, namevar), &d.resource_id) {
                return Err(fail(
                    d,
                    "duplicate-namevar",
                    format!(
                        "this {} manages {:?}, which resource {:?} already manages; \
                         two declarations of one thing is a conflict Puppet reports on the \
                         node at apply time, not here, so it is refused now",
                        d.r#type, namevar, previous
                    ),
                ));
            }
        }

        // A type of Puppet's own may take nothing: a defined type with
        // defaults for every parameter is declared by its title alone.
        if d.attrs.is_empty() && !d.native {
            return Err(fail(
                d,
                "empty-resource",
                format!(
                    "{} resource {:?} was given no attributes",
                    d.r#type, d.title
                ),
            ));
        }

        for reference in d.require.iter().chain(&d.notify) {
            if !declared.contains(reference.title.as_str()) {
                return Err(fail(
                    d,
                    "dangling-reference",
                    format!("references {reference}, which this manifest does not declare"),
                ));
            }
        }
    }
    Ok(())
}

/// Rejects a title Puppet could not parse inside a declaration. A title is
/// written between single quotes, so a quote or a newline in one would change
/// the manifest's structure.
fn check_title(d: &Declaration) -> Result<(), CompileError> {
    if d.title.trim().is_empty() {
        return Err(fail(
            d,
            "empty-title",
            "resource has an empty title".to_owned(),
        ));
    }
    if d.title.contains(['\n', '\r']) {
        return Err(fail(
            d,
            "multiline-title",
            format!("resource title {:?} spans more than one line", d.title),
        ));
    }
    if d.title.contains('\'') {
        return Err(fail(
            d,
            "quoted-title",
            format!(
                "resource title {:?} contains a single quote, which would \
                 terminate the title Puppet reads",
                d.title
            ),
        ));
    }
    Ok(())
}

/// The value Puppet identifies the resource by, when it is a string.
fn namevar_of(d: &Declaration) -> Option<&str> {
    // The native type names are the IR type names for every type this target
    // maps, so the table is consulted by the declaration's own type. A
    // Puppet type of the same name reached through `native/` is in the
    // table too, and compared: `native/file` and `file` manage one path.
    let attribute = mapping(&d.r#type)?.namevar;
    match d.attrs.get(attribute)? {
        Attr::Value(Value::String(s)) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Reference;
    use meridian_sdk::Position;

    fn text(s: &str) -> Attr {
        Attr::Value(Value::String(s.to_owned()))
    }

    fn declaration(r#type: &str, id: &str, namevar: &str, name: &str) -> Declaration {
        Declaration {
            r#type: r#type.to_owned(),
            title: id.to_owned(),
            attrs: [
                (namevar.to_owned(), text(name)),
                ("ensure".to_owned(), Attr::Bare("present")),
            ]
            .into(),
            resource_id: id.to_owned(),
            pos: Position {
                line: 11,
                ..Position::default()
            },
            ..Declaration::default()
        }
    }

    fn to(r#type: &str, title: &str) -> Reference {
        Reference {
            r#type: r#type.to_owned(),
            title: title.to_owned(),
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            class: "web".to_owned(),
            host: "web01".to_owned(),
            resources: vec![
                declaration("package", "pkg", "name", "nginx"),
                Declaration {
                    require: vec![to("package", "pkg")],
                    ..declaration("file", "conf", "path", "/etc/nginx/nginx.conf")
                },
                Declaration {
                    notify: vec![to("file", "conf")],
                    ..declaration("service", "svc", "name", "nginx")
                },
            ],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Manifest);
        let table: &[(&str, &str, Break)] = &[
            ("missing-host", "", |m| m.host.clear()),
            ("empty-title", "pkg", |m| {
                m.resources[0].title = " ".to_owned()
            }),
            ("multiline-title", "pkg", |m| {
                m.resources[0].title = "pkg\nother".to_owned();
            }),
            ("multiline-title", "pkg", |m| {
                m.resources[0].title = "pkg\rother".to_owned();
            }),
            ("quoted-title", "pkg", |m| {
                m.resources[0].title = "pkg' }".to_owned();
            }),
            ("duplicate-title", "again", |m| {
                let mut d = declaration("package", "again", "name", "curl");
                d.title = "pkg".to_owned();
                m.resources.push(d);
            }),
            ("duplicate-namevar", "again", |m| {
                m.resources
                    .push(declaration("package", "again", "name", "nginx"));
            }),
            ("duplicate-namevar", "copy", |m| {
                m.resources
                    .push(declaration("file", "copy", "path", "/etc/nginx/nginx.conf"));
            }),
            ("empty-resource", "pkg", |m| m.resources[0].attrs.clear()),
            ("dangling-reference", "conf", |m| {
                m.resources[1].require.push(to("package", "gone"));
            }),
            ("dangling-reference", "svc", |m| {
                m.resources[2].notify = vec![to("exec", "gone")];
            }),
        ];

        assert_eq!(validate(&manifest()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = manifest();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 11, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Manifest);
        let table: &[(&str, Change)] = &[
            ("a package and a service of one name", |_| {}),
            ("one title on two different types", |m| {
                let mut d = declaration("user", "deploy", "name", "deploy");
                d.title = "pkg".to_owned();
                m.resources.push(d);
            }),
            ("a namevar that is not text is not compared", |m| {
                for id in ["one", "two"] {
                    let mut d = declaration("package", id, "name", "x");
                    d.attrs.insert(
                        "name".to_owned(),
                        Attr::Value(Value::List(vec![Value::String("x".to_owned())])),
                    );
                    m.resources.push(d);
                }
            }),
            ("a type with no namevar table is not compared", |m| {
                m.resources
                    .push(declaration("notify", "one", "message", "hello"));
                m.resources
                    .push(declaration("notify", "two", "message", "hello"));
            }),
            ("two files at different paths", |m| {
                m.resources
                    .push(declaration("file", "site", "path", "/etc/nginx/site.conf"));
            }),
            ("a manifest with nothing in it", |m| m.resources.clear()),
        ];
        for (what, change) in table {
            let mut m = manifest();
            change(&mut m);
            assert_eq!(validate(&m), Ok(()), "{what}");
        }
    }

    #[test]
    fn a_reference_is_written_the_way_puppet_reads_it() {
        assert_eq!(to("package", "nginx").to_string(), "Package['nginx']");
        assert_eq!(to("", "x").to_string(), "['x']");
    }
}
