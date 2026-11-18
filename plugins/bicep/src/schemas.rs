//! Reading the Azure resource type definitions Bicep compiles against.
//!
//! The Bicep compiler knows every Azure resource type, API version by API
//! version, from the type definitions in
//! [Azure/bicep-types-az](https://github.com/Azure/bicep-types-az), which
//! are generated from the Azure REST API specifications and published as
//! files: an index of every `<Namespace>/<type>@<api-version>` and, per
//! provider and version, a `types.json` holding the type graph. This half
//! of the plugin fetches the index and the files the documents' types live
//! in, describes those types property by property, and lists every other
//! type of the index by name, so completion offers it.
//!
//! The same files describe what an ARM template takes, since Bicep
//! compiles to one, so the ARM plugin reads them through this source too;
//! the two differ only in how a document writes a type (`@<api-version>`
//! on the type for Bicep, an `apiVersion` param for ARM).

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::fetch::first_paragraph;
use meridian_sdk::{
    decode_value, fetch, Log, ParamKind, ParamSchema, ResourceSchema, RunError, SchemaExport,
    SchemaRequest, SchemaSource, Value,
};

/// Where the generated type definitions are published.
pub const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/Azure/bicep-types-az/main/generated/";

/// How a document writes an Azure resource type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// `Microsoft.Storage/storageAccounts@2023-01-01`: the API version is
    /// part of the type.
    Bicep,
    /// `Microsoft.Storage/storageAccounts` with an `apiVersion` param.
    Arm,
}

/// Reads the published type definitions.
pub struct AzureSchemas {
    pub dialect: Dialect,
    /// The directory the index and the type files are fetched from, with
    /// its trailing slash. Changed only by tests.
    pub url: String,
}

impl AzureSchemas {
    pub fn new(dialect: Dialect) -> Self {
        AzureSchemas {
            dialect,
            url: DEFAULT_URL.to_owned(),
        }
    }
}

/// Meridian's own types on these targets, which the index does not have.
const OWN_TYPES: &[&str] = &["deployment", "parameter"];

/// How many mappings inside mappings a type graph is read down to.
const MAX_DEPTH: usize = 12;

/// The property flags of a type definition.
const REQUIRED: i64 = 1;
const READ_ONLY: i64 = 2;
const WRITE_ONLY: i64 = 4;

impl SchemaSource for AzureSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let index = fetch(
            &format!("{}index.json", self.url),
            &request.work_dir,
            "index.json",
            "the index of Azure resource types",
            log,
        )?;
        let mut export = SchemaExport::new("bicep-types-az", &index.pin());
        export.pin("index", &index.pin());
        if let Some(note) = &index.note {
            export.note(note.clone());
        }
        let index = decode_value(&index.bytes).map_err(|e| {
            RunError::new(format!(
                "the index of Azure resource types could not be read: {e}"
            ))
        })?;
        let catalogue = Catalogue::read(&index);

        // Which `<type>@<version>` each used type means, in this dialect.
        let used = self.used(request, &catalogue);

        // The files those live in, each fetched once.
        let mut files: BTreeMap<String, Option<TypeFile>> = BTreeMap::new();
        let mut described: Vec<ResourceSchema> = Vec::new();
        for (written, versioned) in &used {
            let Some(versioned) = versioned else {
                export.note(format!(
                    "{written} is not in the index of Azure resource types"
                ));
                continue;
            };
            let Some(reference) = catalogue.resources.get(versioned) else {
                export.note(format!(
                    "{written} is not in the index of Azure resource types"
                ));
                continue;
            };
            let (path, number) = reference;
            let file = files.entry(path.clone()).or_insert_with(|| {
                match self.fetch_types(path, &request.work_dir, log) {
                    Ok((fetched, document)) => {
                        let pin = fetched.pin();
                        let where_ = path.trim_end_matches("/types.json");
                        export.pin(where_, &pin);
                        if let Some(note) = fetched.note {
                            export.note(note);
                        }
                        Some(TypeFile {
                            source: format!("bicep-types-az {where_} {pin}"),
                            types: document,
                        })
                    }
                    Err(e) => {
                        export.note(format!("{written}: {}", e.msg));
                        None
                    }
                }
            });
            let Some(file) = file else { continue };
            match file.resource(*number, self.dialect, written, versioned, &catalogue) {
                Some(schema) => described.push(schema),
                None => export.note(format!(
                    "{written} is in the index but {path} does not describe it"
                )),
            }
        }
        let described_names: BTreeSet<String> =
            described.iter().map(|s| s.r#type.clone()).collect();
        let mut named = 0usize;
        for (lower, versions) in &catalogue.by_base {
            let base = catalogue.spelling(lower);
            match self.dialect {
                Dialect::Bicep => {
                    let used_base = used.keys().any(|w| {
                        w.split('@').next().map(str::to_ascii_lowercase).as_deref() == Some(lower)
                    });
                    for version in versions {
                        let name = format!("{base}@{version}");
                        // Every version of a used type, and the newest of
                        // the rest, so completion offers a type without
                        // listing thirty years of API versions of each.
                        if described_names.contains(&name)
                            || !(used_base || Some(version) == newest(versions))
                        {
                            continue;
                        }
                        named += 1;
                        described.push(ResourceSchema::open(&name, ""));
                    }
                }
                Dialect::Arm => {
                    if described_names.iter().any(|d| d.eq_ignore_ascii_case(base)) {
                        continue;
                    }
                    named += 1;
                    let mut schema = ResourceSchema::open(base, "");
                    schema.params.push(api_version_param(versions));
                    described.push(schema);
                }
            }
        }
        export.schemas = described;
        if named > 0 {
            export.note(format!(
                "{} listed by name only; a type is described in full once a document uses it \
                 and the schemas are synced again",
                count(named, "other type")
            ));
        }
        export.tidy();
        Ok(export)
    }
}

impl AzureSchemas {
    /// Each type the documents use, as written, with the `<type>@<version>`
    /// it means in the index: the version the document names, else the
    /// newest the index has.
    fn used(
        &self,
        request: &SchemaRequest,
        catalogue: &Catalogue,
    ) -> BTreeMap<String, Option<String>> {
        let mut used = BTreeMap::new();
        for written in &request.types {
            if OWN_TYPES.contains(&written.as_str()) || !written.contains('/') {
                continue;
            }
            let versioned = match self.dialect {
                Dialect::Bicep => catalogue.exact(written),
                Dialect::Arm => {
                    let version = request
                        .graphs
                        .iter()
                        .flat_map(|g| g.resources.iter())
                        .filter(|r| &r.r#type == written)
                        .find_map(|r| r.params.get("apiVersion").and_then(Value::as_str));
                    match version {
                        Some(version) => catalogue.exact(&format!("{written}@{version}")),
                        None => catalogue
                            .by_base
                            .get_key_value(&written.to_ascii_lowercase())
                            .and_then(|(_, versions)| newest(versions))
                            .map(|v| format!("{}@{v}", catalogue.spelling(written))),
                    }
                }
            };
            used.insert(written.clone(), versioned);
        }
        used
    }

    fn fetch_types(
        &self,
        path: &str,
        work_dir: &std::path::Path,
        log: &dyn Log,
    ) -> Result<(meridian_sdk::Fetched, Value), RunError> {
        if path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
            || path.contains('\\')
        {
            return Err(RunError::new(format!(
                "the index points at {path}, which is not a path under it"
            )));
        }
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let fetched = fetch(
            &format!("{}{path}", self.url),
            &work_dir.join(dir),
            name,
            &format!("the Azure type definitions {path}"),
            log,
        )?;
        let document = decode_value(&fetched.bytes)
            .map_err(|e| RunError::new(format!("{path} could not be read: {e}")))?;
        Ok((fetched, document))
    }
}

/// The index: every `<type>@<version>`, where its definition lives, and
/// the versions of each type.
#[derive(Debug, Default)]
pub(crate) struct Catalogue {
    /// `<type>@<version>` as the index spells it, to the `types.json` path
    /// and the entry number in it.
    resources: BTreeMap<String, (String, usize)>,
    /// Lower-case `<type>` to its versions, sorted.
    by_base: BTreeMap<String, Vec<String>>,
    /// Lower-case `<type>` to the spelling the index uses.
    spellings: BTreeMap<String, String>,
    /// Lower-case `<type>@<version>` to the index's spelling.
    exact: BTreeMap<String, String>,
}

impl Catalogue {
    pub(crate) fn read(index: &Value) -> Self {
        let mut catalogue = Catalogue::default();
        let Some(resources) = index
            .as_map()
            .and_then(|m| m.get("resources"))
            .and_then(Value::as_map)
        else {
            return catalogue;
        };
        for (name, entry) in resources {
            let Some(reference) = entry
                .as_map()
                .and_then(|m| m.get("$ref"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some((path, number)) = reference.rsplit_once("#/") else {
                continue;
            };
            let Ok(number) = number.parse::<usize>() else {
                continue;
            };
            let Some((base, version)) = name.rsplit_once('@') else {
                continue;
            };
            catalogue
                .resources
                .insert(name.clone(), (path.to_owned(), number));
            catalogue
                .by_base
                .entry(base.to_ascii_lowercase())
                .or_default()
                .push(version.to_owned());
            catalogue
                .spellings
                .entry(base.to_ascii_lowercase())
                .or_insert_with(|| base.to_owned());
            catalogue
                .exact
                .insert(name.to_ascii_lowercase(), name.clone());
        }
        for versions in catalogue.by_base.values_mut() {
            versions.sort();
            versions.dedup();
        }
        catalogue
    }

    /// The index's spelling of `<type>@<version>`, matched without regard
    /// to case, as Azure matches resource types.
    fn exact(&self, name: &str) -> Option<String> {
        self.exact.get(&name.to_ascii_lowercase()).cloned()
    }

    fn spelling<'a>(&'a self, base: &'a str) -> &'a str {
        self.spellings
            .get(&base.to_ascii_lowercase())
            .map_or(base, String::as_str)
    }
}

/// The newest API version: the greatest stable one, or the greatest
/// preview when there is no stable one. Versions are dates, so text order
/// is time order.
fn newest(versions: &[String]) -> Option<&String> {
    versions
        .iter()
        .filter(|v| !v.contains("-preview") && !v.contains("-privatepreview"))
        .max()
        .or_else(|| versions.iter().max())
}

fn api_version_param(versions: &[String]) -> ParamSchema {
    let mut param = ParamSchema::new(
        "apiVersion",
        ParamKind::String,
        "The version of the resource type's API the template uses.",
    )
    .required();
    param.values = versions.to_vec();
    param
}

/// One `types.json`: the type graph of a provider at an API version.
struct TypeFile {
    source: String,
    types: Value,
}

impl TypeFile {
    fn entry(&self, number: usize) -> Option<&BTreeMap<String, Value>> {
        self.types.as_list()?.get(number)?.as_map()
    }

    fn follow(&self, reference: &Value) -> Option<usize> {
        reference
            .as_map()?
            .get("$ref")?
            .as_str()?
            .strip_prefix("#/")?
            .parse()
            .ok()
    }

    /// The resource entry `number` as a schema under the name `written`.
    fn resource(
        &self,
        number: usize,
        dialect: Dialect,
        written: &str,
        versioned: &str,
        catalogue: &Catalogue,
    ) -> Option<ResourceSchema> {
        let entry = self.entry(number)?;
        if entry.get("$type").and_then(Value::as_str) != Some("ResourceType") {
            return None;
        }
        let body = self.follow(entry.get("body")?)?;
        let (mut params, open) = self.object(body, 0);
        let (base, version) = versioned.rsplit_once('@')?;
        match dialect {
            Dialect::Bicep => {
                // `type` and `apiVersion` are the type itself; `id` is what
                // Azure assigns.
                params.retain(|p| p.name != "type" && p.name != "apiVersion");
            }
            Dialect::Arm => {
                params.retain(|p| p.name != "type" && p.name != "apiVersion");
                let versions = catalogue
                    .by_base
                    .get(&base.to_ascii_lowercase())
                    .cloned()
                    .unwrap_or_else(|| vec![version.to_owned()]);
                params.push(api_version_param(&versions));
            }
        }
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let description = match dialect {
            Dialect::Bicep => format!("{base} at API version {version}."),
            Dialect::Arm => format!("{base}; the document's apiVersion chooses the API version (described here at {version})."),
        };
        let mut schema = ResourceSchema::closed(written, &description).with_params(params);
        schema.open = open;
        schema.source = self.source.clone();
        Some(schema)
    }

    /// The properties of the object type `number` as params, and whether
    /// it takes other keys.
    fn object(&self, number: usize, depth: usize) -> (Vec<ParamSchema>, bool) {
        let Some(entry) = self.entry(number) else {
            return (Vec::new(), true);
        };
        let kind = entry.get("$type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "ObjectType" => {
                let mut params = self.properties(entry.get("properties"), depth);
                let open = entry.contains_key("additionalProperties");
                params.sort_by(|a, b| a.name.cmp(&b.name));
                (params, open)
            }
            "DiscriminatedObjectType" => {
                // The base properties, then every variant's: an editor that
                // does not know which variant is written accepts any.
                let mut params = self.properties(entry.get("baseProperties"), depth);
                if let Some(elements) = entry.get("elements").and_then(Value::as_map) {
                    for element in elements.values() {
                        let Some(number) = self.follow(element) else {
                            continue;
                        };
                        if depth >= MAX_DEPTH {
                            break;
                        }
                        let (more, _) = self.object(number, depth + 1);
                        for p in more {
                            if !params.iter().any(|q| q.name == p.name) {
                                params.push(p);
                            }
                        }
                    }
                }
                params.sort_by(|a, b| a.name.cmp(&b.name));
                (params, true)
            }
            _ => (Vec::new(), true),
        }
    }

    fn properties(&self, properties: Option<&Value>, depth: usize) -> Vec<ParamSchema> {
        let Some(properties) = properties.and_then(Value::as_map) else {
            return Vec::new();
        };
        properties
            .iter()
            .map(|(name, property)| self.property(name, property, depth))
            .collect()
    }

    fn property(&self, name: &str, property: &Value, depth: usize) -> ParamSchema {
        let map = property.as_map();
        let description = map
            .and_then(|m| m.get("description"))
            .and_then(Value::as_str)
            .map(first_paragraph)
            .unwrap_or_default();
        let flags = match map.and_then(|m| m.get("flags")) {
            Some(Value::Int(flags)) => *flags,
            Some(Value::Uint(flags)) => i64::try_from(*flags).unwrap_or(0),
            _ => 0,
        };
        let mut param = ParamSchema::new(name, ParamKind::Any, &description);
        param.required = flags & REQUIRED != 0;
        param.computed = flags & READ_ONLY != 0;
        param.sensitive = flags & WRITE_ONLY != 0;
        if let Some(number) = map.and_then(|m| m.get("type")).and_then(|t| self.follow(t)) {
            self.describe(number, &mut param, depth + 1);
        }
        param
    }

    /// Fills `param` with what the type entry `number` is.
    fn describe(&self, number: usize, param: &mut ParamSchema, depth: usize) {
        let Some(entry) = self.entry(number) else {
            return;
        };
        let kind = entry.get("$type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "StringType" => param.kind = ParamKind::String,
            "IntegerType" => param.kind = ParamKind::Integer,
            "BooleanType" => param.kind = ParamKind::Bool,
            "NumberType" => param.kind = ParamKind::Number,
            "StringLiteralType" => {
                param.kind = ParamKind::String;
                if let Some(value) = entry.get("value").and_then(Value::as_str) {
                    param.values = vec![value.to_owned()];
                }
            }
            "UnionType" => {
                let elements = entry.get("elements").and_then(Value::as_list);
                let mut values = Vec::new();
                let mut all_literal = true;
                let mut all_text = true;
                for element in elements.into_iter().flatten() {
                    match self
                        .follow(element)
                        .and_then(|n| self.entry(n))
                        .and_then(|e| e.get("$type"))
                        .and_then(Value::as_str)
                    {
                        Some("StringLiteralType") => {
                            let e = self.follow(element).and_then(|n| self.entry(n));
                            if let Some(v) = e.and_then(|e| e.get("value")).and_then(Value::as_str)
                            {
                                values.push(v.to_owned());
                            }
                        }
                        Some("StringType") => all_literal = false,
                        _ => {
                            all_literal = false;
                            all_text = false;
                        }
                    }
                }
                // A union of literals is an enum. Azure's extensible enums
                // are literals beside a plain string ("one of these, or
                // another"), and an editor must not refuse the other, so
                // those are a string with no values listed.
                if all_literal {
                    param.kind = ParamKind::String;
                    param.values = values;
                } else if all_text {
                    param.kind = ParamKind::String;
                } else {
                    param.kind = ParamKind::Any;
                }
            }
            "ArrayType" => {
                param.kind = ParamKind::List;
                if depth < MAX_DEPTH {
                    if let Some(item) = entry.get("itemType").and_then(|t| self.follow(t)) {
                        let (params, open) = self.object(item, depth);
                        param.params = params;
                        param.open = open;
                    }
                }
            }
            "ObjectType" | "DiscriminatedObjectType" => {
                param.kind = ParamKind::Map;
                if depth < MAX_DEPTH {
                    let (params, open) = self.object(number, depth);
                    param.params = params;
                    param.open = open;
                } else {
                    param.open = true;
                }
            }
            _ => param.kind = ParamKind::Any,
        }
    }
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `generated/index.json`, cut down.
    pub(crate) const INDEX: &str = r##"{
  "resources": {
    "Microsoft.Storage/storageAccounts@2023-01-01": {"$ref": "storage/microsoft.storage/2023-01-01/types.json#/3"},
    "Microsoft.Storage/storageAccounts@2024-01-01-preview": {"$ref": "storage/microsoft.storage/2024-01-01-preview/types.json#/3"},
    "Microsoft.Storage/storageAccounts@2022-09-01": {"$ref": "storage/microsoft.storage/2022-09-01/types.json#/3"},
    "Microsoft.Network/virtualNetworks@2024-01-01": {"$ref": "network/microsoft.network/2024-01-01/types.json#/0"}
  },
  "resourceFunctions": {},
  "settings": {"name": "AzureResourceManager", "version": "1.0"}
}"##;

    /// `storage/microsoft.storage/2023-01-01/types.json`, cut down to the
    /// shapes the reader has to handle.
    pub(crate) const STORAGE: &str = r##"[
  {"$type": "StringType", "minLength": 3, "maxLength": 24},
  {"$type": "StringLiteralType", "value": "Microsoft.Storage/storageAccounts"},
  {"$type": "StringLiteralType", "value": "2023-01-01"},
  {"$type": "ResourceType", "name": "Microsoft.Storage/storageAccounts@2023-01-01", "body": {"$ref": "#/4"}, "readableScopes": 8, "writableScopes": 8},
  {"$type": "ObjectType", "name": "Microsoft.Storage/storageAccounts", "properties": {
    "id": {"type": {"$ref": "#/0"}, "flags": 10, "description": "The resource id"},
    "name": {"type": {"$ref": "#/0"}, "flags": 9, "description": "The resource name"},
    "type": {"type": {"$ref": "#/1"}, "flags": 10, "description": "The resource type"},
    "apiVersion": {"type": {"$ref": "#/2"}, "flags": 10, "description": "The resource api version"},
    "sku": {"type": {"$ref": "#/5"}, "flags": 1, "description": "Required. Gets or sets the SKU name."},
    "kind": {"type": {"$ref": "#/8"}, "flags": 1, "description": "Required. Indicates the type of storage account."},
    "location": {"type": {"$ref": "#/0"}, "flags": 1, "description": "Required. Gets or sets the location of the resource."},
    "tags": {"type": {"$ref": "#/11"}, "flags": 0, "description": "Gets or sets a list of key value pairs."},
    "properties": {"type": {"$ref": "#/12"}, "flags": 0, "description": "The parameters used to create the storage account."},
    "secret": {"type": {"$ref": "#/0"}, "flags": 4, "description": "Write-only."},
    "routing": {"type": {"$ref": "#/14"}, "flags": 0, "description": "Routing."}
  }},
  {"$type": "ObjectType", "name": "Sku", "properties": {
    "name": {"type": {"$ref": "#/6"}, "flags": 1, "description": "The SKU name."},
    "tier": {"type": {"$ref": "#/0"}, "flags": 2, "description": "The SKU tier."}
  }},
  {"$type": "UnionType", "elements": [{"$ref": "#/7"}, {"$ref": "#/13"}]},
  {"$type": "StringLiteralType", "value": "Standard_LRS"},
  {"$type": "UnionType", "elements": [{"$ref": "#/9"}, {"$ref": "#/10"}, {"$ref": "#/0"}]},
  {"$type": "StringLiteralType", "value": "Storage"},
  {"$type": "StringLiteralType", "value": "StorageV2"},
  {"$type": "ObjectType", "name": "Tags", "properties": {}, "additionalProperties": {"$ref": "#/0"}},
  {"$type": "ObjectType", "name": "StorageAccountPropertiesCreateParameters", "properties": {
    "accessTier": {"type": {"$ref": "#/8"}, "flags": 0, "description": "Required for storage accounts where kind = BlobStorage."},
    "networkAcls": {"type": {"$ref": "#/15"}, "flags": 0, "description": "Network rule set"},
    "isHnsEnabled": {"type": {"$ref": "#/16"}, "flags": 0, "description": "Account HierarchicalNamespace enabled if sets to true."},
    "provisioningState": {"type": {"$ref": "#/0"}, "flags": 2, "description": "Gets the status of the storage account at the time the operation was called."}
  }},
  {"$type": "StringLiteralType", "value": "Premium_LRS"},
  {"$type": "DiscriminatedObjectType", "name": "Routing", "discriminator": "kind", "baseProperties": {"kind": {"type": {"$ref": "#/0"}, "flags": 1}}, "elements": {"A": {"$ref": "#/17"}}},
  {"$type": "ObjectType", "name": "NetworkRuleSet", "properties": {
    "ipRules": {"type": {"$ref": "#/18"}, "flags": 0, "description": "Sets the IP ACL rules"}
  }},
  {"$type": "BooleanType"},
  {"$type": "ObjectType", "name": "RoutingA", "properties": {"extra": {"type": {"$ref": "#/0"}, "flags": 0}}},
  {"$type": "ArrayType", "itemType": {"$ref": "#/19"}},
  {"$type": "ObjectType", "name": "IPRule", "properties": {
    "value": {"type": {"$ref": "#/0"}, "flags": 1, "description": "Specifies the IP or IP range in CIDR format."},
    "action": {"type": {"$ref": "#/20"}, "flags": 0, "description": "The action of IP ACL rule."}
  }},
  {"$type": "StringLiteralType", "value": "Allow"},
  {"$type": "IntegerType"}
]"##;

    fn file() -> TypeFile {
        TypeFile {
            source: "bicep-types-az storage/microsoft.storage/2023-01-01 sha256:abc".to_owned(),
            types: decode_value(STORAGE.as_bytes()).unwrap(),
        }
    }

    #[test]
    fn the_index_lists_each_types_versions_and_where_they_live() {
        let catalogue = Catalogue::read(&decode_value(INDEX.as_bytes()).unwrap());
        assert_eq!(
            catalogue.by_base["microsoft.storage/storageaccounts"],
            ["2022-09-01", "2023-01-01", "2024-01-01-preview"]
        );
        assert_eq!(
            newest(&catalogue.by_base["microsoft.storage/storageaccounts"]).unwrap(),
            "2023-01-01",
            "a preview is not the newest while a stable version exists"
        );
        assert_eq!(
            newest(&["2020-01-01-preview".to_owned()]).unwrap(),
            "2020-01-01-preview"
        );
        assert_eq!(
            catalogue
                .exact("microsoft.storage/storageaccounts@2023-01-01")
                .as_deref(),
            Some("Microsoft.Storage/storageAccounts@2023-01-01"),
            "matched without regard to case"
        );
        assert_eq!(
            catalogue.resources["Microsoft.Storage/storageAccounts@2023-01-01"],
            (
                "storage/microsoft.storage/2023-01-01/types.json".to_owned(),
                3
            )
        );
        assert_eq!(
            catalogue.spelling("MICROSOFT.network/virtualnetworks"),
            "Microsoft.Network/virtualNetworks"
        );
    }

    #[test]
    fn a_resource_type_becomes_a_bicep_schema() {
        let catalogue = Catalogue::read(&decode_value(INDEX.as_bytes()).unwrap());
        let written = "Microsoft.Storage/storageAccounts@2023-01-01";
        let schema = file()
            .resource(3, Dialect::Bicep, written, written, &catalogue)
            .unwrap();
        assert_eq!(schema.r#type, written);
        assert!(!schema.open);
        assert_eq!(
            schema.source,
            "bicep-types-az storage/microsoft.storage/2023-01-01 sha256:abc"
        );
        let p = |name: &str| schema.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert!(schema.find_param("type").is_none() && schema.find_param("apiVersion").is_none());
        assert!(p("id").computed);
        assert!(p("name").required && !p("name").computed);
        assert_eq!(p("name").kind, ParamKind::String);
        assert!(p("location").required);
        assert!(p("secret").sensitive);
        // A union of literals is an enum.
        let sku = p("sku");
        assert_eq!(sku.kind, ParamKind::Map);
        assert!(!sku.open);
        assert_eq!(
            sku.find_param("name").unwrap().values,
            ["Standard_LRS", "Premium_LRS"]
        );
        assert!(sku.find_param("tier").unwrap().computed);
        // A union with a plain string among the literals is a string that
        // takes any value: an extensible enum.
        assert_eq!(p("kind").kind, ParamKind::String);
        assert!(p("kind").values.is_empty());
        // additionalProperties opens a mapping.
        assert!(p("tags").open && p("tags").kind == ParamKind::Map);
        // Nested objects, arrays of objects, booleans.
        let properties = p("properties");
        assert!(properties.find_param("provisioningState").unwrap().computed);
        assert_eq!(
            properties.find_param("isHnsEnabled").unwrap().kind,
            ParamKind::Bool
        );
        let rules = properties
            .find_param("networkAcls")
            .unwrap()
            .find_param("ipRules")
            .unwrap();
        assert_eq!(rules.kind, ParamKind::List);
        assert!(rules.find_param("value").unwrap().required);
        assert_eq!(rules.find_param("action").unwrap().values, ["Allow"]);
        // A discriminated object takes the base properties and every
        // variant's, open.
        let routing = p("routing");
        assert!(routing.open);
        assert!(routing.find_param("kind").unwrap().required);
        assert!(routing.find_param("extra").is_some());
        // Sorted.
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn an_arm_schema_takes_the_api_version_as_a_param() {
        let catalogue = Catalogue::read(&decode_value(INDEX.as_bytes()).unwrap());
        let schema = file()
            .resource(
                3,
                Dialect::Arm,
                "Microsoft.Storage/storageAccounts",
                "Microsoft.Storage/storageAccounts@2023-01-01",
                &catalogue,
            )
            .unwrap();
        assert_eq!(schema.r#type, "Microsoft.Storage/storageAccounts");
        let version = schema.find_param("apiVersion").unwrap();
        assert!(version.required);
        assert_eq!(
            version.values,
            ["2022-09-01", "2023-01-01", "2024-01-01-preview"]
        );
        assert!(schema.find_param("type").is_none());
        assert!(schema.description.contains("2023-01-01"));
        assert!(
            file()
                .resource(4, Dialect::Arm, "x", "x@1", &catalogue)
                .is_none(),
            "not a resource entry"
        );
    }

    #[test]
    fn the_used_types_are_matched_to_the_index_in_each_dialect() {
        let catalogue = Catalogue::read(&decode_value(INDEX.as_bytes()).unwrap());
        let text = |target: &str, body: &str| {
            meridian_plugin_testkit::graph_from_text(
                "site.yaml",
                &format!("apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: site\nspec:\n  target: {target}\n  resources:\n{body}"),
                target,
            )
        };
        let bicep = AzureSchemas::new(Dialect::Bicep);
        let request = SchemaRequest::new(
            std::path::PathBuf::from("w"),
            vec![text(
                "bicep",
                "    - id: a\n      type: microsoft.storage/storageAccounts@2023-01-01\n      params:\n        name: a\n    - id: b\n      type: Example.Missing/thing@2020-01-01\n      params:\n        name: b\n    - id: d\n      type: deployment\n      params:\n        name: d\n",
            )],
        );
        let used = bicep.used(&request, &catalogue);
        assert_eq!(
            used.get("microsoft.storage/storageAccounts@2023-01-01")
                .unwrap()
                .as_deref(),
            Some("Microsoft.Storage/storageAccounts@2023-01-01")
        );
        assert_eq!(used.get("Example.Missing/thing@2020-01-01"), Some(&None));
        assert!(!used.contains_key("deployment"));

        let arm = AzureSchemas::new(Dialect::Arm);
        let request = SchemaRequest::new(
            std::path::PathBuf::from("w"),
            vec![text(
                "arm",
                "    - id: a\n      type: Microsoft.Storage/storageAccounts\n      params:\n        apiVersion: 2022-09-01\n        name: a\n    - id: n\n      type: Microsoft.Network/virtualNetworks\n      params:\n        name: n\n",
            )],
        );
        let used = arm.used(&request, &catalogue);
        assert_eq!(
            used["Microsoft.Storage/storageAccounts"].as_deref(),
            Some("Microsoft.Storage/storageAccounts@2022-09-01"),
            "the document's apiVersion"
        );
        assert_eq!(
            used["Microsoft.Network/virtualNetworks"].as_deref(),
            Some("Microsoft.Network/virtualNetworks@2024-01-01"),
            "the newest when the document names none"
        );
    }
}
