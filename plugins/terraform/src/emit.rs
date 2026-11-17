//! Stage four: a configuration becomes its file.

use std::collections::BTreeMap;

use meridian_sdk::Value;

use crate::{Config, Kind};

/// The one file a compile produces, in Terraform's JSON syntax.
pub const CONFIG_PATH: &str = "main.tf.json";

fn map(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Map(entries.into_iter().collect())
}

/// Inserts `value` at `outer.key.name`, creating the levels as needed.
fn nest(outer: &mut BTreeMap<String, Value>, key: &str, name: &str, value: Value) {
    let level = outer
        .entry(key.to_owned())
        .or_insert_with(|| Value::Map(BTreeMap::new()));
    if let Value::Map(level) = level {
        level.insert(name.to_owned(), value);
    }
}

/// Renders the configuration. This stage performs no mapping and no ordering.
/// Keys are sorted at every level, so the same document always renders to the
/// same bytes.
pub(crate) fn serialise(config: &Config) -> BTreeMap<String, String> {
    let mut root: BTreeMap<String, Value> = BTreeMap::new();
    let mut terraform: BTreeMap<String, Value> = BTreeMap::new();
    let mut providers: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut resources: BTreeMap<String, Value> = BTreeMap::new();
    let mut data: BTreeMap<String, Value> = BTreeMap::new();
    let mut locals: BTreeMap<String, Value> = BTreeMap::new();

    if !config.required_providers.is_empty() {
        terraform.insert(
            "required_providers".to_owned(),
            Value::Map(config.required_providers.clone()),
        );
    }

    for block in &config.blocks {
        let body = Value::Map(block.body.clone());
        match block.kind {
            Kind::Managed => nest(&mut resources, &block.r#type, &block.name, body),
            Kind::Data => nest(&mut data, &block.r#type, &block.name, body),
            Kind::Module => nest(&mut root, "module", &block.name, body),
            Kind::Variable => nest(&mut root, "variable", &block.name, body),
            Kind::Local => {
                // Validation has already required the value.
                let value = block.body.get("value").cloned().unwrap_or(Value::Null);
                locals.insert(block.name.clone(), value);
            }
            Kind::Provider => providers
                .entry(block.r#type.clone())
                .or_default()
                .push(body),
            Kind::Backend => nest(&mut terraform, "backend", &block.r#type, body),
        }
    }

    if !terraform.is_empty() {
        root.insert("terraform".to_owned(), Value::Map(terraform));
    }
    if !providers.is_empty() {
        // One configuration of a provider is written as an object; several,
        // told apart by alias, as a list of them.
        root.insert(
            "provider".to_owned(),
            map(providers.into_iter().map(|(name, mut bodies)| {
                let value = if bodies.len() == 1 {
                    bodies.remove(0)
                } else {
                    Value::List(bodies)
                };
                (name, value)
            })),
        );
    }
    if !resources.is_empty() {
        root.insert("resource".to_owned(), Value::Map(resources));
    }
    if !data.is_empty() {
        root.insert("data".to_owned(), Value::Map(data));
    }
    if !locals.is_empty() {
        root.insert("locals".to_owned(), Value::Map(locals));
    }
    if !config.outputs.is_empty() {
        root.insert(
            "output".to_owned(),
            map(config.outputs.iter().map(|(name, value, _)| {
                (name.clone(), map([("value".to_owned(), value.clone())]))
            })),
        );
    }

    BTreeMap::from([(CONFIG_PATH.to_owned(), Value::Map(root).pretty())])
}
