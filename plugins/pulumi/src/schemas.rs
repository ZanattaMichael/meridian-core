//! Asking `pulumi package get-schema` what each package's types take.
//!
//! Every Pulumi package publishes its schema, and `pulumi package
//! get-schema <package>[@<version>]` prints it, fetching the provider plugin
//! if it is not installed. This half of the plugin asks for each package the
//! documents use and turns the package's resources and functions into
//! resource schemas keyed the way a document writes a type: the resource's
//! token (`random:index:RandomPet`) and `invoke.<function token>`.
//!
//! The types the documents use are described property by property. Every
//! other resource and function of each package is listed by name, with its
//! description, so completion offers it; the next sync after a document
//! starts using one describes it in full. A package such as AWS has
//! thousands of types, and a cache that held every property of every one
//! would be too big for an editor to read at each start.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::schema::params_from_json_schema;
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, version_in, Log, ParamKind, ParamSchema, ResourceGraph, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, Value,
};

use crate::{INVOKE_OPTIONS, RESOURCE_OPTIONS};

/// Asks `pulumi` for the schemas of the packages the documents use.
pub struct PulumiSchemas {
    /// The program to run: `pulumi`.
    pub program: String,
}

impl PulumiSchemas {
    /// The source as the environment configures it, with the same variable
    /// the runner reads.
    pub fn from_env() -> Self {
        PulumiSchemas {
            program: std::env::var(crate::PROGRAM_VARIABLE)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "pulumi".to_owned()),
        }
    }

    fn pulumi(&self, request: &SchemaRequest) -> Tool {
        Tool::new(&self.program)
            .dir(&request.work_dir)
            .env("PULUMI_SKIP_UPDATE_CHECK", "true")
            .capture_stdout()
    }
}

impl SchemaSource for PulumiSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        std::fs::create_dir_all(&request.work_dir).map_err(|e| {
            RunError::new(format!(
                "could not create {}: {e}",
                request.work_dir.display()
            ))
        })?;
        let version = self
            .pulumi(request)
            .arg("version")
            .run_ok("pulumi version failed", log)?;
        let mut export =
            SchemaExport::new("pulumi", &version_in(&version.stdout).unwrap_or_default());

        let packages = packages_in(&request.graphs);
        if packages.is_empty() {
            export.note(
                "the documents use no package's types, so there was nothing to ask pulumi about",
            );
            return Ok(export);
        }
        let wanted: Vec<String> = request.types.iter().map(|t| canonical(t)).collect();
        for (package, version) in &packages {
            let name = match version {
                Some(version) => format!("{package}@{version}"),
                None => package.clone(),
            };
            log.err(&format!("reading the schema of {name}"));
            let shown = self
                .pulumi(request)
                .args(["package", "get-schema", &name])
                .run_ok(&format!("pulumi could not read the schema of {name}"), log)?;
            let document = decode_value(shown.stdout.as_bytes()).map_err(|e| {
                RunError::new(format!("the schema of {name} could not be read: {e}"))
            })?;
            let read = read_package(&document, &wanted);
            if let Some(version) = &read.version {
                export.pin(package, version);
            }
            export.schemas.extend(read.schemas);
            if read.named > 0 {
                export.note(format!(
                    "{package}: {} listed by name only; a type is described in full once a \
                     document uses it and the schemas are synced again",
                    count(read.named, "other type")
                ));
            }
        }
        for (wanted, original) in wanted.iter().zip(&request.types) {
            if is_package_type(original)
                && !export
                    .schemas
                    .iter()
                    .any(|s| canonical(&s.r#type) == *wanted)
            {
                export.note(format!("no package describes {original}"));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// Whether a type is a package's, as opposed to `config` or `stack`.
fn is_package_type(t: &str) -> bool {
    t != "config" && t != "stack" && t.contains(':')
}

/// The packages the documents use, with the version an explicit provider
/// or a `resourceOptions.version` pins, if any.
fn packages_in(graphs: &[ResourceGraph]) -> BTreeMap<String, Option<String>> {
    let mut packages: BTreeMap<String, Option<String>> = BTreeMap::new();
    for resource in graphs.iter().flat_map(|g| g.resources.iter()) {
        let t = resource.r#type.as_str();
        if !is_package_type(t) {
            continue;
        }
        let token = t.strip_prefix("invoke.").unwrap_or(t);
        let mut parts = token.split(':');
        let (Some(first), Some(second)) = (parts.next(), parts.next()) else {
            continue;
        };
        // An explicit provider, `pulumi:providers:<package>`, names the
        // package it configures.
        let package = if first == "pulumi" && second == "providers" {
            match parts.next() {
                Some(package) => package,
                None => continue,
            }
        } else {
            first
        };
        let version = resource
            .params
            .get(RESOURCE_OPTIONS)
            .or_else(|| resource.params.get(INVOKE_OPTIONS))
            .and_then(Value::as_map)
            .and_then(|o| o.get("version"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let entry = packages.entry(package.to_owned()).or_default();
        if entry.is_none() {
            *entry = version;
        }
    }
    packages
}

/// A token in the form a package's schema keys it by, so the two ways a
/// document may write one compare equal: `random:index:RandomPet` and
/// `random:index/randomPet:RandomPet` are the same type, and so is
/// `random:RandomPet`. The `invoke.` prefix of a function is kept.
pub(crate) fn canonical(token: &str) -> String {
    let (prefix, token) = match token.strip_prefix("invoke.") {
        Some(rest) => ("invoke.", rest),
        None => ("", token),
    };
    let parts: Vec<&str> = token.split(':').collect();
    let (package, module, name) = match parts.as_slice() {
        [package, name] => (*package, "index", *name),
        [package, module, name] => (*package, module.split('/').next().unwrap_or(module), *name),
        _ => return format!("{prefix}{token}"),
    };
    format!("{prefix}{package}:{module}:{name}")
}

/// What was read from one package's schema.
#[derive(Debug, Default)]
struct Read {
    version: Option<String>,
    schemas: Vec<ResourceSchema>,
    named: usize,
}

/// Reads a package schema. The resources and functions whose canonical
/// token is in `full` get their properties; the rest their name and
/// description.
fn read_package(document: &Value, full: &[String]) -> Read {
    let mut read = Read::default();
    let Some(package) = document.as_map() else {
        return read;
    };
    let text = |map: &BTreeMap<String, Value>, key: &str| -> String {
        map.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let name = text(package, "name");
    read.version = package
        .get("version")
        .and_then(Value::as_str)
        .map(|v| v.trim_start_matches('v').to_owned());
    let source = match &read.version {
        Some(version) => format!("{name} {version}"),
        None => name.clone(),
    };
    let types = package.get("types").and_then(Value::as_map);

    if let Some(resources) = package.get("resources").and_then(Value::as_map) {
        for (token, resource) in resources {
            let Some(resource) = resource.as_map() else {
                continue;
            };
            let description = first_paragraph(&text(resource, "description"));
            let mut schema = if full.contains(&canonical(token)) {
                let mut params = inputs(
                    resource.get("inputProperties"),
                    resource.get("requiredInputs"),
                    types,
                );
                params.extend(outputs(resource.get("properties"), &params, types));
                params.push(resource_options());
                params.sort_by(|a, b| a.name.cmp(&b.name));
                ResourceSchema::closed(token, &description).with_params(params)
            } else {
                read.named += 1;
                ResourceSchema::open(token, &description)
            };
            if matches!(resource.get("deprecationMessage"), Some(Value::String(_))) {
                schema.description = format!("Deprecated. {}", schema.description);
            }
            schema.source = source.clone();
            read.schemas.push(schema);
        }
    }
    if let Some(functions) = package.get("functions").and_then(Value::as_map) {
        for (token, function) in functions {
            let Some(function) = function.as_map() else {
                continue;
            };
            let name = format!("invoke.{token}");
            let description = first_paragraph(&text(function, "description"));
            let mut schema = if full.contains(&canonical(&name)) {
                let (inputs, required) = match function.get("inputs").and_then(Value::as_map) {
                    Some(inputs) => (inputs.get("properties"), inputs.get("required")),
                    None => (None, None),
                };
                let mut params = self::inputs(inputs, required, types);
                let outputs = function
                    .get("outputs")
                    .and_then(Value::as_map)
                    .and_then(|o| o.get("properties"));
                params.extend(self::outputs(outputs, &params, types));
                params.push(invoke_options());
                params.sort_by(|a, b| a.name.cmp(&b.name));
                ResourceSchema::closed(&name, &description).with_params(params)
            } else {
                read.named += 1;
                ResourceSchema::open(&name, &description)
            };
            schema.source = source.clone();
            read.schemas.push(schema);
        }
    }
    read
}

/// Reads an object schema with the package's `types` answering its `$ref`s.
fn convert<'a>(
    object: &'a Value,
    types: Option<&'a BTreeMap<String, Value>>,
) -> (Vec<ParamSchema>, bool) {
    let resolve = move |reference: &str| -> Option<&'a Value> {
        let token = reference.strip_prefix("#/types/")?;
        types?.get(token)
    };
    params_from_json_schema(object, &resolve)
}

/// The input properties as params, the required ones marked.
fn inputs(
    properties: Option<&Value>,
    required: Option<&Value>,
    types: Option<&BTreeMap<String, Value>>,
) -> Vec<ParamSchema> {
    let Some(properties) = properties.and_then(Value::as_map) else {
        return Vec::new();
    };
    let mut object = BTreeMap::new();
    object.insert("type".to_owned(), Value::String("object".to_owned()));
    object.insert("properties".to_owned(), Value::Map(properties.clone()));
    if let Some(required) = required {
        object.insert("required".to_owned(), required.clone());
    }
    let object = Value::Map(object);
    let (mut params, _) = convert(&object, types);
    for param in &mut params {
        let property = properties.get(&param.name).and_then(Value::as_map);
        if let Some(property) = property {
            if matches!(property.get("secret"), Some(Value::Bool(true))) {
                param.sensitive = true;
            }
            if let Some(Value::String(why)) = property.get("deprecationMessage") {
                param.deprecated = true;
                if param.description.is_empty() {
                    param.description = why.clone();
                }
            }
            param.description = first_paragraph(&param.description);
        }
    }
    params
}

/// The output-only properties, which a document reads and never writes.
fn outputs(
    properties: Option<&Value>,
    inputs: &[ParamSchema],
    types: Option<&BTreeMap<String, Value>>,
) -> Vec<ParamSchema> {
    let Some(properties) = properties.and_then(Value::as_map) else {
        return Vec::new();
    };
    let written: BTreeSet<&str> = inputs.iter().map(|p| p.name.as_str()).collect();
    let only_outputs: BTreeMap<String, Value> = properties
        .iter()
        .filter(|(name, _)| !written.contains(name.as_str()))
        .map(|(name, property)| (name.clone(), property.clone()))
        .collect();
    if only_outputs.is_empty() {
        return Vec::new();
    }
    let mut object = BTreeMap::new();
    object.insert("type".to_owned(), Value::String("object".to_owned()));
    object.insert("properties".to_owned(), Value::Map(only_outputs));
    let object = Value::Map(object);
    let (params, _) = convert(&object, types);
    params
        .into_iter()
        .map(|mut p| {
            p.description = first_paragraph(&p.description);
            p.computed()
        })
        .collect()
}

/// The `resourceOptions` param, which holds Pulumi's resource options.
fn resource_options() -> ParamSchema {
    let option =
        |name: &str, kind: ParamKind, description: &str| ParamSchema::new(name, kind, description);
    ParamSchema::new(
        RESOURCE_OPTIONS,
        ParamKind::Map,
        "Pulumi's resource options for this resource.",
    )
    .with_params(vec![
        option(
            "additionalSecretOutputs",
            ParamKind::List,
            "Output properties to treat as secrets.",
        ),
        option(
            "aliases",
            ParamKind::List,
            "Earlier names or URNs of the resource.",
        ),
        option(
            "customTimeouts",
            ParamKind::Map,
            "How long create, update and delete may take.",
        )
        .with_params(vec![
            option("create", ParamKind::String, ""),
            option("update", ParamKind::String, ""),
            option("delete", ParamKind::String, ""),
        ]),
        option(
            "deleteBeforeReplace",
            ParamKind::Bool,
            "Delete the old resource before creating its replacement.",
        ),
        option(
            "deletedWith",
            ParamKind::String,
            "A resource whose deletion deletes this one.",
        ),
        option("dependsOn", ParamKind::List, "Resources to create first."),
        option(
            "ignoreChanges",
            ParamKind::List,
            "Properties whose changes are not applied.",
        ),
        option(
            "import",
            ParamKind::String,
            "The id of an existing resource to adopt.",
        ),
        option("parent", ParamKind::String, "The parent resource."),
        option(
            "pluginDownloadURL",
            ParamKind::String,
            "Where the provider plugin is downloaded from.",
        ),
        option("protect", ParamKind::Bool, "Refuse to delete the resource."),
        option(
            "provider",
            ParamKind::String,
            "The explicit provider to use.",
        ),
        option(
            "providers",
            ParamKind::Map,
            "Explicit providers for child resources.",
        )
        .open(),
        option(
            "replaceOnChanges",
            ParamKind::List,
            "Properties whose changes replace the resource.",
        ),
        option(
            "retainOnDelete",
            ParamKind::Bool,
            "Keep the cloud resource when it leaves the program.",
        ),
        option(
            "version",
            ParamKind::String,
            "The provider plugin version to use.",
        ),
    ])
}

/// The `invokeOptions` param, which holds the options of a function call.
fn invoke_options() -> ParamSchema {
    ParamSchema::new(
        INVOKE_OPTIONS,
        ParamKind::Map,
        "Pulumi's options for this call.",
    )
    .with_params(vec![
        ParamSchema::new("dependsOn", ParamKind::List, "Resources to create first."),
        ParamSchema::new("parent", ParamKind::String, "The parent resource."),
        ParamSchema::new(
            "pluginDownloadURL",
            ParamKind::String,
            "Where the provider plugin is downloaded from.",
        ),
        ParamSchema::new(
            "provider",
            ParamKind::String,
            "The explicit provider to use.",
        ),
        ParamSchema::new(
            "version",
            ParamKind::String,
            "The provider plugin version to use.",
        ),
    ])
}

/// A package's descriptions run to pages, with examples in several
/// languages. An editor shows the first paragraph.
fn first_paragraph(text: &str) -> String {
    let text = text.trim();
    let end = text.find("\n\n").unwrap_or(text.len());
    text[..end].trim().to_owned()
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::Resource;

    #[test]
    fn tokens_compare_in_their_canonical_form() {
        assert_eq!(
            canonical("random:index:RandomPet"),
            "random:index:RandomPet"
        );
        assert_eq!(
            canonical("random:index/randomPet:RandomPet"),
            "random:index:RandomPet"
        );
        assert_eq!(canonical("random:RandomPet"), "random:index:RandomPet");
        assert_eq!(canonical("aws:s3/bucket:Bucket"), "aws:s3:Bucket");
        assert_eq!(
            canonical("invoke.aws:ec2/getAmi:getAmi"),
            "invoke.aws:ec2:getAmi"
        );
        assert_eq!(canonical("config"), "config");
    }

    #[test]
    fn the_packages_come_from_the_tokens_and_the_providers() {
        let mut graph = ResourceGraph::default();
        let mut push = |t: &str, options: Option<(&str, &str)>| {
            let mut params = BTreeMap::new();
            if let Some((key, value)) = options {
                params.insert(
                    RESOURCE_OPTIONS.to_owned(),
                    Value::Map(BTreeMap::from([(
                        key.to_owned(),
                        Value::String(value.to_owned()),
                    )])),
                );
            }
            graph.resources.push(Resource {
                id: t.to_owned(),
                r#type: t.to_owned(),
                params,
                ..Resource::default()
            });
        };
        push("config", None);
        push("stack", None);
        push("random:index:RandomPet", None);
        push("aws:s3/bucket:Bucket", Some(("version", "6.50.0")));
        push("pulumi:providers:azure-native", None);
        push("invoke.gcp:compute:getImage", None);
        let packages = packages_in(&[graph]);
        assert_eq!(
            packages,
            BTreeMap::from([
                ("aws".to_owned(), Some("6.50.0".to_owned())),
                ("azure-native".to_owned(), None),
                ("gcp".to_owned(), None),
                ("random".to_owned(), None),
            ])
        );
    }

    #[test]
    fn a_package_schema_becomes_resource_schemas() {
        let json = r##"{"name":"random","version":"v4.16.3",
          "types":{"random:index/Keepers:Keepers":{"type":"object","properties":{"k":{"type":"string"}}}},
          "resources":{
            "random:index/randomPet:RandomPet":{
              "description":"The resource `random.RandomPet` generates random pet names.\n\n## Example Usage\n\nlots",
              "properties":{
                "id":{"type":"string","description":"The random pet name."},
                "length":{"type":"integer","description":"The length (in words)."},
                "keepers":{"type":"object","additionalProperties":{"type":"string"},"description":"Arbitrary map."},
                "prefix":{"type":"string"},
                "separator":{"type":"string","description":"The character to separate words.\n\nMore."},
                "legacy":{"type":"string","deprecationMessage":"Do not use."},
                "secretly":{"type":"string","secret":true},
                "tags":{"$ref":"#/types/random:index/Keepers:Keepers"}},
              "inputProperties":{
                "length":{"type":"integer","description":"The length (in words)."},
                "keepers":{"type":"object","additionalProperties":{"type":"string"}},
                "prefix":{"type":"string"},
                "separator":{"type":"string","description":"The character to separate words.\n\nMore."},
                "legacy":{"type":"string","deprecationMessage":"Do not use."},
                "secretly":{"type":"string","secret":true},
                "tags":{"$ref":"#/types/random:index/Keepers:Keepers"}},
              "requiredInputs":["separator"]},
            "random:index/randomId:RandomId":{"description":"An id.","properties":{},"inputProperties":{"byteLength":{"type":"integer"}},"requiredInputs":["byteLength"]}
          },
          "functions":{
            "random:index/getThing:getThing":{"description":"Looks a thing up.","inputs":{"properties":{"name":{"type":"string"}},"required":["name"]},"outputs":{"properties":{"name":{"type":"string"},"found":{"type":"boolean"}}}}
          }}"##;
        let document = decode_value(json.as_bytes()).unwrap();
        let read = read_package(
            &document,
            &[
                "random:index:RandomPet".to_owned(),
                "invoke.random:index:getThing".to_owned(),
            ],
        );
        assert_eq!(read.version.as_deref(), Some("4.16.3"));
        assert_eq!(read.named, 1);
        let names: Vec<&str> = read.schemas.iter().map(|s| s.r#type.as_str()).collect();
        assert_eq!(
            names,
            [
                "random:index/randomId:RandomId",
                "random:index/randomPet:RandomPet",
                "invoke.random:index/getThing:getThing"
            ]
        );

        let pet = &read.schemas[1];
        assert_eq!(
            pet.description,
            "The resource `random.RandomPet` generates random pet names."
        );
        assert_eq!(pet.source, "random 4.16.3");
        assert!(!pet.open);
        let p = |name: &str| pet.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert!(p("separator").required);
        assert_eq!(
            p("separator").description,
            "The character to separate words."
        );
        assert!(p("length").kind == ParamKind::Integer && !p("length").required);
        assert!(p("keepers").kind == ParamKind::Map && p("keepers").open);
        assert!(
            p("id").computed,
            "an output-only property is read, not written"
        );
        assert_eq!(p("id").description, "The random pet name.");
        assert!(p("legacy").deprecated);
        assert_eq!(p("legacy").description, "Do not use.");
        assert!(p("secretly").sensitive);
        assert_eq!(
            p("tags").params[0].name,
            "k",
            "the $ref into types was followed"
        );
        assert!(p(RESOURCE_OPTIONS).find_param("protect").is_some());

        let id = &read.schemas[0];
        assert!(id.open && id.params.is_empty());
        assert_eq!(id.description, "An id.");

        let thing = &read.schemas[2];
        assert!(thing.find_param("name").unwrap().required);
        assert!(thing.find_param("found").unwrap().computed);
        assert!(thing.find_param(INVOKE_OPTIONS).is_some());
    }
}
