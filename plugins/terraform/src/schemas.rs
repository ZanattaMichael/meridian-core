//! Asking `terraform providers schema -json` what the providers' types take.
//!
//! The compile needs no schema (see the crate docs on why the output is
//! JSON), but an editor does: without one it knows `aws_instance` only as a
//! name that fits `<provider>_<type>`. Terraform publishes every provider's
//! schema itself, so this half of the plugin writes a configuration that
//! requires the providers the documents use, initialises it in the work
//! directory the host keeps, and reads the schemas back. The types the
//! documents use are described param by param. Every other resource and
//! data source of each provider is listed by name, with its description,
//! so completion offers the types a document does not use yet; the next
//! sync after a document starts using one describes it in full. A
//! provider such as AWS has over a thousand types, and a cache that held
//! every param of every one would be too big for an editor to read at
//! each start.
//!
//! The same code serves the OpenTofu plugin, whose `tofu` prints the same
//! document.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, version_in, Log, ParamKind, ParamSchema, ResourceGraph, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, Value,
};

/// The configuration written into the work directory, which requires the
/// providers and nothing else.
pub const PROBE_PATH: &str = "meridian-schemas.tf.json";

/// The lock file `init` writes, which pins each provider's version.
const LOCK_PATH: &str = ".terraform.lock.hcl";

/// Where a provider with no `source` comes from.
const DEFAULT_NAMESPACE: &str = "hashicorp";

/// The provider built into the tool, whose `terraform_data` needs no
/// download.
const BUILTIN: &str = "terraform";

/// Resource types that are Meridian's own and not a provider's.
const OWN_TYPES: &[&str] = &[
    "backend",
    "local",
    "module",
    "provider",
    "variable",
    "encryption",
];

/// Asks the tool for the schemas of the providers the documents use.
pub struct TerraformSchemas {
    /// The program to run: `terraform`, or `tofu`.
    pub program: String,
    /// The tool's name as the cache records it.
    pub tool: String,
}

impl TerraformSchemas {
    /// A source that runs `program` and records the schemas under `tool`.
    pub fn new(program: impl Into<String>, tool: impl Into<String>) -> Self {
        TerraformSchemas {
            program: program.into(),
            tool: tool.into(),
        }
    }

    fn tool(&self, dir: &Path) -> Tool {
        Tool::new(&self.program)
            .dir(dir)
            .env("TF_IN_AUTOMATION", "1")
            .env("TF_INPUT", "0")
    }
}

impl SchemaSource for TerraformSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let version = self
            .tool(&request.work_dir)
            .arg("version")
            .capture_stdout()
            .run_ok(&format!("{} version failed", self.tool), log)?;
        let version = version_in(&version.stdout).unwrap_or_default();
        let mut export = SchemaExport::new(&self.tool, &version);

        let providers = providers_in(&request.graphs);
        fs::create_dir_all(&request.work_dir).map_err(|e| {
            RunError::new(format!(
                "could not create {}: {e}",
                request.work_dir.display()
            ))
        })?;
        let probe = request.work_dir.join(PROBE_PATH);
        fs::write(&probe, probe_config(&providers))
            .map_err(|e| RunError::new(format!("could not write {}: {e}", probe.display())))?;
        if providers.required.is_empty() && !providers.builtin {
            export.note(
                "the documents use no provider resource types, so there was nothing to ask \
                 the tool about",
            );
            return Ok(export);
        }

        self.tool(&request.work_dir)
            .args(["init", "-input=false", "-no-color", "-backend=false"])
            .run_ok(&format!("{} init failed", self.tool), log)?;
        let shown = self
            .tool(&request.work_dir)
            .args(["providers", "schema", "-json"])
            .capture_stdout()
            .run_ok(&format!("{} providers schema failed", self.tool), log)?;
        let document = decode_value(shown.stdout.as_bytes())
            .map_err(|e| RunError::new(format!("the provider schemas could not be read: {e}")))?;

        let pins =
            read_lock(&fs::read_to_string(request.work_dir.join(LOCK_PATH)).unwrap_or_default());
        for (address, version) in &pins {
            export.pin(address, version);
        }
        let read = read_schemas(&document, &pins, &request.types);
        export.schemas = read.schemas;
        if read.named > 0 {
            export.note(format!(
                "{} listed by name only; a type is described in full once a document uses it \
                 and the schemas are synced again",
                count(read.named, "other type")
            ));
        }
        for (local, source) in &providers.required {
            if !read
                .providers
                .iter()
                .any(|p| p.ends_with(&format!("/{source}")))
            {
                export.note(format!(
                    "provider {local} ({source}) was required, but the tool described no \
                     schemas for it"
                ));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// The providers a set of documents needs.
#[derive(Debug, Default, PartialEq, Eq)]
struct Providers {
    /// Local name to `namespace/type` source, with the version constraint
    /// the documents give, if any.
    required: BTreeMap<String, String>,
    constraints: BTreeMap<String, String>,
    /// Whether the built-in `terraform` provider is used.
    builtin: bool,
}

/// Reads the providers from the resources: a `provider` resource's `source`
/// and `version`, and the prefix of every `<provider>_<type>` and
/// `data.<type>`.
fn providers_in(graphs: &[ResourceGraph]) -> Providers {
    let mut providers = Providers::default();
    let mut used = BTreeSet::new();
    for graph in graphs {
        for resource in &graph.resources {
            let t = resource.r#type.as_str();
            if t == "provider" {
                let name = resource
                    .params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&resource.id)
                    .to_owned();
                if let Some(source) = resource.params.get("source").and_then(Value::as_str) {
                    providers.required.insert(name.clone(), source.to_owned());
                }
                if let Some(version) = resource.params.get("version").and_then(Value::as_str) {
                    providers
                        .constraints
                        .insert(name.clone(), version.to_owned());
                }
                used.insert(name);
                continue;
            }
            if OWN_TYPES.contains(&t) {
                continue;
            }
            let bare = t
                .strip_prefix("data.")
                .or_else(|| t.strip_prefix("ephemeral."))
                .unwrap_or(t);
            if let Some((prefix, _)) = bare.split_once('_') {
                used.insert(prefix.to_owned());
            }
        }
    }
    for name in used {
        if name == BUILTIN {
            providers.builtin = true;
        } else {
            providers
                .required
                .entry(name.clone())
                .or_insert_with(|| format!("{DEFAULT_NAMESPACE}/{name}"));
        }
    }
    providers
}

/// The JSON configuration that requires the providers. A `terraform_data`
/// resource is declared when the built-in provider is used, so its schema
/// is reported too.
fn probe_config(providers: &Providers) -> String {
    let mut required = BTreeMap::new();
    for (local, source) in &providers.required {
        let mut entry = BTreeMap::new();
        entry.insert("source".to_owned(), Value::String(source.clone()));
        if let Some(constraint) = providers.constraints.get(local) {
            entry.insert("version".to_owned(), Value::String(constraint.clone()));
        }
        required.insert(local.clone(), Value::Map(entry));
    }
    let mut terraform = BTreeMap::new();
    terraform.insert("required_providers".to_owned(), Value::Map(required));
    let mut config = BTreeMap::new();
    config.insert("terraform".to_owned(), Value::Map(terraform));
    if providers.builtin {
        let mut probe = BTreeMap::new();
        probe.insert(
            "meridian_schema_probe".to_owned(),
            Value::Map(BTreeMap::new()),
        );
        let mut resource = BTreeMap::new();
        resource.insert("terraform_data".to_owned(), Value::Map(probe));
        config.insert("resource".to_owned(), Value::Map(resource));
    }
    let mut text = Value::Map(config).canonical();
    text.push('\n');
    text
}

/// The provider versions `.terraform.lock.hcl` pins, keyed by the provider's
/// source without the registry host: `hashicorp/aws`.
fn read_lock(text: &str) -> BTreeMap<String, String> {
    let mut pins = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("provider ") {
            let address = rest.trim_end_matches('{').trim().trim_matches('"');
            current = Some(short_address(address).to_owned());
        } else if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim_start();
            if let (Some(rest), Some(address)) = (rest.strip_prefix('='), &current) {
                pins.insert(address.clone(), rest.trim().trim_matches('"').to_owned());
            }
        } else if line == "}" {
            current = None;
        }
    }
    pins
}

/// `registry.terraform.io/hashicorp/aws` as `hashicorp/aws`, and
/// `terraform.io/builtin/terraform` as `builtin/terraform`.
fn short_address(address: &str) -> &str {
    match address.splitn(2, '/').collect::<Vec<_>>().as_slice() {
        [host, rest] if host.contains('.') => rest,
        _ => address,
    }
}

/// What was read from the tool's document.
#[derive(Debug, Default)]
struct Read {
    schemas: Vec<ResourceSchema>,
    /// The provider addresses the document described.
    providers: Vec<String>,
    /// How many types were listed by name only.
    named: usize,
}

/// Reads `terraform providers schema -json`. The types in `full` get their
/// params; the rest get their name and description.
fn read_schemas(document: &Value, pins: &BTreeMap<String, String>, full: &[String]) -> Read {
    let mut read = Read::default();
    let Some(providers) = document
        .as_map()
        .and_then(|d| d.get("provider_schemas"))
        .and_then(Value::as_map)
    else {
        return read;
    };
    for (address, provider) in providers {
        let short = short_address(address);
        read.providers.push(short.to_owned());
        let source = match pins.get(short) {
            Some(version) => format!("{short} {version}"),
            None => short.to_owned(),
        };
        let Some(provider) = provider.as_map() else {
            continue;
        };
        for (key, prefix) in [
            ("resource_schemas", ""),
            ("data_source_schemas", "data."),
            ("ephemeral_resource_schemas", "ephemeral."),
        ] {
            let Some(types) = provider.get(key).and_then(Value::as_map) else {
                continue;
            };
            for (name, schema) in types {
                let Some(block) = schema.as_map().and_then(|s| s.get("block")) else {
                    continue;
                };
                let full_name = format!("{prefix}{name}");
                let mut resource = if full.contains(&full_name) {
                    let (params, description) = read_block(block);
                    let mut resource =
                        ResourceSchema::closed(&full_name, &description).with_params(params);
                    resource.params.extend(meta_params(prefix));
                    resource
                } else {
                    read.named += 1;
                    let description = block
                        .as_map()
                        .map(|b| text(b, "description").to_owned())
                        .unwrap_or_default();
                    ResourceSchema::open(&full_name, &description)
                };
                resource.source = source.clone();
                read.schemas.push(resource);
            }
        }
    }
    read
}

/// The params of a block: its attributes and its nested blocks.
fn read_block(block: &Value) -> (Vec<ParamSchema>, String) {
    let mut params = Vec::new();
    let Some(block) = block.as_map() else {
        return (params, String::new());
    };
    let description = text(block, "description").to_owned();
    if let Some(attributes) = block.get("attributes").and_then(Value::as_map) {
        for (name, attribute) in attributes {
            if let Some(attribute) = attribute.as_map() {
                params.push(read_attribute(name, attribute));
            }
        }
    }
    if let Some(blocks) = block.get("block_types").and_then(Value::as_map) {
        for (name, nested) in blocks {
            let Some(nested) = nested.as_map() else {
                continue;
            };
            let (inner, description) = read_block(nested.get("block").unwrap_or(&Value::Null));
            let mode = text(nested, "nesting_mode");
            let mut param =
                ParamSchema::new(name, nesting_kind(mode), &description).with_params(inner);
            if mode == "map" {
                param.open = true;
            }
            if nested
                .get("min_items")
                .and_then(number)
                .is_some_and(|n| n >= 1.0)
            {
                param = param.required();
            }
            params.push(param);
        }
    }
    params.sort_by(|a, b| a.name.cmp(&b.name));
    (params, description)
}

/// One attribute of a block.
fn read_attribute(name: &str, attribute: &BTreeMap<String, Value>) -> ParamSchema {
    let flag = |key: &str| matches!(attribute.get(key), Some(Value::Bool(true)));
    let mut description = text(attribute, "description").to_owned();
    let optional = flag("optional");
    let computed = flag("computed");
    let (kind, nested, open) = match attribute.get("nested_type") {
        Some(Value::Map(nested)) => {
            let (inner, _) = read_block(&Value::Map(nested.clone()));
            let mode = text(nested, "nesting_mode");
            (nesting_kind(mode), inner, mode == "map")
        }
        _ => read_type(attribute.get("type").unwrap_or(&Value::Null)),
    };
    if optional && computed {
        if !description.is_empty() && !description.ends_with('.') {
            description.push('.');
        }
        if !description.is_empty() {
            description.push(' ');
        }
        description.push_str("Set by the provider when the document leaves it out.");
    }
    let mut param = ParamSchema::new(name, kind, &description).with_params(nested);
    param.open = open;
    param.sensitive = flag("sensitive");
    param.deprecated = flag("deprecated");
    if flag("required") {
        param = param.required();
    }
    if computed && !optional {
        param = param.computed();
    }
    param
}

/// The kind of a `type`, with the fields of an object type as nested params.
/// Object fields come without an optionality, so none is marked required.
fn read_type(t: &Value) -> (ParamKind, Vec<ParamSchema>, bool) {
    match t {
        Value::String(name) => (
            match name.as_str() {
                "string" => ParamKind::String,
                "number" => ParamKind::Number,
                "bool" => ParamKind::Bool,
                _ => ParamKind::Any,
            },
            Vec::new(),
            false,
        ),
        Value::List(parts) => match parts.as_slice() {
            [Value::String(c), inner] if c == "list" || c == "set" => {
                let (_, nested, open) = read_type(inner);
                (ParamKind::List, nested, open)
            }
            [Value::String(c), _] if c == "map" => (ParamKind::Map, Vec::new(), true),
            [Value::String(c), Value::Map(fields)] if c == "object" => {
                let mut nested: Vec<ParamSchema> = fields
                    .iter()
                    .map(|(name, t)| {
                        let (kind, inner, open) = read_type(t);
                        let mut param = ParamSchema::new(name, kind, "").with_params(inner);
                        param.open = open;
                        param
                    })
                    .collect();
                nested.sort_by(|a, b| a.name.cmp(&b.name));
                (ParamKind::Map, nested, false)
            }
            [Value::String(c), _] if c == "tuple" => (ParamKind::List, Vec::new(), false),
            _ => (ParamKind::Any, Vec::new(), false),
        },
        _ => (ParamKind::Any, Vec::new(), false),
    }
}

fn nesting_kind(mode: &str) -> ParamKind {
    match mode {
        "list" | "set" => ParamKind::List,
        _ => ParamKind::Map,
    }
}

/// The meta-arguments every resource or data source of the kind takes,
/// which no provider's schema lists.
fn meta_params(prefix: &str) -> Vec<ParamSchema> {
    let mut params = vec![
        ParamSchema::new(
            "provider",
            ParamKind::String,
            "The provider configuration to use, as `<provider>.<alias>`, when it is not the \
             default one.",
        ),
        ParamSchema::new(
            "count",
            ParamKind::Integer,
            "How many instances to create. Terraform's own meta-argument.",
        ),
        ParamSchema::new(
            "for_each",
            ParamKind::Any,
            "A map or set of strings, one instance per entry. Terraform's own meta-argument.",
        ),
    ];
    let mut lifecycle = vec![
        ParamSchema::new(
            "precondition",
            ParamKind::List,
            "Conditions checked before the resource is planned.",
        ),
        ParamSchema::new(
            "postcondition",
            ParamKind::List,
            "Conditions checked after the resource is applied.",
        ),
    ];
    if prefix.is_empty() {
        lifecycle.extend([
            ParamSchema::new("create_before_destroy", ParamKind::Bool, ""),
            ParamSchema::new("prevent_destroy", ParamKind::Bool, ""),
            ParamSchema::new(
                "ignore_changes",
                ParamKind::List,
                "Attributes whose changes outside Terraform are kept.",
            ),
            ParamSchema::new(
                "replace_triggered_by",
                ParamKind::List,
                "References whose change replaces this resource.",
            ),
        ]);
        lifecycle.sort_by(|a, b| a.name.cmp(&b.name));
        params.push(
            ParamSchema::new(
                "lifecycle",
                ParamKind::Map,
                "How Terraform plans changes to this resource.",
            )
            .with_params(lifecycle),
        );
        params.push(ParamSchema::new(
            "provisioner",
            ParamKind::Any,
            "Provisioners to run after the resource is created.",
        ));
        params.push(
            ParamSchema::new(
                "connection",
                ParamKind::Map,
                "How provisioners reach the resource.",
            )
            .open(),
        );
    } else {
        params.push(
            ParamSchema::new(
                "lifecycle",
                ParamKind::Map,
                "Conditions checked around reading the data source.",
            )
            .with_params(lifecycle),
        );
    }
    params
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn text<'a>(map: &'a BTreeMap<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(*n as f64),
        Value::Uint(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::Resource;

    fn graph(types: &[(&str, &[(&str, &str)])]) -> ResourceGraph {
        let mut graph = ResourceGraph::default();
        for (i, (t, params)) in types.iter().enumerate() {
            graph.resources.push(Resource {
                id: format!("r{i}"),
                r#type: (*t).to_owned(),
                params: params
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
                    .collect(),
                ..Resource::default()
            });
        }
        graph
    }

    #[test]
    fn the_providers_come_from_the_types_and_the_provider_resources() {
        let graphs = vec![
            graph(&[
                ("aws_instance", &[]),
                ("data.aws_ami", &[]),
                ("terraform_data", &[]),
                ("module", &[]),
                ("variable", &[]),
                (
                    "provider",
                    &[
                        ("name", "oci"),
                        ("source", "oracle/oci"),
                        ("version", "~> 6.0"),
                    ],
                ),
            ]),
            graph(&[("local_file", &[])]),
        ];
        let providers = providers_in(&graphs);
        assert!(providers.builtin);
        assert_eq!(
            providers.required,
            BTreeMap::from([
                ("aws".to_owned(), "hashicorp/aws".to_owned()),
                ("local".to_owned(), "hashicorp/local".to_owned()),
                ("oci".to_owned(), "oracle/oci".to_owned()),
            ])
        );
        assert_eq!(providers.constraints.get("oci").unwrap(), "~> 6.0");
        let config = decode_value(probe_config(&providers).as_bytes()).unwrap();
        let required =
            &config.as_map().unwrap()["terraform"].as_map().unwrap()["required_providers"];
        let required = required.as_map().unwrap();
        assert_eq!(
            required["oci"].as_map().unwrap()["source"].as_str(),
            Some("oracle/oci")
        );
        assert_eq!(
            required["oci"].as_map().unwrap()["version"].as_str(),
            Some("~> 6.0")
        );
        assert!(!required["aws"].as_map().unwrap().contains_key("version"));
        let probe = &config.as_map().unwrap()["resource"].as_map().unwrap()["terraform_data"];
        assert!(probe
            .as_map()
            .unwrap()
            .contains_key("meridian_schema_probe"));
    }

    #[test]
    fn the_lock_file_pins_each_provider() {
        let text = "# This file is maintained automatically by \"terraform init\".\n\
                    provider \"registry.terraform.io/hashicorp/local\" {\n  \
                    version     = \"2.5.2\"\n  constraints = \"~> 2.0\"\n  hashes = [\n    \
                    \"h1:abc\",\n  ]\n}\n\nprovider \"registry.opentofu.org/hashicorp/aws\" {\n  \
                    version = \"5.60.0\"\n}\n";
        assert_eq!(
            read_lock(text),
            BTreeMap::from([
                ("hashicorp/local".to_owned(), "2.5.2".to_owned()),
                ("hashicorp/aws".to_owned(), "5.60.0".to_owned()),
            ])
        );
        assert_eq!(
            short_address("terraform.io/builtin/terraform"),
            "builtin/terraform"
        );
    }

    #[test]
    fn a_provider_schema_becomes_resource_schemas() {
        let json = r#"{"format_version":"1.0","provider_schemas":{"registry.terraform.io/hashicorp/local":{
            "provider":{"version":0,"block":{}},
            "resource_schemas":{"local_file":{"version":0,"block":{
                "attributes":{
                    "content":{"type":"string","description":"Content to store.","description_kind":"plain","optional":true},
                    "filename":{"type":"string","description":"The path.","required":true},
                    "id":{"type":"string","computed":true},
                    "file_permission":{"type":"string","optional":true,"computed":true,"description":"Permissions"},
                    "sensitive_content":{"type":"string","optional":true,"sensitive":true,"deprecated":true},
                    "tags":{"type":["map","string"],"optional":true},
                    "ports":{"type":["list",["object",{"from":"number","to":"number"}]],"optional":true},
                    "settings":{"nested_type":{"attributes":{"name":{"type":"string","required":true}},"nesting_mode":"single"},"optional":true}
                },
                "block_types":{
                    "source":{"nesting_mode":"list","block":{"attributes":{"path":{"type":"string","required":true}},"description":"Where from."},"min_items":1,"max_items":1},
                    "timeouts":{"nesting_mode":"single","block":{"attributes":{"create":{"type":"string","optional":true}}}}
                },
                "description":"Generates a local file."
            }}},
            "data_source_schemas":{"local_file":{"version":0,"block":{"attributes":{"filename":{"type":"string","required":true},"content":{"type":"string","computed":true}}}}}
        }}}"#;
        let document = decode_value(json.as_bytes()).unwrap();
        let pins = BTreeMap::from([("hashicorp/local".to_owned(), "2.5.2".to_owned())]);
        let read = read_schemas(&document, &pins, &["local_file".to_owned()]);
        assert_eq!(read.providers, ["hashicorp/local"]);
        let names: Vec<&str> = read.schemas.iter().map(|s| s.r#type.as_str()).collect();
        assert_eq!(names, ["local_file", "data.local_file"]);
        assert_eq!(read.named, 1);

        let file = &read.schemas[0];
        assert_eq!(file.description, "Generates a local file.");
        assert_eq!(file.source, "hashicorp/local 2.5.2");
        assert!(!file.open);
        let p = |name: &str| file.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert!(p("filename").required && p("filename").kind == ParamKind::String);
        assert!(!p("content").required && !p("content").computed);
        assert!(p("id").computed);
        let permission = p("file_permission");
        assert!(!permission.computed);
        assert_eq!(
            permission.description,
            "Permissions. Set by the provider when the document leaves it out."
        );
        assert!(p("sensitive_content").sensitive && p("sensitive_content").deprecated);
        assert!(p("tags").kind == ParamKind::Map && p("tags").open);
        let ports = p("ports");
        assert_eq!(ports.kind, ParamKind::List);
        assert_eq!(ports.params[0].name, "from");
        assert_eq!(ports.params[0].kind, ParamKind::Number);
        assert!(!ports.params[0].required);
        let settings = p("settings");
        assert_eq!(settings.kind, ParamKind::Map);
        assert!(settings.params[0].required);
        let source = p("source");
        assert!(source.required && source.kind == ParamKind::List);
        assert_eq!(source.description, "Where from.");
        assert_eq!(source.params[0].name, "path");
        assert!(!p("timeouts").required && p("timeouts").kind == ParamKind::Map);
        // Terraform's own meta-arguments are there too.
        assert!(p("lifecycle").find_param("prevent_destroy").is_some());
        assert!(p("count").kind == ParamKind::Integer);

        // The data source was not asked for, so it is known by name.
        let data = &read.schemas[1];
        assert!(data.open && data.params.is_empty());
        assert_eq!(data.source, "hashicorp/local 2.5.2");

        let read = read_schemas(&document, &pins, &["data.local_file".to_owned()]);
        let data = &read.schemas[1];
        assert!(data.find_param("content").unwrap().computed);
        assert!(data.find_param("provisioner").is_none());
        assert!(data
            .find_param("lifecycle")
            .unwrap()
            .find_param("precondition")
            .is_some());
    }
}
