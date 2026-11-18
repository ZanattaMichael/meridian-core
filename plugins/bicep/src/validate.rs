//! Stage three: the constraints that are Bicep's and not Meridian's.

use std::collections::BTreeSet;

use meridian_sdk::CompileError;

use crate::{is_identifier, refuse_at, Template, NAME};

pub(crate) fn validate(template: &Template) -> Result<(), CompileError> {
    let document = |rule: &str, msg: String| CompileError {
        target: NAME.to_owned(),
        rule: rule.to_owned(),
        msg,
        ..CompileError::default()
    };

    if template.deployment.resource_group.is_empty() {
        return Err(document(
            "missing-deployment",
            "the document does not say which resource group to deploy to. Add a resource of \
             type \"deployment\" with a resourceGroup param"
                .to_owned(),
        ));
    }
    let name = &template.deployment.name;
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.()".contains(c));
    if !valid {
        return Err(document(
            "invalid-deployment-name",
            format!(
                "{name:?} cannot be a deployment name: one holds only letters, digits, hyphens, \
                 underscores, periods and parentheses"
            ),
        ));
    }
    if template.statements.is_empty() {
        return Err(document(
            "empty-template",
            "a Bicep file needs at least one resource".to_owned(),
        ));
    }

    for statement in &template.statements {
        if !is_identifier(&statement.symbol) {
            return Err(refuse_at(
                &statement.symbol,
                &statement.pos,
                "invalid-name",
                format!(
                    "resource id {:?} cannot be a Bicep symbolic name: one starts with a letter \
                     or underscore and holds only letters, digits and underscores",
                    statement.symbol
                ),
            ));
        }
        if !statement.body.iter().any(|(key, _)| key == "name") {
            return Err(refuse_at(
                &statement.symbol,
                &statement.pos,
                "missing-param",
                format!(
                    "resource type {:?} requires param \"name\" on this target: the resource's \
                     name in Azure",
                    statement.r#type
                ),
            ));
        }
    }

    let mut outputs = BTreeSet::new();
    for (name, _, pos) in &template.outputs {
        if !is_identifier(name) {
            return Err(refuse_at(
                "",
                pos,
                "invalid-name",
                format!("output name {name:?} cannot be a Bicep name"),
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
        if template.statements.iter().any(|s| &s.symbol == name) {
            return Err(refuse_at(
                "",
                pos,
                "duplicate-name",
                format!(
                    "output {name:?} has the same name as a resource; Bicep keeps both in one namespace"
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Deployment, Expr, Statement};
    use meridian_sdk::Position;

    fn raw(s: &str) -> Expr {
        Expr::Raw(s.to_owned())
    }

    fn statement(symbol: &str) -> Statement {
        Statement {
            symbol: symbol.to_owned(),
            r#type: "Microsoft.Storage/storageAccounts@2023-01-01".to_owned(),
            body: vec![("name".to_owned(), raw("'store'"))],
            azure_name: None,
            depends_on: Vec::new(),
            refs: Default::default(),
            pos: Position {
                line: 3,
                ..Position::default()
            },
        }
    }

    fn template() -> Template {
        Template {
            document: "site".to_owned(),
            deployment: Deployment {
                name: "site".to_owned(),
                resource_group: "rg-site".to_owned(),
                subscription: String::new(),
            },
            parameters: Vec::new(),
            statements: vec![statement("store"), statement("logs")],
            outputs: vec![("endpoint".to_owned(), raw("store.id"), Position::default())],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Template);
        let table: &[(&str, &str, Break)] = &[
            ("missing-deployment", "", |t| {
                t.deployment.resource_group.clear();
            }),
            ("empty-template", "", |t| t.statements.clear()),
            ("invalid-name", "web-vm", |t| {
                t.statements[0].symbol = "web-vm".to_owned();
            }),
            ("invalid-name", "1store", |t| {
                t.statements[1].symbol = "1store".to_owned();
            }),
            ("missing-param", "store", |t| t.statements[0].body.clear()),
            ("missing-param", "logs", |t| {
                t.statements[1].body = vec![("location".to_owned(), raw("'westeurope'"))];
            }),
            ("invalid-name", "", |t| {
                t.outputs[0].0 = "end-point".to_owned()
            }),
            ("duplicate-output", "", |t| {
                let again = t.outputs[0].clone();
                t.outputs.push(again);
            }),
            ("duplicate-name", "", |t| {
                t.outputs[0].0 = "store".to_owned()
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
                assert_eq!(err.pos.line, 3, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn a_deployment_name_is_at_most_64_safe_characters() {
        let named = |name: &str| {
            let mut t = template();
            t.deployment.name = name.to_owned();
            validate(&t)
        };
        let longest = "a".repeat(64);
        for good in [
            "a", "site", "site-1", "site_1", "site.1", "site(1)", "1", &longest,
        ] {
            assert_eq!(named(good), Ok(()), "{good:?}");
        }
        let too_long = "a".repeat(65);
        for bad in [
            "", "site 1", "site/1", "site:1", "sité", "site\n", "site'", &too_long,
        ] {
            let err = named(bad).expect_err(bad);
            assert_eq!(err.rule, "invalid-deployment-name", "{bad:?}");
        }
    }

    #[test]
    fn a_symbolic_name_is_a_bicep_identifier() {
        for good in ["a", "_", "_a", "store", "store1", "Store", "a_b"] {
            assert!(is_identifier(good), "{good:?}");
        }
        for bad in ["", "1", "1a", "a-b", "a.b", "a b", "sté", "a\n"] {
            assert!(!is_identifier(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_first_broken_rule_is_the_one_reported() {
        // The resource group is asked for before the name is looked at.
        let mut t = template();
        t.deployment.resource_group.clear();
        t.deployment.name = "not valid".to_owned();
        assert_eq!(validate(&t).unwrap_err().rule, "missing-deployment");

        let mut t = template();
        t.outputs.clear();
        assert_eq!(validate(&t), Ok(()));
    }
}
