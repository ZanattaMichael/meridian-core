//! Stage three: the constraints that are Compose's and not Meridian's.

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::CompileError;

use crate::{check, refuse_at, Composition, Section, NAME};

/// A Compose project name: lower-case letters, digits, hyphens and
/// underscores, starting with a letter or digit.
fn is_project_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub(crate) fn validate(composition: &Composition) -> Result<(), CompileError> {
    let whole = |rule: &Rule, msg: String| CompileError {
        target: NAME.to_owned(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        msg,
        ..CompileError::default()
    };
    if !is_project_name(&composition.project.name) {
        return Err(whole(
            &rules::INVALID_NAME,
            format!(
                "{:?} cannot be the name of a Compose project: a name holds only lower-case \
                 letters, digits, hyphens and underscores, and starts with a letter or digit. \
                 Set name on a resource of type \"project\"",
                composition.project.name
            ),
        ));
    }
    if !composition.project.host.is_empty() && !composition.project.context.is_empty() {
        return Err(whole(
            &rules::INVALID_PARAM,
            "the project names both a host and a context; Docker takes one or the other".to_owned(),
        ));
    }
    if !composition
        .elements
        .iter()
        .any(|e| e.section == Section::Service)
    {
        return Err(whole(
            &rules::EMPTY_DOCUMENT,
            "there is nothing to run: the document declares no service".to_owned(),
        ));
    }
    for element in &composition.elements {
        check::attributes(element)?;
        if element.section == Section::Service
            && !element.body.contains_key("image")
            && !element.body.contains_key("build")
            && !element.body.contains_key("extends")
            && !element.body.contains_key("provider")
        {
            return Err(refuse_at(
                &element.id,
                &element.pos,
                &rules::MISSING_PARAM,
                "a service needs an image to run, or a build to make one".to_owned(),
            ));
        }
    }
    check::references(composition)?;
    let mut outputs = std::collections::BTreeSet::new();
    for (name, _) in &composition.outputs {
        if !outputs.insert(name) {
            return Err(whole(
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
    use crate::{Element, Project};
    use meridian_sdk::{Position, Value};

    fn element(section: Section, id: &str, keys: &[(&str, &str)]) -> Element {
        Element {
            id: id.to_owned(),
            section,
            body: keys
                .iter()
                .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
                .collect(),
            pos: Position {
                line: 15,
                ..Position::default()
            },
        }
    }

    fn composition() -> Composition {
        Composition {
            project: Project {
                name: "site".to_owned(),
                ..Project::default()
            },
            elements: vec![
                element(Section::Service, "web", &[("image", "nginx:1.27")]),
                element(Section::Network, "back", &[]),
                element(Section::Volume, "data", &[]),
            ],
            outputs: vec![("address".to_owned(), Vec::new())],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Composition);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "", |c| c.project.name.clear()),
            ("invalid-name", "", |c| c.project.name = "Site".to_owned()),
            ("invalid-name", "", |c| c.project.name = "-site".to_owned()),
            ("invalid-name", "", |c| {
                c.project.name = "my.site".to_owned()
            }),
            ("invalid-param", "", |c| {
                c.project.host = "ssh://web01".to_owned();
                c.project.context = "remote".to_owned();
            }),
            ("empty-document", "", |c| {
                c.elements.retain(|e| e.section != Section::Service);
            }),
            ("empty-document", "", |c| c.elements.clear()),
            ("missing-param", "web", |c| c.elements[0].body.clear()),
            ("duplicate-output", "", |c| {
                c.outputs.push(("address".to_owned(), Vec::new()));
            }),
        ];

        assert_eq!(validate(&composition()), Ok(()));
        for (i, (rule, resource, break_it)) in table.iter().enumerate() {
            let mut broken = composition();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule, "row {i}");
            assert_eq!(err.resource, *resource, "row {i}: {rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 15, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn a_project_name_is_lower_case_and_starts_with_a_letter_or_digit() {
        for good in ["a", "0", "site", "site-1", "site_1", "1site"] {
            assert!(is_project_name(good), "{good:?}");
        }
        for bad in [
            "", "-site", "_site", "Site", "my.site", "my site", "sité", "site\n",
        ] {
            assert!(!is_project_name(bad), "{bad:?}");
        }
    }

    /// A service runs from an image, or from something that yields one.
    #[test]
    fn a_service_needs_one_way_to_get_its_image() {
        for key in ["image", "build", "extends", "provider"] {
            let mut c = composition();
            c.elements[0] = element(Section::Service, "web", &[(key, "x")]);
            let outcome = validate(&c);
            assert!(
                outcome
                    .as_ref()
                    .map_or_else(|e| e.rule != "missing-param", |()| true),
                "{key}: {outcome:?}"
            );
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Composition);
        let table: &[(&str, Change)] = &[
            ("a host and no context", |c| {
                c.project.host = "ssh://web01".to_owned()
            }),
            ("a context and no host", |c| {
                c.project.context = "remote".to_owned()
            }),
            ("a service alone", |c| c.elements.truncate(1)),
            ("no outputs", |c| c.outputs.clear()),
            ("two outputs with different names", |c| {
                c.outputs.push(("port".to_owned(), Vec::new()));
            }),
        ];
        for (what, change) in table {
            let mut c = composition();
            change(&mut c);
            assert_eq!(validate(&c), Ok(()), "{what}");
        }
    }
}
