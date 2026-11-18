//! Stage three: the constraints that are CloudFormation's and not Meridian's.

use std::collections::BTreeMap;

use meridian_sdk::CompileError;

use crate::{refuse_at, Template, NAME};

/// The capabilities a stack can acknowledge.
const CAPABILITIES: &[&str] = &[
    "CAPABILITY_AUTO_EXPAND",
    "CAPABILITY_IAM",
    "CAPABILITY_NAMED_IAM",
];

pub(crate) fn validate(template: &Template) -> Result<(), CompileError> {
    let document = |rule: &str, msg: String| CompileError {
        target: NAME.to_owned(),
        rule: rule.to_owned(),
        msg,
        ..CompileError::default()
    };

    let name = &template.settings.name;
    let valid = name.len() <= 128
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(document(
            "invalid-stack-name",
            format!(
                "{name:?} cannot be a stack name: one starts with a letter and holds only \
                 letters, digits and hyphens. Name the stack with a resource of type \"stack\""
            ),
        ));
    }
    for capability in &template.settings.capabilities {
        if !CAPABILITIES.contains(&capability.as_str()) {
            return Err(document(
                "invalid-param",
                format!(
                    "{capability:?} is not a capability a stack can acknowledge (accepted: {})",
                    CAPABILITIES.join(", ")
                ),
            ));
        }
    }
    if template.entries.is_empty() {
        return Err(document(
            "empty-template",
            "a template needs at least one resource".to_owned(),
        ));
    }

    // Two ids can differ and still become the same logical id: web_vm and
    // web-vm are both WebVm.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for entry in &template.entries {
        if let Some(previous) = seen.insert(&entry.logical, &entry.id) {
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                "duplicate-logical-id",
                format!(
                    "resource ids {previous:?} and {:?} both become the logical id {:?}; \
                     CloudFormation's logical ids hold only letters and digits",
                    entry.id, entry.logical
                ),
            ));
        }
    }
    let mut outputs: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, logical, _, pos) in &template.outputs {
        if let Some(previous) = outputs.insert(logical, name) {
            return Err(refuse_at(
                "",
                pos,
                "duplicate-output",
                format!("outputs {previous:?} and {name:?} both become the logical id {logical:?}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plain, Entry, Settings};
    use meridian_sdk::{Position, Value};

    fn entry(id: &str, logical: &str) -> Entry {
        Entry {
            id: id.to_owned(),
            logical: logical.to_owned(),
            r#type: "AWS::S3::Bucket".to_owned(),
            properties: Vec::new(),
            attributes: Vec::new(),
            depends_on: Vec::new(),
            refs: Default::default(),
            pos: Position {
                line: 8,
                ..Position::default()
            },
        }
    }

    fn output(name: &str, logical: &str) -> (String, String, meridian_yaml::Yaml, Position) {
        (
            name.to_owned(),
            logical.to_owned(),
            plain(&Value::Null),
            Position::default(),
        )
    }

    fn template() -> Template {
        Template {
            document: "site".to_owned(),
            settings: Settings {
                name: "site-prod".to_owned(),
                region: "eu-west-1".to_owned(),
                capabilities: vec!["CAPABILITY_IAM".to_owned()],
            },
            parameters: Vec::new(),
            entries: vec![entry("web_vm", "WebVm"), entry("logs", "Logs")],
            outputs: vec![output("bucket_name", "BucketName")],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Template);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-param", "", |t| {
                t.settings.capabilities.push("CAPABILITY_ROOT".to_owned());
            }),
            ("invalid-param", "", |t| {
                t.settings.capabilities = vec!["capability_iam".to_owned()];
            }),
            ("empty-template", "", |t| t.entries.clear()),
            ("duplicate-logical-id", "web-vm", |t| {
                t.entries.push(entry("web-vm", "WebVm"));
            }),
            ("duplicate-output", "", |t| {
                t.outputs.push(output("bucket-name", "BucketName"));
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
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 8, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn a_stack_name_is_letters_digits_and_hyphens_from_a_letter() {
        let named = |name: &str| {
            let mut t = template();
            t.settings.name = name.to_owned();
            validate(&t)
        };
        let longest = "a".repeat(128);
        for good in ["a", "A", "site", "site-prod", "Site1", "a-", &longest] {
            assert_eq!(named(good), Ok(()), "{good:?}");
        }
        let too_long = "a".repeat(129);
        for bad in [
            "",
            "1site",
            "-site",
            "site_prod",
            "site.prod",
            "site prod",
            "sité",
            "site\n",
            &too_long,
        ] {
            let err = named(bad).expect_err(bad);
            assert_eq!(err.rule, "invalid-stack-name", "{bad:?}");
        }
    }

    #[test]
    fn every_capability_aws_knows_is_accepted() {
        for capability in CAPABILITIES {
            let mut t = template();
            t.settings.capabilities = vec![(*capability).to_owned()];
            assert_eq!(validate(&t), Ok(()), "{capability}");
        }
        let mut t = template();
        t.settings.capabilities = CAPABILITIES.iter().map(|c| (*c).to_owned()).collect();
        assert_eq!(validate(&t), Ok(()));
        t.settings.capabilities.clear();
        assert_eq!(validate(&t), Ok(()));
    }

    #[test]
    fn a_resource_and_an_output_may_share_a_logical_id() {
        let mut t = template();
        t.outputs.push(output("web_vm", "WebVm"));
        assert_eq!(validate(&t), Ok(()));
        t.outputs.clear();
        assert_eq!(validate(&t), Ok(()));
    }
}
