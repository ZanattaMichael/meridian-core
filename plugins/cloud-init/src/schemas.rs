//! Reading the cloud-config schema cloud-init publishes.
//!
//! cloud-init validates user-data against a JSON Schema it ships
//! (`cloudinit/config/schemas/schema-cloud-config-v1.json`), one definition
//! per module with the keys the module reads. That schema is the catalogue
//! of every cloud-config module a document may write beside the six, so an
//! editor can know `ntp` takes `servers` and `timezone` takes a string.
//! This half of the plugin fetches it from the cloud-init repository, at
//! the tag of the installed `cloud-init` when there is one and from the
//! main branch otherwise, and describes every module in full: there are
//! some sixty, and the whole schema is small.
//!
//! A module whose value is a mapping takes its keys as params; a module
//! whose value is a string or a list takes the one param `value`, as the
//! compiler reads it.

use meridian_sdk::fetch::first_paragraph;
use meridian_sdk::schema::params_from_json_schema;
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, fetch, settings, version_in, Log, ParamKind, ParamSchema, ResourceSchema,
    RunError, SchemaExport, SchemaRequest, SchemaSource, Value,
};

use crate::runner::settings as runner_settings;
use crate::transform::is_shared_key;

/// Where the schema is published, with `{ref}` where the branch or tag
/// goes.
pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/canonical/cloud-init/{ref}/cloudinit/config/schemas/schema-cloud-config-v1.json";

/// The branch read when no installed cloud-init names a version.
const MAIN: &str = "main";

/// Reads the published cloud-config schema.
pub struct CloudInitSchemas {
    /// The `cloud-init` program, asked for its version.
    pub program: String,
    /// The URL of the schema, with `{ref}` where the tag goes. Changed
    /// only by tests.
    pub url: String,
}

impl CloudInitSchemas {
    /// The source as the environment configures it: `MERIDIAN_CLOUD_INIT`
    /// names the program, as for the runner.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        CloudInitSchemas {
            program: settings::text(settings::find(&declared, "program")),
            url: DEFAULT_URL.to_owned(),
        }
    }

    /// The installed cloud-init's version, when it runs.
    fn installed_version(&self, log: &dyn Log) -> Option<String> {
        let finished = Tool::new(&self.program)
            .arg("--version")
            .capture_stdout()
            .run(log)
            .ok()?;
        if !finished.success() {
            return None;
        }
        // `/usr/bin/cloud-init 25.1.1-0ubuntu1`: the version before the
        // packager's suffix.
        version_in(&finished.stdout).map(|v| v.split('-').next().unwrap_or(&v).to_owned())
    }
}

impl SchemaSource for CloudInitSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let installed = self.installed_version(log);
        let reference = installed.clone().unwrap_or_else(|| MAIN.to_owned());
        let what = "the cloud-config schema";
        let fetched = match fetch(
            &self.url.replace("{ref}", &reference),
            &request.work_dir.join(&reference),
            "schema-cloud-config-v1.json",
            what,
            log,
        ) {
            Ok(fetched) => fetched,
            // A version packaged from a branch has no tag of its name.
            Err(e) if reference != MAIN && e.msg.contains("answered 404") => {
                log.err(&format!(
                    "cloud-init {reference} has no tag in the cloud-init repository; reading the schema of {MAIN}"
                ));
                fetch(
                    &self.url.replace("{ref}", MAIN),
                    &request.work_dir.join(MAIN),
                    "schema-cloud-config-v1.json",
                    what,
                    log,
                )?
            }
            Err(e) => return Err(e),
        };
        let mut export = SchemaExport::new("cloud-init", &reference);
        export.pin("cloud-init", &reference);
        export.pin("schema", &fetched.pin());
        if let Some(note) = &fetched.note {
            export.note(note.clone());
        }
        if installed.is_none() {
            export.note(format!(
                "{} is not installed, so the schema of the {MAIN} branch was read; \
                 install cloud-init to read the schema of its version",
                self.program
            ));
        }
        let document = decode_value(&fetched.bytes)
            .map_err(|e| RunError::new(format!("{what} could not be read: {e}")))?;
        let source = format!("cloud-init {reference} {}", fetched.pin());
        export.schemas = read_modules(&document, &source);
        for wanted in &request.types {
            if !is_shared_key(wanted)
                && meridian_sdk::TypeName::read(wanted).is_native()
                && !export.schemas.iter().any(|s| &s.r#type == wanted)
            {
                export.note(format!(
                    "{wanted} is not a module of cloud-init {reference}"
                ));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// Every module of the schema: each key of each `cc_*` definition's
/// properties, as the type a document writes it as. The keys the six
/// shared types write are left out.
pub(crate) fn read_modules(document: &Value, source: &str) -> Vec<ResourceSchema> {
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
    for (name, definition) in defs {
        if !name.starts_with("cc_") {
            continue;
        }
        let Some(properties) = definition
            .as_map()
            .and_then(|d| d.get("properties"))
            .and_then(Value::as_map)
        else {
            continue;
        };
        for (key, property) in properties {
            if is_shared_key(key) || schemas.iter().any(|s: &ResourceSchema| &s.r#type == key) {
                continue;
            }
            let mut schema = read_module(key, property, &resolve);
            schema.source = source.to_owned();
            schemas.push(schema);
        }
    }
    schemas
}

/// One module: its keys as params when its value is a mapping, else the
/// one param `value`.
fn read_module<'a>(
    key: &str,
    property: &'a Value,
    resolve: &dyn Fn(&str) -> Option<&'a Value>,
) -> ResourceSchema {
    let map = property.as_map();
    let description = first_paragraph(
        map.and_then(|m| m.get("description"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let kind = match map.and_then(|m| m.get("type")) {
        Some(Value::String(t)) => kind_named(t),
        // `["null", "object"]`: the first kind that is not null.
        Some(Value::List(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .map(kind_named)
            .unwrap_or(ParamKind::Any),
        _ if map.is_some_and(|m| m.contains_key("properties")) => ParamKind::Map,
        _ if map.is_some_and(|m| m.contains_key("items")) => ParamKind::List,
        _ => ParamKind::Any,
    };
    if kind == ParamKind::Map {
        let (mut params, open) = params_from_json_schema(property, resolve);
        for param in &mut params {
            param.description = first_paragraph(&param.description);
        }
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let mut schema = ResourceSchema::closed(key, &description).with_params(params);
        schema.open = open;
        return schema;
    }
    let mut value = ParamSchema::new(
        "value",
        kind,
        &format!(
            "The module's whole value{}",
            if description.is_empty() {
                ".".to_owned()
            } else {
                format!(": {description}")
            }
        ),
    )
    .required();
    if let Some(values) = map.and_then(|m| m.get("enum")).and_then(Value::as_list) {
        value.values = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    ResourceSchema::closed(key, &description).with_params(vec![value])
}

fn kind_named(name: &str) -> ParamKind {
    match name {
        "string" => ParamKind::String,
        "boolean" => ParamKind::Bool,
        "integer" => ParamKind::Integer,
        "number" => ParamKind::Number,
        "array" => ParamKind::List,
        "object" => ParamKind::Map,
        _ => ParamKind::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `schema-cloud-config-v1.json`, cut down to the shapes the reader has
    /// to handle.
    const SCHEMA: &str = r##"{
  "$schema": "http://json-schema.org/draft-04/schema#",
  "$defs": {
    "base_config": {"type": "object", "properties": {"ssh_pwauth": {"type": "boolean"}}},
    "cc_ntp": {
      "type": "object",
      "properties": {
        "ntp": {
          "type": ["null", "object"],
          "additionalProperties": false,
          "description": "Configure NTP.",
          "properties": {
            "pools": {"type": "array", "items": {"type": "string"}, "description": "List of ntp pools. If both pools and servers are empty, 4 default pool servers will be provided."},
            "servers": {"type": "array", "items": {"type": "string"}},
            "enabled": {"type": "boolean", "default": true},
            "config": {"type": "object", "additionalProperties": false, "properties": {"confpath": {"type": "string"}, "packages": {"type": "array", "items": {"type": "string"}}}},
            "ntp_client": {"type": "string", "default": "auto", "description": "Name of the NTP client to install and configure."}
          }
        }
      }
    },
    "cc_timezone": {"type": "object", "properties": {"timezone": {"type": "string", "description": "The timezone to use as represented in /usr/share/zoneinfo."}}},
    "cc_bootcmd": {"type": "object", "properties": {"bootcmd": {"type": "array", "items": {"oneOf": [{"type": "array", "items": {"type": "string"}}, {"type": "string"}]}, "minItems": 1, "description": "Commands to run early."}}},
    "cc_runcmd": {"type": "object", "properties": {"runcmd": {"type": "array"}}},
    "cc_package_update_upgrade_install": {"type": "object", "properties": {
      "packages": {"type": "array"},
      "package_update": {"type": "boolean", "default": false, "description": "Set ``true`` to update packages. Happens before upgrade or install."},
      "package_reboot_if_required": {"type": "boolean", "default": false}
    }},
    "cc_apt_configure": {"type": "object", "properties": {"apt": {"type": "object", "additionalProperties": false, "properties": {
      "preserve_sources_list": {"type": "boolean", "default": false},
      "sources": {"type": "object", "additionalProperties": false, "patternProperties": {"^.+$": {"$ref": "#/$defs/apt_source"}}}
    }}}},
    "apt_source": {"type": "object", "properties": {"source": {"type": "string"}}},
    "cc_users_groups": {"type": "object", "properties": {"users": {"type": "array"}, "groups": {"type": "object"}, "user": {"type": "object", "description": "The default user."}}}
  },
  "allOf": [{"$ref": "#/$defs/base_config"}, {"$ref": "#/$defs/cc_ntp"}]
}"##;

    #[test]
    fn every_module_is_a_type_and_the_shared_keys_are_not() {
        let schemas = read_modules(
            &decode_value(SCHEMA.as_bytes()).unwrap(),
            "cloud-init 25.1 sha256:abc",
        );
        let names: Vec<&str> = schemas.iter().map(|s| s.r#type.as_str()).collect();
        assert_eq!(
            names,
            [
                "apt",
                "bootcmd",
                "ntp",
                "package_reboot_if_required",
                "package_update",
                "timezone",
                "user"
            ],
            "sorted by definition then key; packages, runcmd, users, groups and ssh_pwauth are not modules a document writes"
        );
        assert!(schemas
            .iter()
            .all(|s| s.source == "cloud-init 25.1 sha256:abc"));
    }

    #[test]
    fn a_mapping_module_takes_its_keys_and_a_scalar_module_takes_value() {
        let schemas = read_modules(&decode_value(SCHEMA.as_bytes()).unwrap(), "s");
        let find = |name: &str| schemas.iter().find(|s| s.r#type == name).unwrap();
        let ntp = find("ntp");
        assert_eq!(ntp.description, "Configure NTP.");
        assert!(!ntp.open);
        let p = |name: &str| ntp.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(p("pools").kind, ParamKind::List);
        assert_eq!(p("pools").description, "List of ntp pools. If both pools and servers are empty, 4 default pool servers will be provided.");
        assert_eq!(p("enabled").kind, ParamKind::Bool);
        assert_eq!(p("config").kind, ParamKind::Map);
        assert!(p("config").find_param("confpath").is_some());
        let names: Vec<&str> = ntp.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["config", "enabled", "ntp_client", "pools", "servers"]
        );

        let timezone = find("timezone");
        assert_eq!(timezone.params.len(), 1);
        let value = &timezone.params[0];
        assert_eq!(value.name, "value");
        assert!(value.required);
        assert_eq!(value.kind, ParamKind::String);
        assert_eq!(
            value.description,
            "The module's whole value: The timezone to use as represented in /usr/share/zoneinfo."
        );
        let bootcmd = find("bootcmd");
        assert_eq!(bootcmd.params[0].kind, ParamKind::List);
        assert_eq!(bootcmd.params[0].name, "value");
        let flag = find("package_update");
        assert_eq!(flag.params[0].kind, ParamKind::Bool);
        assert_eq!(
            flag.description,
            "Set ``true`` to update packages. Happens before upgrade or install."
        );
        // A mapping taking keys by pattern is open, and a $ref into $defs
        // is followed.
        let apt = find("apt");
        let sources = apt.find_param("sources").unwrap();
        assert!(sources.open && sources.kind == ParamKind::Map);
    }
}
