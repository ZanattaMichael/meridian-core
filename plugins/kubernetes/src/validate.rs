//! Stage three: the constraints that are Kubernetes' and not Meridian's.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::CompileError;

use crate::{refuse_at, Manifests, NAME};

/// A DNS subdomain name: lower-case letters, digits, hyphens and periods,
/// starting and ending with a letter or digit, at most 253 characters.
fn is_object_name(name: &str) -> bool {
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    !name.is_empty()
        && name.len() <= 253
        && name.starts_with(edge)
        && name.ends_with(edge)
        && name.chars().all(|c| edge(c) || c == '-' || c == '.')
}

pub(crate) fn validate(manifests: &Manifests) -> Result<(), CompileError> {
    if manifests.objects.is_empty() {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: rules::EMPTY_DOCUMENT.name.to_owned(),
            hint: rules::EMPTY_DOCUMENT.hint.to_owned(),
            msg: "there is nothing to apply: the document declares no objects".to_owned(),
            ..CompileError::default()
        });
    }
    let mut seen: BTreeMap<(String, &str, &str), &str> = BTreeMap::new();
    for object in &manifests.objects {
        if !is_object_name(&object.name) {
            return Err(refuse_at(
                &object.id,
                &object.pos,
                &rules::INVALID_NAME,
                format!(
                    "{:?} cannot be the name of a Kubernetes object: a name holds only \
                     lower-case letters, digits, hyphens and periods, and starts and ends with \
                     a letter or digit. Set metadata.name",
                    object.name
                ),
            ));
        }
        let key = (
            object.get_as(),
            object.namespace.as_str(),
            object.name.as_str(),
        );
        if let Some(previous) = seen.insert(key, &object.id) {
            return Err(refuse_at(
                &object.id,
                &object.pos,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "this {} is named {:?}, which resource {previous:?} already is; the second \
                     would overwrite the first",
                    object.kind, object.name
                ),
            ));
        }
    }
    let mut outputs = BTreeMap::new();
    for (name, _) in &manifests.outputs {
        if outputs.insert(name, ()).is_some() {
            return Err(CompileError {
                target: NAME.to_owned(),
                rule: rules::DUPLICATE_OUTPUT.name.to_owned(),
                hint: rules::DUPLICATE_OUTPUT.hint.to_owned(),
                msg: format!("output {name:?} is declared twice"),
                ..CompileError::default()
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Object;
    use meridian_sdk::Position;

    fn object(id: &str, api_version: &str, kind: &str, namespace: &str, name: &str) -> Object {
        Object {
            id: id.to_owned(),
            api_version: api_version.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            body: Default::default(),
            pos: Position {
                line: 6,
                ..Position::default()
            },
        }
    }

    fn manifests() -> Manifests {
        Manifests {
            objects: vec![
                object("app", "apps/v1", "Deployment", "web", "web"),
                object("svc", "v1", "Service", "web", "web"),
            ],
            outputs: vec![("address".to_owned(), Vec::new())],
            ..Manifests::default()
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Manifests);
        let table: &[(&str, &str, Break)] = &[
            ("empty-document", "", |m| m.objects.clear()),
            ("invalid-name", "app", |m| {
                m.objects[0].name = "Web".to_owned()
            }),
            ("invalid-name", "svc", |m| m.objects[1].name.clear()),
            ("duplicate-resource", "again", |m| {
                m.objects
                    .push(object("again", "apps/v1", "Deployment", "web", "web"));
            }),
            // The kind is compared without regard to case.
            ("duplicate-resource", "again", |m| {
                m.objects
                    .push(object("again", "v1", "service", "web", "web"));
            }),
            ("duplicate-output", "", |m| {
                m.outputs.push(("address".to_owned(), Vec::new()));
            }),
        ];

        assert_eq!(validate(&manifests()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = manifests();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 6, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn an_object_name_is_a_dns_subdomain() {
        let longest = "a".repeat(253);
        for good in [
            "a",
            "0",
            "web",
            "web-1",
            "web.example.com",
            "a-b.c-d",
            "1a",
            &longest,
        ] {
            assert!(is_object_name(good), "{good:?}");
        }
        let too_long = "a".repeat(254);
        for bad in [
            "", "-", ".", "-web", "web-", ".web", "web.", "Web", "web_1", "web 1", "web/1", "wéb",
            "web\n", &too_long,
        ] {
            assert!(!is_object_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Manifests);
        let table: &[(&str, Change)] = &[
            ("two kinds under one name", |_| {}),
            ("one name in two namespaces", |m| {
                m.objects
                    .push(object("other", "apps/v1", "Deployment", "api", "web"));
            }),
            ("one kind name in two API groups", |m| {
                m.objects.push(object(
                    "other",
                    "example.com/v1",
                    "Deployment",
                    "web",
                    "web",
                ));
            }),
            ("two versions of one group", |m| {
                m.objects
                    .push(object("other", "apps/v1beta1", "Deployment", "web", "web"));
            }),
            ("no outputs", |m| m.outputs.clear()),
            ("two outputs with different names", |m| {
                m.outputs.push(("port".to_owned(), Vec::new()));
            }),
        ];
        for (what, change) in table {
            let mut m = manifests();
            change(&mut m);
            assert_eq!(validate(&m), Ok(()), "{what}");
        }
    }
}
