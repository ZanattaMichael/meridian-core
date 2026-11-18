//! Stage four: a template becomes its files.

use std::collections::BTreeMap;

use meridian_sdk::Value;
use meridian_yaml::Yaml;

use crate::{Files, Template};

/// The template itself.
pub const TEMPLATE_PATH: &str = "template.yaml";
/// How to deploy it: the stack's name and settings, and the ids the author
/// wrote for each logical id. The runner reads it; CloudFormation never does.
pub const STACK_PATH: &str = "stack.json";

/// Renders the template. This stage performs no mapping and no ordering.
/// Resources are written in the document's apply order and properties in
/// sorted order, so the same document always renders to the same bytes.
pub(crate) fn serialise(template: &Template) -> Files {
    let mut resources = Vec::with_capacity(template.entries.len());
    for entry in &template.entries {
        let mut body = vec![("Type".to_owned(), Yaml::str(&entry.r#type))];
        if !entry.depends_on.is_empty() {
            body.push((
                "DependsOn".to_owned(),
                Yaml::Seq(entry.depends_on.iter().map(Yaml::str).collect()),
            ));
        }
        body.extend(entry.attributes.iter().cloned());
        if !entry.properties.is_empty() {
            body.push(("Properties".to_owned(), Yaml::Map(entry.properties.clone())));
        }
        resources.push((entry.logical.clone(), Yaml::Map(body)));
    }

    let mut root = vec![
        (
            "AWSTemplateFormatVersion".to_owned(),
            Yaml::str("2010-09-09"),
        ),
        (
            "Description".to_owned(),
            Yaml::str(format!(
                "Compiled by Meridian from the document {}. Do not edit.",
                template.document
            )),
        ),
    ];
    if !template.parameters.is_empty() {
        // A secret's parameter: NoEcho, so CloudFormation shows `****` in
        // place of its value wherever it describes the stack or its change
        // sets.
        root.push((
            "Parameters".to_owned(),
            Yaml::Map(
                template
                    .parameters
                    .iter()
                    .map(|name| {
                        (
                            name.clone(),
                            Yaml::Map(vec![
                                ("Type".to_owned(), Yaml::str("String")),
                                ("NoEcho".to_owned(), Yaml::Bool(true)),
                            ]),
                        )
                    })
                    .collect(),
            ),
        ));
    }
    root.push(("Resources".to_owned(), Yaml::Map(resources)));
    if !template.outputs.is_empty() {
        root.push((
            "Outputs".to_owned(),
            Yaml::Map(
                template
                    .outputs
                    .iter()
                    .map(|(_, logical, value, _)| {
                        (
                            logical.clone(),
                            Yaml::Map(vec![("Value".to_owned(), value.clone())]),
                        )
                    })
                    .collect(),
            ),
        ));
    }

    let names = |pairs: Vec<(&String, &String)>| {
        Value::Map(
            pairs
                .into_iter()
                .map(|(logical, written)| (logical.clone(), Value::String(written.clone())))
                .collect(),
        )
    };
    let stack = Value::Map(BTreeMap::from([
        (
            "name".to_owned(),
            Value::String(template.settings.name.clone()),
        ),
        (
            "region".to_owned(),
            Value::String(template.settings.region.clone()),
        ),
        (
            "capabilities".to_owned(),
            Value::List(
                template
                    .settings
                    .capabilities
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        ),
        (
            "resources".to_owned(),
            names(
                template
                    .entries
                    .iter()
                    .map(|e| (&e.logical, &e.id))
                    .collect(),
            ),
        ),
        (
            "outputs".to_owned(),
            names(
                template
                    .outputs
                    .iter()
                    .map(|(name, logical, _, _)| (logical, name))
                    .collect(),
            ),
        ),
    ]));

    Files::from([
        (
            TEMPLATE_PATH.to_owned(),
            meridian_yaml::write(&Yaml::Map(root)),
        ),
        (STACK_PATH.to_owned(), stack.pretty()),
    ])
}
