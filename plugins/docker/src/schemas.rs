//! Reading the Compose Specification's JSON Schema.
//!
//! A Compose file's shape is published by the Compose Specification as a
//! JSON Schema (`schema/compose-spec.json`), with one definition for each
//! of the element types a document writes as resources: `service`,
//! `network`, `volume`, `config` and `secret`. This half of the plugin
//! fetches that schema and describes the five in full, so an editor knows
//! every attribute a service takes and its kind, not only the names the
//! compile checks. The `project` resource is Meridian's own and is not in
//! the schema.
//!
//! What is checked at compile time stays as it is: the hand-kept list of
//! attribute names in `check.rs` is the compiler's, and the real-tool test
//! holds it against the schema so the two cannot drift apart.

use meridian_sdk::fetch::first_paragraph;
use meridian_sdk::schema::params_from_json_schema;
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, fetch, settings, version_in, Log, ResourceSchema, RunError, SchemaExport,
    SchemaRequest, SchemaSource, Value,
};

use crate::runner::settings as runner_settings;
use crate::Section;

/// Where the specification's schema is published.
pub const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/compose-spec/compose-spec/master/schema/compose-spec.json";

/// Reads the published Compose schema.
pub struct ComposeSchemas {
    /// The `docker` program, asked for Compose's version.
    pub program: String,
    /// Arguments given to every `docker` call, from the environment.
    pub extra_args: Vec<String>,
    /// The URL of the schema. Changed only by tests.
    pub url: String,
}

impl ComposeSchemas {
    /// The source as the environment configures it, the same settings the
    /// runner reads.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        ComposeSchemas {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
            url: DEFAULT_URL.to_owned(),
        }
    }

    /// The installed Compose's version, when `docker compose` runs.
    fn installed_version(&self, log: &dyn Log) -> Option<String> {
        let finished = Tool::new(&self.program)
            .args(["compose", "version", "--short"])
            .args(self.extra_args.iter().cloned())
            .capture_stdout()
            .run(log)
            .ok()?;
        if !finished.success() {
            return None;
        }
        version_in(&finished.stdout)
    }
}

impl SchemaSource for ComposeSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let fetched = fetch(
            &self.url,
            &request.work_dir,
            "compose-spec.json",
            "the Compose Specification schema",
            log,
        )?;
        let version = self.installed_version(log);
        let mut export = SchemaExport::new("compose", version.as_deref().unwrap_or(""));
        export.pin("compose-spec", &fetched.pin());
        if let Some(version) = &version {
            export.pin("compose", version);
        }
        if let Some(note) = &fetched.note {
            export.note(note.clone());
        }
        let document = decode_value(&fetched.bytes).map_err(|e| {
            RunError::new(format!(
                "the Compose Specification schema could not be read: {e}"
            ))
        })?;
        let source = format!("compose-spec {}", fetched.pin());
        export.schemas = read_sections(&document, &source);
        for section in Section::ALL {
            if !export
                .schemas
                .iter()
                .any(|s| s.r#type == section.type_name())
            {
                export.note(format!(
                    "the schema has no definition of a {}",
                    section.type_name()
                ));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// The five element types, from the definitions of their names.
pub(crate) fn read_sections(document: &Value, source: &str) -> Vec<ResourceSchema> {
    let Some(defs) = document
        .as_map()
        .and_then(|m| m.get("$defs"))
        .and_then(Value::as_map)
    else {
        return Vec::new();
    };
    let resolve =
        |reference: &str| -> Option<&Value> { defs.get(reference.strip_prefix("#/$defs/")?) };
    let mut schemas = Vec::new();
    for section in Section::ALL {
        let name = section.type_name();
        let Some(definition) = defs.get(name) else {
            continue;
        };
        let (mut params, open) = params_from_json_schema(definition, &resolve);
        for param in &mut params {
            param.description = first_paragraph(&param.description);
        }
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let description = first_paragraph(
            definition
                .as_map()
                .and_then(|d| d.get("description"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let mut schema = ResourceSchema::closed(name, &description).with_params(params);
        // Compose takes `x-` extension fields on every element.
        schema.open = open;
        schema.source = source.to_owned();
        schemas.push(schema);
    }
    schemas
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::ParamKind;

    /// `compose-spec.json`, cut down to the shapes the reader has to handle.
    const SCHEMA: &str = r##"{
  "$schema": "https://json-schema.org/draft/2019-09/schema#",
  "type": "object",
  "properties": {"services": {"type": "object", "patternProperties": {"^[a-zA-Z0-9._-]+$": {"$ref": "#/$defs/service"}}}},
  "$defs": {
    "service": {
      "description": "Configuration for a service.",
      "allOf": [{"$ref": "#/$defs/container_spec"}],
      "properties": {
        "deploy": {"$ref": "#/$defs/deployment"},
        "restart": {"type": "string", "description": "Restart policy for the service container. Options include: 'no', 'always', 'on-failure', and 'unless-stopped'."},
        "scale": {"type": ["integer", "string"], "description": "Number of containers to deploy for this service."},
        "depends_on": {"oneOf": [{"$ref": "#/$defs/list_of_strings"}, {"type": "object", "additionalProperties": false, "patternProperties": {"^[a-zA-Z0-9._-]+$": {"type": "object", "properties": {"condition": {"type": "string", "enum": ["service_started", "service_healthy", "service_completed_successfully"]}}}}}]}
      },
      "patternProperties": {"^x-": {}},
      "additionalProperties": false
    },
    "container_spec": {
      "type": "object",
      "properties": {
        "image": {"type": "string", "description": "Specify the image to start the container from."},
        "environment": {"$ref": "#/$defs/list_or_dict"},
        "healthcheck": {"$ref": "#/$defs/healthcheck"}
      }
    },
    "deployment": {"type": ["object", "null"], "properties": {"replicas": {"type": ["integer", "string"]}}, "additionalProperties": false, "patternProperties": {"^x-": {}}},
    "healthcheck": {"type": "object", "properties": {"disable": {"type": "boolean"}, "test": {"oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]}}, "additionalProperties": false},
    "list_or_dict": {"oneOf": [{"type": "object", "patternProperties": {".+": {"type": ["string", "number", "boolean", "null"]}}, "additionalProperties": false}, {"type": "array", "items": {"type": "string"}, "uniqueItems": true}]},
    "list_of_strings": {"type": "array", "items": {"type": "string"}, "uniqueItems": true},
    "network": {"type": ["object", "null"], "description": "Network configuration.", "properties": {"name": {"type": "string"}, "driver": {"type": "string"}, "external": {"type": ["boolean", "object"]}}, "additionalProperties": false, "patternProperties": {"^x-": {}}},
    "volume": {"type": ["object", "null"], "properties": {"name": {"type": "string"}}, "additionalProperties": false},
    "secret": {"type": "object", "properties": {"file": {"type": "string"}}, "additionalProperties": false},
    "config": {"type": "object", "properties": {"content": {"type": "string"}}, "additionalProperties": false}
  }
}"##;

    #[test]
    fn the_five_element_types_are_described_in_full() {
        let schemas = read_sections(
            &decode_value(SCHEMA.as_bytes()).unwrap(),
            "compose-spec sha256:abc",
        );
        let names: Vec<&str> = schemas.iter().map(|s| s.r#type.as_str()).collect();
        assert_eq!(names, ["service", "network", "volume", "config", "secret"]);
        let service = &schemas[0];
        assert_eq!(service.description, "Configuration for a service.");
        assert!(service.open, "x- extension fields are taken");
        assert_eq!(service.source, "compose-spec sha256:abc");
        let p = |name: &str| service.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(p("image").kind, ParamKind::String, "through allOf and $ref");
        assert_eq!(p("restart").kind, ParamKind::String);
        assert_eq!(
            p("scale").kind,
            ParamKind::Integer,
            "the first kind of several"
        );
        assert_eq!(p("deploy").kind, ParamKind::Map);
        assert!(p("deploy").open);
        assert!(p("deploy").find_param("replicas").is_some());
        assert_eq!(p("healthcheck").kind, ParamKind::Map);
        assert!(!p("healthcheck").open);
        assert_eq!(
            p("environment").kind,
            ParamKind::Any,
            "a list or a mapping: either"
        );
        let names: Vec<&str> = service.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "depends_on",
                "deploy",
                "environment",
                "healthcheck",
                "image",
                "restart",
                "scale"
            ]
        );
        let volume = &schemas[2];
        assert!(!volume.open);
        assert_eq!(volume.params.len(), 1);
    }
}
