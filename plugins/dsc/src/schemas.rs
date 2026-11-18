//! Asking DSC what each resource's properties are.
//!
//! Both generations describe their resources. A PowerShell module's
//! resources, which DSC v2 runs directly and DSC v3 runs through its
//! PowerShell adapter, are read with `Get-DscResource -Module`, which lists
//! every property with its type, whether it is mandatory and the values an
//! enum property allows; where it lists nothing, as PSDesiredStateConfiguration
//! on Linux does for a class-based module, the module's `[DscResource()]`
//! classes are read through reflection instead. A resource built for DSC v3 alone, such as
//! `Microsoft.Windows/Registry`, is read with `dsc resource schema`, which
//! prints the JSON Schema its manifest declares; `dsc resource list` names
//! every such resource, so completion can offer them.
//!
//! Every resource of a PowerShell module the documents use is described in
//! full, since one call describes the whole module. Of the DSC v3 resources,
//! the ones the documents use are described in full and the rest by name.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::schema::params_from_json_schema;
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, version_in, Log, ParamKind, ParamSchema, ResourceSchema, RunError, SchemaExport,
    SchemaRequest, SchemaSource, Value,
};

use crate::runner::PROGRAM_VARIABLE;

/// Marks the line of PowerShell's output that holds the answer.
const RESULT_MARK: &str = "MERIDIAN-SCHEMAS:";

/// Asks PowerShell and `dsc` for the resources' properties.
pub struct DscSchemas {
    /// The PowerShell program: `pwsh`.
    pub program: String,
    /// The `dsc` command, when DSC v3 is installed.
    pub dsc: String,
}

impl DscSchemas {
    /// The source as the environment configures it, with the same variables
    /// the runner reads. `MERIDIAN_DSC` names the `dsc` command.
    pub fn from_env() -> Self {
        let variable = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        DscSchemas {
            program: variable(PROGRAM_VARIABLE, "pwsh"),
            dsc: variable("MERIDIAN_DSC", "dsc"),
        }
    }

    fn pwsh(&self, script: &str, request: &SchemaRequest) -> Tool {
        Tool::new(&self.program)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .dir(&request.work_dir)
            .capture_stdout()
    }

    fn dsc(&self, request: &SchemaRequest) -> Tool {
        Tool::new(&self.dsc).dir(&request.work_dir).capture_stdout()
    }
}

impl SchemaSource for DscSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        std::fs::create_dir_all(&request.work_dir).map_err(|e| {
            RunError::new(format!(
                "could not create {}: {e}",
                request.work_dir.display()
            ))
        })?;
        let wanted: Vec<&str> = request
            .types
            .iter()
            .map(String::as_str)
            .filter(|t| *t != crate::transform::RUNNER_TYPE && t.contains('/'))
            .collect();

        // DSC v3, when it is installed: the version, every native resource
        // by name, and the schema of each native resource the documents use.
        let mut export = SchemaExport::new("dsc", "");
        let mut native: BTreeMap<String, Native> = BTreeMap::new();
        match self.dsc(request).arg("--version").run(log) {
            Ok(finished) if finished.success() => {
                export.tool_version = version_in(&finished.stdout).unwrap_or_default();
                let listed = self
                    .dsc(request)
                    .args(["resource", "list", "--output-format", "json"])
                    .run_ok("dsc resource list failed", log)?;
                native = read_native_list(&listed.stdout);
            }
            _ => {
                export
                    .note("dsc (DSC v3) is not installed, so only PowerShell resources were read");
            }
        }
        let mut modules: BTreeSet<&str> = BTreeSet::new();
        for t in &wanted {
            if let Some(resource) = native.get(*t) {
                let shown = self
                    .dsc(request)
                    .args([
                        "resource",
                        "schema",
                        "--resource",
                        t,
                        "--output-format",
                        "json",
                    ])
                    .run_ok(&format!("dsc could not show the schema of {t}"), log)?;
                let schema = decode_value(shown.stdout.as_bytes()).map_err(|e| {
                    RunError::new(format!("the schema of {t} could not be read: {e}"))
                })?;
                let (params, open) = params_from_json_schema(&schema, &|_| None);
                let mut full = ResourceSchema::closed(t, &resource.description).with_params(params);
                full.open = open;
                full.source = native_source(resource);
                export.schemas.push(full);
                export.pin(t, &resource.version);
            } else if let Some((module, _)) = t.split_once('/') {
                modules.insert(module);
            }
        }
        for (name, resource) in &native {
            if !wanted.contains(&name.as_str()) {
                let mut named = ResourceSchema::open(name, &resource.description);
                named.source = native_source(resource);
                export.schemas.push(named);
            }
        }

        // PowerShell modules: every resource of each module the documents use.
        if !modules.is_empty() {
            let script = module_script(&modules);
            let finished = self
                .pwsh(&script, request)
                .run_ok("PowerShell could not list the DSC resources", log)?;
            let Some(json) = finished
                .stdout
                .lines()
                .rev()
                .find_map(|line| line.trim().strip_prefix(RESULT_MARK))
            else {
                return Err(
                    finished.into_error("PowerShell finished without reporting the resources")
                );
            };
            let read = read_modules(json.trim()).map_err(|e| {
                RunError::new(format!("what PowerShell reported could not be read: {e}"))
            })?;
            if !read.powershell.is_empty() {
                export.pin("powershell", &read.powershell);
            }
            for (module, version) in &read.versions {
                export.pin(module, version);
            }
            for problem in &read.problems {
                export.note(format!("PowerShell: {problem}"));
            }
            for module in &modules {
                if !read.versions.contains_key(*module) {
                    export.note(format!(
                        "no DSC resources were found in module {module}; is it on PSModulePath?"
                    ));
                }
            }
            export.schemas.extend(read.schemas);
        }
        for t in &wanted {
            if !export.schemas.iter().any(|s| &s.r#type == t) {
                export.note(format!("neither PowerShell nor dsc describes {t}"));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// One resource `dsc resource list` reported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Native {
    version: String,
    description: String,
}

fn native_source(resource: &Native) -> String {
    if resource.version.is_empty() {
        "dsc".to_owned()
    } else {
        format!("dsc {}", resource.version)
    }
}

/// Reads `dsc resource list --output-format json`: one object per line, or
/// one array, depending on the version.
fn read_native_list(text: &str) -> BTreeMap<String, Native> {
    let mut found = BTreeMap::new();
    let entries: Vec<Value> = match decode_value(text.trim().as_bytes()) {
        Ok(Value::List(list)) => list,
        Ok(one @ Value::Map(_)) => vec![one],
        _ => text
            .lines()
            .filter_map(|line| decode_value(line.trim().as_bytes()).ok())
            .collect(),
    };
    for entry in entries {
        let Some(map) = entry.as_map() else {
            continue;
        };
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let name = text("type");
        if name.is_empty() {
            continue;
        }
        found.insert(
            name,
            Native {
                version: text("version"),
                description: text("description"),
            },
        );
    }
    found
}

/// The PowerShell that lists every resource of the modules, as JSON on one
/// marked line.
fn module_script(modules: &BTreeSet<&str>) -> String {
    let list = modules
        .iter()
        .map(|m| quote(m))
        .collect::<Vec<_>>()
        .join(", ");
    MODULE_SCRIPT
        .replace("__MODULES__", &list)
        .replace("__MARK__", RESULT_MARK)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The PowerShell that lists a module's resources. `Get-DscResource -Module`
/// is asked first; where it lists nothing (PSDesiredStateConfiguration on
/// Linux does not list class-based resources) the module is imported and
/// its `[DscResource()]` classes are read through reflection, which gives
/// the same names, types, key and mandatory flags and enum values. Every
/// error PowerShell raised on the way is reported under `Problems`.
const MODULE_SCRIPT: &str = "
    $ErrorActionPreference = 'Continue';
    Import-Module PSDesiredStateConfiguration -ErrorAction SilentlyContinue;
    $problems = [System.Collections.Generic.List[string]]::new();
    $found = [System.Collections.Generic.List[object]]::new();
    foreach ($m in @(__MODULES__)) {
      $listed = @(Get-DscResource -Module $m -ErrorAction SilentlyContinue -ErrorVariable listErrors);
      foreach ($e in $listErrors) { $problems.Add(\"Get-DscResource -Module ${m}: $e\") }
      foreach ($r in $listed) {
        $found.Add([pscustomobject]@{
          Name = [string]$r.Name; Module = [string]$r.ModuleName; Version = [string]$r.Version;
          Properties = @($r.Properties | ForEach-Object {
            [pscustomobject]@{ Name = [string]$_.Name; PropertyType = [string]$_.PropertyType;
              IsMandatory = [bool]$_.IsMandatory; Values = @($_.Values | ForEach-Object { [string]$_ }) } }) })
      }
      if ($listed.Count -gt 0) { continue }
      $info = Import-Module $m -PassThru -ErrorAction SilentlyContinue -ErrorVariable importErrors;
      foreach ($e in $importErrors) { $problems.Add(\"Import-Module ${m}: $e\") }
      if (-not $info -or -not $info.ImplementingAssembly) { continue }
      foreach ($t in $info.ImplementingAssembly.GetTypes()) {
        if (-not ($t.CustomAttributes | Where-Object { $_.AttributeType.Name -eq 'DscResourceAttribute' })) { continue }
        $props = @(foreach ($p in $t.GetProperties()) {
          $a = $p.GetCustomAttributes($true) | Where-Object { $_.GetType().Name -eq 'DscPropertyAttribute' } | Select-Object -First 1;
          if (-not $a) { continue }
          $pt = $p.PropertyType; $values = @(); $typeName = '[' + $pt.Name + ']';
          if ($pt.IsEnum) { $values = @([enum]::GetNames($pt)); $typeName = '[string]' }
          [pscustomobject]@{ Name = $p.Name; PropertyType = $typeName;
            IsMandatory = [bool]($a.Key -or $a.Mandatory); Values = $values } });
        $found.Add([pscustomobject]@{ Name = $t.Name; Module = [string]$info.Name; Version = [string]$info.Version; Properties = $props })
      }
    }
    $answer = [pscustomobject]@{ PowerShell = $PSVersionTable.PSVersion.ToString(); Resources = $found.ToArray(); Problems = $problems.ToArray() };
    '__MARK__' + (ConvertTo-Json -InputObject $answer -Depth 6 -Compress)
";

/// A PowerShell single-quoted string, in which only the quote is special.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// What PowerShell reported about the modules.
#[derive(Debug, Default)]
struct Modules {
    powershell: String,
    /// Each module's version.
    versions: BTreeMap<String, String>,
    schemas: Vec<ResourceSchema>,
    /// The errors PowerShell raised while listing.
    problems: Vec<String>,
}

/// Reads the JSON the module script prints.
fn read_modules(json: &str) -> Result<Modules, String> {
    let answer = decode_value(json.as_bytes()).map_err(|e| e.to_string())?;
    let answer = answer.as_map().ok_or("not an object")?;
    let mut read = Modules {
        powershell: answer
            .get("PowerShell")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        problems: answer
            .get("Problems")
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
        ..Modules::default()
    };
    let resources = match answer.get("Resources") {
        Some(Value::List(list)) => list.clone(),
        Some(one @ Value::Map(_)) => vec![one.clone()],
        _ => Vec::new(),
    };
    for resource in &resources {
        let Some(resource) = resource.as_map() else {
            continue;
        };
        let text = |key: &str| {
            resource
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let (name, module, version) = (text("Name"), text("Module"), text("Version"));
        if name.is_empty() || module.is_empty() {
            continue;
        }
        if !version.is_empty() {
            read.versions
                .entry(module.clone())
                .or_insert(version.clone());
        }
        let mut params: Vec<ParamSchema> = resource
            .get("Properties")
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .iter()
            .filter_map(Value::as_map)
            .filter_map(read_property)
            .collect();
        params.sort_by(|a, b| a.name.cmp(&b.name));
        let mut schema =
            ResourceSchema::closed(&format!("{module}/{name}"), "").with_params(params);
        schema.source = if version.is_empty() {
            module.clone()
        } else {
            format!("{module} {version}")
        };
        read.schemas.push(schema);
    }
    Ok(read)
}

/// One property as `Get-DscResource` reports it.
fn read_property(property: &BTreeMap<String, Value>) -> Option<ParamSchema> {
    let name = property.get("Name").and_then(Value::as_str)?;
    let property_type = property
        .get("PropertyType")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (kind, open) = kind_of(property_type);
    let mut param = ParamSchema::new(name, kind, "");
    param.open = open;
    if matches!(property.get("IsMandatory"), Some(Value::Bool(true))) {
        param = param.required();
    }
    if let Some(values) = property.get("Values").and_then(Value::as_list) {
        param.values = values
            .iter()
            .filter_map(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if !property_type.is_empty() {
        param.description = format!("PowerShell type {property_type}.");
    }
    Some(param)
}

/// The kind a PowerShell property type maps to, and whether a map's keys
/// are open.
fn kind_of(property_type: &str) -> (ParamKind, bool) {
    let mut t = property_type.trim().to_ascii_lowercase();
    // `[string[]]` is written with the outer brackets of every type name.
    if let Some(inner) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        t = inner.to_owned();
    }
    if let Some(inner) = t
        .strip_prefix("nullable[")
        .and_then(|r| r.strip_suffix(']'))
    {
        t = inner.to_owned();
    }
    if t.ends_with("[]") {
        return (ParamKind::List, false);
    }
    let t = t.rsplit('.').next().unwrap_or(&t);
    match t {
        "string" | "char" | "datetime" | "guid" | "uri" | "timespan" | "securestring" => {
            (ParamKind::String, false)
        }
        "bool" | "boolean" => (ParamKind::Bool, false),
        "int" | "int16" | "int32" | "int64" | "uint16" | "uint32" | "uint64" | "byte" | "sbyte"
        | "long" => (ParamKind::Integer, false),
        "double" | "single" | "float" | "decimal" => (ParamKind::Number, false),
        "hashtable" | "pscredential" | "ciminstance" | "ordereddictionary" | "psobject" => {
            (ParamKind::Map, true)
        }
        _ => (ParamKind::Any, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_types_map_to_kinds() {
        assert_eq!(kind_of("[string]"), (ParamKind::String, false));
        assert_eq!(kind_of("[string[]]"), (ParamKind::List, false));
        assert_eq!(kind_of("[bool]"), (ParamKind::Bool, false));
        assert_eq!(kind_of("[Int32]"), (ParamKind::Integer, false));
        assert_eq!(kind_of("[Nullable[UInt32]]"), (ParamKind::Integer, false));
        assert_eq!(kind_of("[double]"), (ParamKind::Number, false));
        assert_eq!(kind_of("[PSCredential]"), (ParamKind::Map, true));
        assert_eq!(kind_of("[hashtable]"), (ParamKind::Map, true));
        assert_eq!(kind_of("[Ensure]"), (ParamKind::Any, false));
        assert_eq!(
            kind_of("[Microsoft.Management.Infrastructure.CimInstance[]]"),
            (ParamKind::List, false)
        );
    }

    #[test]
    fn what_powershell_reports_becomes_schemas() {
        let json = r#"{"PowerShell":"7.4.6","Resources":[
            {"Name":"MarkerFile","Module":"MeridianTestDsc","Version":"1.0.0","Properties":[
                {"Name":"Name","PropertyType":"[string]","IsMandatory":true,"Values":[]},
                {"Name":"Path","PropertyType":"[string]","IsMandatory":true,"Values":[]},
                {"Name":"Content","PropertyType":"[string]","IsMandatory":false,"Values":[]},
                {"Name":"Ensure","PropertyType":"[Ensure]","IsMandatory":false,"Values":["Absent","Present"]},
                {"Name":"Retries","PropertyType":"[Int32]","IsMandatory":false,"Values":[]}]},
            {"Name":"Other","Module":"MeridianTestDsc","Version":"1.0.0","Properties":{"Name":"X","PropertyType":"[string]","IsMandatory":true,"Values":[]}}
        ]}"#;
        let read = read_modules(json).unwrap();
        assert_eq!(read.powershell, "7.4.6");
        assert_eq!(read.versions.get("MeridianTestDsc").unwrap(), "1.0.0");
        let marker = &read.schemas[0];
        assert_eq!(marker.r#type, "MeridianTestDsc/MarkerFile");
        assert_eq!(marker.source, "MeridianTestDsc 1.0.0");
        assert!(!marker.open);
        let p = |name: &str| marker.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert!(p("Name").required && p("Path").required && !p("Content").required);
        assert_eq!(p("Ensure").values, ["Absent", "Present"]);
        assert_eq!(p("Retries").kind, ParamKind::Integer);
        assert_eq!(p("Retries").description, "PowerShell type [Int32].");
        // A single property is a map, not a list, in what ConvertTo-Json prints.
        assert!(read.schemas[1].params.is_empty());
    }

    #[test]
    fn the_native_list_is_read_in_either_shape() {
        let lines = "{\"type\":\"Microsoft/OSInfo\",\"kind\":\"resource\",\"version\":\"0.1.0\",\"description\":\"Returns information about the operating system.\"}\n{\"type\":\"Microsoft.DSC/Group\",\"version\":\"0.1.0\"}\n";
        let found = read_native_list(lines);
        assert_eq!(found.len(), 2);
        assert_eq!(found["Microsoft/OSInfo"].version, "0.1.0");
        assert!(found["Microsoft/OSInfo"].description.starts_with("Returns"));
        let array = "[{\"type\":\"Microsoft/OSInfo\",\"version\":\"0.1.0\"}]";
        assert_eq!(read_native_list(array).len(), 1);
        assert!(read_native_list("").is_empty());
    }

    #[test]
    fn the_module_script_quotes_the_modules() {
        let script = module_script(&BTreeSet::from(["PSDscResources", "It's"]));
        assert!(script.contains("@('It''s', 'PSDscResources')"));
        assert!(script.contains(RESULT_MARK));
        assert!(
            script.contains("DscResourceAttribute"),
            "falls back to the classes"
        );
    }
}
