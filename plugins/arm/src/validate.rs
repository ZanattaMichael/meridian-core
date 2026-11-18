//! Stage three: the constraints that are Resource Manager's and not
//! Meridian's.

use std::collections::BTreeMap;

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, Position, Value, Warning};

use crate::expression;
use crate::{own, refuse_at, Scope, Template, NAME};

/// The types a template parameter can have.
pub(crate) const PARAMETER_TYPES: &[&str] = &[
    "array",
    "bool",
    "int",
    "object",
    "secureObject",
    "securestring",
    "string",
];

/// What a parameter's declaration may hold.
const PARAMETER_KEYS: &[&str] = &[
    "allowedValues",
    "defaultValue",
    "maxLength",
    "maxValue",
    "metadata",
    "minLength",
    "minValue",
    "nullable",
    "type",
];

/// Resource Manager's own limits on one template.
pub(crate) const MAX_RESOURCES: usize = 800;
pub(crate) const MAX_PARAMETERS: usize = 256;
pub(crate) const MAX_OUTPUTS: usize = 64;

/// Whether a parameter type holds a secret.
pub(crate) fn is_secure(parameter_type: &str) -> bool {
    parameter_type.eq_ignore_ascii_case("securestring")
        || parameter_type.eq_ignore_ascii_case("secureObject")
}

fn document(rule: &Rule, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        msg,
        ..CompileError::default()
    }
}

/// Letters, digits and `-_.()`, which is what Azure allows in the name of a
/// deployment and of a resource group.
fn is_azure_name(name: &str, max: usize) -> bool {
    !name.is_empty()
        && name.chars().count() <= max
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.()".contains(c))
}

pub(crate) fn validate(template: &Template) -> Result<Vec<Warning>, CompileError> {
    let settings = &template.settings;
    if !is_azure_name(&settings.name, 64) {
        return Err(document(
            &rules::INVALID_NAME,
            format!(
                "{:?} cannot be a deployment name: one holds up to 64 letters, digits and \
                 the characters -_.(). Name the deployment with a resource of type \"deployment\"",
                settings.name
            ),
        ));
    }
    match settings.scope {
        Scope::ResourceGroup => {
            if settings.resource_group.is_empty() {
                return Err(document(
                    &rules::MISSING_PARAM,
                    "a deployment needs the resource group it deploys to: add a resource of \
                     type \"deployment\" with param \"resourceGroup\", or give it scope \
                     \"subscription\""
                        .to_owned(),
                ));
            }
            if !is_azure_name(&settings.resource_group, 90)
                || settings.resource_group.ends_with('.')
            {
                return Err(document(
                    &rules::INVALID_PARAM,
                    format!(
                        "{:?} cannot be a resource group name: one holds up to 90 letters, \
                         digits and the characters -_.(), and does not end with a period",
                        settings.resource_group
                    ),
                ));
            }
            if !settings.location.is_empty() {
                return Err(document(
                    &rules::INVALID_PARAM,
                    "a deployment to a resource group takes no \"location\": it is stored in \
                     the group's own. Give each resource its location instead"
                        .to_owned(),
                ));
            }
        }
        Scope::Subscription => {
            if settings.location.is_empty() {
                return Err(document(
                    &rules::MISSING_PARAM,
                    "a deployment to a subscription needs param \"location\": the region that \
                     stores the deployment's record"
                        .to_owned(),
                ));
            }
            if !settings.resource_group.is_empty() {
                return Err(document(
                    &rules::INVALID_PARAM,
                    "a deployment to a subscription takes no \"resourceGroup\"".to_owned(),
                ));
            }
        }
    }
    match (settings.mode.as_str(), settings.scope) {
        ("Incremental", _) | ("Complete", Scope::ResourceGroup) => {}
        ("Complete", Scope::Subscription) => {
            return Err(document(
                &rules::INVALID_PARAM,
                "mode \"Complete\" exists only for a deployment to a resource group".to_owned(),
            ))
        }
        (other, _) => {
            return Err(document(
                &rules::INVALID_PARAM,
                format!("{other:?} is not a deployment mode (accepted: Incremental, Complete)"),
            ))
        }
    }

    if template.entries.is_empty() {
        return Err(document(
            &rules::EMPTY_DOCUMENT,
            "a template needs at least one resource".to_owned(),
        ));
    }
    for (what, count, max) in [
        ("resources", template.entries.len(), MAX_RESOURCES),
        ("parameters", template.parameters.len(), MAX_PARAMETERS),
        ("outputs", template.outputs.len(), MAX_OUTPUTS),
    ] {
        if count > max {
            return Err(document(
                &own::TEMPLATE_TOO_LARGE,
                format!("the document has {count} {what}, and a template holds at most {max}"),
            ));
        }
    }

    let mut warnings = Vec::new();
    for parameter in &template.parameters {
        let refuse =
            |rule: &Rule, msg: String| refuse_at(&parameter.name, &parameter.pos, rule, msg);
        for key in parameter.definition.keys() {
            if !PARAMETER_KEYS.contains(&key.as_str()) {
                return Err(refuse(
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "resource type \"parameter\" does not accept param {key:?} on this \
                         target (accepted: {})",
                        PARAMETER_KEYS.join(", ")
                    ),
                ));
            }
        }
        let parameter_type = match parameter.definition.get("type") {
            Some(Value::String(t)) => t,
            Some(_) => {
                return Err(refuse(
                    &rules::INVALID_PARAM,
                    "param \"type\" of a parameter must be text".to_owned(),
                ))
            }
            None => {
                return Err(refuse(
                    &rules::MISSING_PARAM,
                    format!(
                        "a parameter needs param \"type\" (accepted: {})",
                        PARAMETER_TYPES.join(", ")
                    ),
                ))
            }
        };
        if !PARAMETER_TYPES
            .iter()
            .any(|t| t.eq_ignore_ascii_case(parameter_type))
        {
            return Err(refuse(
                &rules::INVALID_PARAM,
                format!(
                    "{parameter_type:?} is not a type a parameter can have (accepted: {})",
                    PARAMETER_TYPES.join(", ")
                ),
            ));
        }
        check_expressions(
            &Value::Map(parameter.definition.clone()),
            &parameter.name,
            &parameter.pos,
        )?;
        // A secret written into the template is readable by anyone who can
        // read the deployment's history.
        let written_secret = matches!(
            parameter.definition.get("defaultValue"),
            Some(Value::String(v)) if !v.is_empty() && expression::inner(v).is_none()
        ) || matches!(
            parameter.definition.get("defaultValue"),
            Some(Value::Map(_) | Value::List(_))
        );
        if is_secure(parameter_type) && written_secret {
            warnings.push(Warning {
                target: NAME.to_owned(),
                resource: parameter.name.clone(),
                msg: format!(
                    "the {parameter_type} parameter has its default written in the document, \
                     so the secret is stored in the template and in the deployment's history; \
                     pass it when deploying instead"
                ),
            });
        }
    }

    // Azure compares types and names without regard to case.
    let mut seen: BTreeMap<(String, String), &str> = BTreeMap::new();
    for entry in &template.entries {
        let key = (entry.r#type.to_lowercase(), entry.name.to_lowercase());
        // A condition or a copy loop is how one template declares the same
        // name twice on purpose.
        let deliberate = entry.body.contains_key("condition") || entry.body.contains_key("copy");
        if let Some(previous) = seen.insert(key, &entry.id) {
            if !deliberate {
                return Err(refuse_at(
                    &entry.id,
                    &entry.pos,
                    &rules::DUPLICATE_RESOURCE,
                    format!(
                        "resources {previous:?} and {:?} are both the {} named {:?}; a template \
                         declares a resource once",
                        entry.id, entry.r#type, entry.name
                    ),
                ));
            }
        }
        check_expressions(&Value::String(entry.name.clone()), &entry.id, &entry.pos)?;
        check_expressions(&Value::Map(entry.body.clone()), &entry.id, &entry.pos)?;
    }

    let mut names: BTreeMap<String, &str> = BTreeMap::new();
    for (name, value, pos) in &template.outputs {
        if let Some(previous) = names.insert(name.to_lowercase(), name) {
            return Err(refuse_at(
                "",
                pos,
                &rules::DUPLICATE_OUTPUT,
                format!(
                    "outputs {previous:?} and {name:?} differ only in case, and Resource \
                     Manager does not tell them apart"
                ),
            ));
        }
        if let Some(inner) = expression::inner(value) {
            expression::check(inner).map_err(|why| {
                refuse_at(
                    "",
                    pos,
                    &own::INVALID_EXPRESSION,
                    format!(
                        "output {name:?}: {value:?} is not a well-formed ARM expression: {why}"
                    ),
                )
            })?;
        }
    }
    Ok(warnings)
}

/// Checks every expression anywhere inside a value.
fn check_expressions(value: &Value, resource: &str, pos: &Position) -> Result<(), CompileError> {
    match value {
        Value::String(text) => match expression::inner(text) {
            Some(inner) => expression::check(inner).map_err(|why| {
                refuse_at(
                    resource,
                    pos,
                    &own::INVALID_EXPRESSION,
                    format!("{text:?} is not a well-formed ARM expression: {why}"),
                )
            }),
            None => Ok(()),
        },
        Value::List(items) => items
            .iter()
            .try_for_each(|item| check_expressions(item, resource, pos)),
        Value::Map(map) => map
            .values()
            .try_for_each(|item| check_expressions(item, resource, pos)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entry, Parameter, Settings};

    fn text(s: &str) -> Value {
        Value::String(s.to_owned())
    }

    fn at_line() -> Position {
        Position {
            line: 21,
            ..Position::default()
        }
    }

    fn entry(id: &str, name: &str) -> Entry {
        Entry {
            id: id.to_owned(),
            r#type: "Microsoft.Storage/storageAccounts".to_owned(),
            api_version: "2023-01-01".to_owned(),
            name: name.to_owned(),
            resource_id: String::new(),
            body: [("location".to_owned(), text("westeurope"))].into(),
            depends: Default::default(),
            depends_on: Vec::new(),
            pos: at_line(),
        }
    }

    fn parameter(name: &str, r#type: &str) -> Parameter {
        Parameter {
            name: name.to_owned(),
            definition: [("type".to_owned(), text(r#type))].into(),
            pos: at_line(),
        }
    }

    fn template() -> Template {
        Template {
            document: "site".to_owned(),
            settings: Settings {
                name: "site".to_owned(),
                scope: Scope::ResourceGroup,
                resource_group: "rg-site".to_owned(),
                location: String::new(),
                subscription: String::new(),
                mode: "Incremental".to_owned(),
            },
            parameters: vec![parameter("size", "string")],
            entries: vec![entry("store", "store1"), entry("logs", "logs1")],
            outputs: vec![(
                "endpoint".to_owned(),
                "[reference('store1').primaryEndpoints.blob]".to_owned(),
                Position::default(),
            )],
        }
    }

    fn to_subscription(t: &mut Template) {
        t.settings.scope = Scope::Subscription;
        t.settings.resource_group.clear();
        t.settings.location = "westeurope".to_owned();
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Template);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "", |t| t.settings.name.clear()),
            ("invalid-name", "", |t| {
                t.settings.name = "my site".to_owned()
            }),
            ("invalid-name", "", |t| t.settings.name = "a".repeat(65)),
            ("missing-param", "", |t| t.settings.resource_group.clear()),
            ("invalid-param", "", |t| {
                t.settings.resource_group = "rg site".to_owned()
            }),
            ("invalid-param", "", |t| {
                t.settings.resource_group = "rg-site.".to_owned()
            }),
            ("invalid-param", "", |t| {
                t.settings.resource_group = "a".repeat(91)
            }),
            ("invalid-param", "", |t| {
                t.settings.location = "westeurope".to_owned()
            }),
            ("missing-param", "", |t| {
                to_subscription(t);
                t.settings.location.clear();
            }),
            ("invalid-param", "", |t| {
                to_subscription(t);
                t.settings.resource_group = "rg-site".to_owned();
            }),
            ("invalid-param", "", |t| {
                to_subscription(t);
                t.settings.mode = "Complete".to_owned();
            }),
            ("invalid-param", "", |t| {
                t.settings.mode = "incremental".to_owned()
            }),
            ("invalid-param", "", |t| t.settings.mode.clear()),
            ("empty-document", "", |t| t.entries.clear()),
            ("template-too-large", "", |t| {
                t.entries = (0..=MAX_RESOURCES)
                    .map(|i| entry(&format!("r{i}"), &format!("r{i}")))
                    .collect();
            }),
            ("template-too-large", "", |t| {
                t.parameters = (0..=MAX_PARAMETERS)
                    .map(|i| parameter(&format!("p{i}"), "string"))
                    .collect();
            }),
            ("template-too-large", "", |t| {
                t.outputs = (0..=MAX_OUTPUTS)
                    .map(|i| (format!("o{i}"), "x".to_owned(), Position::default()))
                    .collect();
            }),
            ("unsupported-param", "size", |t| {
                t.parameters[0]
                    .definition
                    .insert("description".to_owned(), text("x"));
            }),
            ("invalid-param", "size", |t| {
                t.parameters[0]
                    .definition
                    .insert("type".to_owned(), Value::Int(1));
            }),
            ("missing-param", "size", |t| {
                t.parameters[0].definition.clear()
            }),
            ("invalid-param", "size", |t| {
                t.parameters[0]
                    .definition
                    .insert("type".to_owned(), text("number"));
            }),
            ("invalid-expression", "size", |t| {
                t.parameters[0]
                    .definition
                    .insert("defaultValue".to_owned(), text("[concat('a', ]"));
            }),
            ("duplicate-resource", "logs", |t| {
                t.entries[1].name = "STORE1".to_owned()
            }),
            ("invalid-expression", "store", |t| {
                t.entries[0].name = "[concat('a', ]".to_owned();
            }),
            // An expression is found however deep in the body it sits.
            ("invalid-expression", "logs", |t| {
                t.entries[1].body.insert(
                    "properties".to_owned(),
                    Value::Map(
                        [(
                            "rules".to_owned(),
                            Value::List(vec![text("ok"), text("[concat('a', ]")]),
                        )]
                        .into(),
                    ),
                );
            }),
            ("duplicate-output", "", |t| {
                t.outputs
                    .push(("Endpoint".to_owned(), "x".to_owned(), Position::default()));
            }),
            ("invalid-expression", "", |t| {
                t.outputs[0].1 = "[concat('a', ]".to_owned()
            }),
        ];

        assert_eq!(validate(&template()), Ok(Vec::new()));
        for (i, (rule, resource, break_it)) in table.iter().enumerate() {
            let mut broken = template();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule, "row {i}");
            assert_eq!(err.resource, *resource, "row {i}: {rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 21, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Template);
        let table: &[(&str, Change)] = &[
            ("a deployment to a subscription", to_subscription),
            ("mode Complete on a resource group", |t| {
                t.settings.mode = "Complete".to_owned();
            }),
            ("names at their limits", |t| {
                t.settings.name = "a".repeat(64);
                t.settings.resource_group = "a".repeat(90);
            }),
            ("a name with letters beyond ASCII", |t| {
                t.settings.resource_group = "rg-café_(1)".to_owned();
            }),
            ("exactly as many of each as a template holds", |t| {
                t.entries = (0..MAX_RESOURCES)
                    .map(|i| entry(&format!("r{i}"), &format!("r{i}")))
                    .collect();
                t.parameters = (0..MAX_PARAMETERS)
                    .map(|i| parameter(&format!("p{i}"), "string"))
                    .collect();
                t.outputs = (0..MAX_OUTPUTS)
                    .map(|i| (format!("o{i}"), "x".to_owned(), Position::default()))
                    .collect();
            }),
            ("one name on two types", |t| {
                t.entries[1].name = "store1".to_owned();
                t.entries[1].r#type = "Microsoft.Network/virtualNetworks".to_owned();
            }),
            ("one name twice under a condition", |t| {
                t.entries[1].name = "store1".to_owned();
                t.entries[1]
                    .body
                    .insert("condition".to_owned(), Value::Bool(false));
            }),
            ("one name twice in a copy loop", |t| {
                t.entries[1].name = "store1".to_owned();
                t.entries[1].body.insert("copy".to_owned(), text("x"));
            }),
            ("a parameter type in another case", |t| {
                t.parameters[0]
                    .definition
                    .insert("type".to_owned(), text("SecureString"));
            }),
            ("no parameters and no outputs", |t| {
                t.parameters.clear();
                t.outputs.clear();
            }),
        ];
        for (what, change) in table {
            let mut t = template();
            change(&mut t);
            assert_eq!(validate(&t), Ok(Vec::new()), "{what}");
        }
    }

    #[test]
    fn every_parameter_type_and_key_is_accepted() {
        for parameter_type in PARAMETER_TYPES {
            let mut t = template();
            t.parameters = vec![parameter("p", parameter_type)];
            assert_eq!(validate(&t), Ok(Vec::new()), "{parameter_type}");
        }
        let mut t = template();
        for key in PARAMETER_KEYS {
            if *key != "type" {
                t.parameters[0]
                    .definition
                    .insert((*key).to_owned(), Value::Int(1));
            }
        }
        assert_eq!(validate(&t), Ok(Vec::new()));
    }

    /// A secret's default written into the document is kept, with a warning;
    /// one that is an expression, or empty, is not a written secret.
    #[test]
    fn a_secret_written_into_the_document_warns() {
        let with_default = |parameter_type: &str, default: Value| {
            let mut t = template();
            let mut p = parameter("password", parameter_type);
            p.definition.insert("defaultValue".to_owned(), default);
            t.parameters = vec![p];
            validate(&t).expect("a warning is not a refusal")
        };
        let written: &[(&str, Value)] = &[
            ("securestring", text("hunter2")),
            ("SecureString", text("hunter2")),
            ("secureObject", Value::Map(Default::default())),
            ("secureObject", Value::List(Vec::new())),
        ];
        for (parameter_type, default) in written {
            let warnings = with_default(parameter_type, default.clone());
            assert_eq!(warnings.len(), 1, "{parameter_type} {default:?}");
            assert_eq!(warnings[0].resource, "password");
            assert_eq!(warnings[0].target, NAME);
        }
        let quiet: &[(&str, Value)] = &[
            ("securestring", text("")),
            ("securestring", text("[newGuid()]")),
            ("string", text("hunter2")),
            ("securestring", Value::Null),
        ];
        for (parameter_type, default) in quiet {
            assert!(
                with_default(parameter_type, default.clone()).is_empty(),
                "{parameter_type} {default:?}"
            );
        }
        assert!(is_secure("SECURESTRING") && is_secure("secureobject") && !is_secure("string"));
    }
}
