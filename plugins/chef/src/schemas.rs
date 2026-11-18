//! Asking the resource inspector what each Chef resource takes.
//!
//! Chef ships `chef-resource-inspector` (`cinc-resource-inspector` with
//! Cinc), which prints every resource the client knows as JSON: its
//! description, its actions, and each property with its type, whether it
//! is required, and the values it is equal to. This half of the plugin runs
//! it once, describes the resources the documents use property by
//! property, and lists every other resource by name with its description,
//! so completion offers it.
//!
//! A custom resource from a cookbook is not in the inspector's output, and
//! a document that uses one gets a note; the editor knows it by name as
//! before.

use std::collections::BTreeMap;

use meridian_sdk::fetch::first_paragraph;
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, is_shared_type, settings, version_in, Log, ParamKind, ParamSchema,
    ResourceSchema, RunError, SchemaExport, SchemaRequest, SchemaSource, TypeName, Value,
    NATIVE_PREFIX,
};

use crate::runner::settings as runner_settings;

/// The inspectors tried, in order, when the configured one does not run:
/// Cinc installs the same program under its own name, and both omnibus
/// packages keep the Ruby gem's own binstub in their embedded `bin`,
/// which is not on `PATH`.
const FALLBACK_INSPECTORS: &[&str] = &[
    "cinc-resource-inspector",
    "/opt/cinc/bin/cinc-resource-inspector",
    "/opt/cinc/bin/chef-resource-inspector",
    "/opt/cinc/embedded/bin/chef-resource-inspector",
    "/opt/chef/bin/chef-resource-inspector",
    "/opt/chef/embedded/bin/chef-resource-inspector",
];

/// Asks the resource inspector for the resources the documents use.
pub struct ChefSchemas {
    /// The inspector program.
    pub inspector: String,
    /// The client, asked for its version.
    pub client: String,
}

impl ChefSchemas {
    /// The source as the environment configures it: the `inspector` and
    /// `client` settings the runner declares.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        ChefSchemas {
            inspector: settings::text(setting("inspector")),
            client: settings::text(setting("client")),
        }
    }
}

impl SchemaSource for ChefSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let version = Tool::new(&self.client)
            .arg("--version")
            .capture_stdout()
            .run_ok(&format!("{} --version failed", self.client), log)?;
        let version = version_in(&version.stdout).unwrap_or_default();
        let mut export = SchemaExport::new("chef", &version);
        export.pin("chef", &version);
        let source = format!("chef {version}");

        let mut shown = Tool::new(&self.inspector).capture_stdout().run(log);
        for fallback in FALLBACK_INSPECTORS {
            let runs = shown.as_ref().is_ok_and(|f| f.success());
            if runs || *fallback == self.inspector {
                continue;
            }
            if !fallback.starts_with('/') || std::path::Path::new(fallback).is_file() {
                log.err(&format!(
                    "{} does not run; trying {fallback}",
                    self.inspector
                ));
                shown = Tool::new(*fallback).capture_stdout().run(log);
            }
        }
        let shown = match shown {
            Ok(finished) if finished.success() => finished,
            Ok(finished) => {
                return Err(finished.into_error(format!(
                    "{} failed; it comes with the Chef or Cinc client",
                    self.inspector
                )))
            }
            Err(e) => {
                return Err(RunError::new(format!(
                    "{} could not be run ({}); it comes with the Chef or Cinc client",
                    self.inspector, e.msg
                )))
            }
        };
        let document = decode_value(shown.stdout.as_bytes()).map_err(|e| {
            RunError::new(format!(
                "{}'s output could not be read: {e}",
                self.inspector
            ))
        })?;
        let wanted: Vec<&str> = request
            .types
            .iter()
            .map(String::as_str)
            .filter(|t| TypeName::read(t).is_native())
            .collect();
        let read = read_resources(&document, &wanted, &source);
        export.schemas = read.schemas;
        if read.named > 0 {
            export.note(format!(
                "{} listed by name only; a resource is described in full once a document uses \
                 it and the schemas are synced again",
                count(read.named, "other resource")
            ));
        }
        for written in wanted {
            if !export.schemas.iter().any(|s| s.r#type == written) {
                export.note(format!(
                    "{written} is not a resource {} knows; a custom resource from a cookbook has \
                     no description to read",
                    self.inspector
                ));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// The name a document writes for Chef's resource `name`: `native/<name>`
/// when a shared type has taken the bare word.
fn written_name(name: &str) -> String {
    if is_shared_type(name) {
        format!("{NATIVE_PREFIX}{name}")
    } else {
        name.to_owned()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Read {
    pub(crate) schemas: Vec<ResourceSchema>,
    pub(crate) named: usize,
}

/// The inspector's output: one entry per resource, keyed by name.
pub(crate) fn read_resources(document: &Value, wanted: &[&str], source: &str) -> Read {
    let mut read = Read::default();
    let Some(resources) = document.as_map() else {
        return read;
    };
    for (name, resource) in resources {
        let Some(resource) = resource.as_map() else {
            continue;
        };
        let written = written_name(name);
        let description = first_paragraph(text(resource, "description"));
        let mut schema = if wanted.contains(&written.as_str()) {
            let mut params: Vec<ParamSchema> = resource
                .get("properties")
                .and_then(Value::as_list)
                .map(|l| l.iter().filter_map(property).collect())
                .unwrap_or_default();
            params.sort_by(|a, b| a.name.cmp(&b.name));
            params.dedup_by(|a, b| a.name == b.name);
            ResourceSchema::closed(&written, &description).with_params(params)
        } else {
            read.named += 1;
            ResourceSchema::open(&written, &description)
        };
        // The actions are a map of name to description (Chef 18), or a
        // list of names in older output.
        schema.actions = match resource.get("actions") {
            Some(Value::Map(m)) => m.keys().cloned().collect(),
            Some(Value::List(l)) => l
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        schema.actions.retain(|a| a != "nothing");
        schema.source = source.to_owned();
        read.schemas.push(schema);
    }
    read
}

fn text<'a>(map: &'a BTreeMap<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or("")
}

/// One property: its Ruby types become a kind, `required` is a flag or a
/// list of the actions it is required for, `equal_to` lists the values.
fn property(value: &Value) -> Option<ParamSchema> {
    let map = value.as_map()?;
    let name = text(map, "name");
    if name.is_empty() {
        return None;
    }
    let types: Vec<String> = match map.get("is") {
        Some(Value::String(s)) => s.split(',').map(|t| t.trim().to_owned()).collect(),
        Some(Value::List(l)) => l
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let kind = match types.first().map(String::as_str) {
        Some("String") => ParamKind::String,
        Some("Integer") => ParamKind::Integer,
        Some("Float" | "Numeric") => ParamKind::Number,
        Some("TrueClass" | "FalseClass" | "true" | "false") => ParamKind::Bool,
        Some("Array") => ParamKind::List,
        Some("Hash") => ParamKind::Map,
        _ => ParamKind::Any,
    };
    // `[true, false]` is written as two types.
    let kind = if types
        .iter()
        .all(|t| matches!(t.as_str(), "true" | "false" | "TrueClass" | "FalseClass"))
        && !types.is_empty()
    {
        ParamKind::Bool
    } else if types.len() > 1 {
        ParamKind::Any
    } else {
        kind
    };
    let mut param = ParamSchema::new(name, kind, &first_paragraph(text(map, "description")));
    // `name` is the resource's id, which the recipe always writes.
    param.required = match map.get("required") {
        _ if name == "name" => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::List(l)) => !l.is_empty(),
        _ => false,
    };
    if let Some(values) = map.get("equal_to").and_then(Value::as_list) {
        param.values = values
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Bool(b) => Some(b.to_string()),
                Value::Int(i) => Some(i.to_string()),
                _ => None,
            })
            .collect();
        if !param.values.is_empty() && param.kind == ParamKind::Any {
            param.kind = ParamKind::String;
        }
    }
    if matches!(map.get("deprecated"), Some(Value::Bool(true)))
        || map
            .get("deprecated")
            .is_some_and(|d| d.as_str().is_some_and(|s| !s.is_empty()))
    {
        param.deprecated = true;
    }
    if matches!(map.get("sensitive"), Some(Value::Bool(true))) {
        param.sensitive = true;
    }
    Some(param)
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `chef-resource-inspector` prints, cut down to the shapes the
    /// reader has to handle.
    const INSPECTED: &str = r##"{
  "apt_repository": {
    "description": "Use the **apt_repository** resource to specify additional APT repositories.\n\nMore text.",
    "default_action": ["add"],
    "actions": {"nothing": null, "add": "Creates a repository file.", "remove": "Removes the repository listing."},
    "introduced": "12.9",
    "properties": [
      {"name": "name", "description": null, "is": "String", "required": true},
      {"name": "repo_name", "description": "An optional property to set the repository name if it differs from the resource block's name.", "introduced": "14.1", "is": "String", "name_property": true, "required": false},
      {"name": "uri", "description": "The base of the Debian distribution.", "is": "String", "required": false},
      {"name": "components", "description": "Package groupings, such as 'main' and 'stable'.", "is": "Array", "default": "[]", "required": false},
      {"name": "trusted", "description": "Determines whether you should treat all packages from this repository as authenticated.", "is": ["true", "false"], "default": "false", "required": false},
      {"name": "keyserver", "is": "String, false", "default": "keyserver.ubuntu.com", "required": false},
      {"name": "arch", "is": "String, false", "equal_to": ["amd64", "i386"], "required": ["add"]}
    ]
  },
  "package": {
    "description": "Use the **package** resource to manage packages.",
    "default_action": ["install"],
    "actions": ["nothing", "install", "remove"],
    "properties": [
      {"name": "package_name", "is": ["String", "Array"], "name_property": true, "required": false},
      {"name": "version", "is": "String, Array", "required": false, "description": "The version of a package to be installed or upgraded."},
      {"name": "timeout", "is": "String, Integer", "required": false}
    ]
  }
}"##;

    #[test]
    fn used_resources_are_described_and_the_rest_listed_by_name() {
        let document = decode_value(INSPECTED.as_bytes()).unwrap();
        let read = read_resources(&document, &["apt_repository"], "chef 18.4");
        assert_eq!(read.named, 1);
        let repo = read
            .schemas
            .iter()
            .find(|s| s.r#type == "apt_repository")
            .unwrap();
        assert!(!repo.open);
        assert_eq!(
            repo.description,
            "Use the **apt_repository** resource to specify additional APT repositories."
        );
        assert_eq!(
            repo.actions,
            ["add", "remove"],
            "nothing is not an action a notify names"
        );
        assert_eq!(repo.source, "chef 18.4");
        let p = |name: &str| repo.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert!(!p("name").required, "the id supplies it");
        assert_eq!(p("uri").kind, ParamKind::String);
        assert_eq!(p("components").kind, ParamKind::List);
        assert_eq!(p("trusted").kind, ParamKind::Bool);
        assert_eq!(p("keyserver").kind, ParamKind::Any, "a string or false");
        assert_eq!(p("arch").values, ["amd64", "i386"]);
        assert_eq!(p("arch").kind, ParamKind::String);
        assert!(p("arch").required, "required for an action is required");
        assert!(!p("uri").required);
        let names: Vec<&str> = repo.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "arch",
                "components",
                "keyserver",
                "name",
                "repo_name",
                "trusted",
                "uri"
            ]
        );
        // Chef's own package is written native/package.
        let package = read
            .schemas
            .iter()
            .find(|s| s.r#type == "native/package")
            .unwrap();
        assert!(package.open && package.params.is_empty());
        assert_eq!(
            package.description,
            "Use the **package** resource to manage packages."
        );
        assert_eq!(package.actions, ["install", "remove"]);
    }
}
