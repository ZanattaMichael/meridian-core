//! Asking `ansible-doc` what the modules a document names take.
//!
//! The compile needs no schema: a module's arguments pass through as
//! written. An editor does need one, or it knows `ansible.builtin.lineinfile`
//! only as a name that fits `<namespace>.<collection>.<module>`. Ansible
//! documents every module it can find, installed collections included, and
//! `ansible-doc` prints that documentation as JSON. This half of the plugin
//! asks it for each module the documents use, describes those param by
//! param, and lists every other module `ansible-doc` knows by name, so
//! completion offers the modules a document does not use yet; the next
//! sync after a document starts using one describes it in full. The
//! installed collections' versions are read from `ansible-galaxy`, so a
//! cache made from other versions is told apart from this one.

use std::collections::BTreeMap;
use std::fs;

use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, settings, version_in, Log, ParamKind, ParamSchema, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, Value,
};

use crate::runner::settings as runner_settings;

/// The collection every module without a collection of its own belongs to,
/// which is versioned with ansible-core itself.
const BUILTIN: &str = "ansible.builtin";

/// Asks `ansible-doc` for the modules the documents use.
pub struct AnsibleSchemas {
    /// The `ansible-doc` program to run.
    pub doc: String,
    /// The `ansible-galaxy` program to run, for the collections' versions.
    pub galaxy: String,
}

impl AnsibleSchemas {
    /// The source as the environment configures it: `MERIDIAN_ANSIBLE_DOC`
    /// and `MERIDIAN_ANSIBLE_GALAXY` name the programs.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        AnsibleSchemas {
            doc: settings::text(setting("doc")),
            galaxy: settings::text(setting("galaxy")),
        }
    }
}

impl SchemaSource for AnsibleSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        fs::create_dir_all(&request.work_dir).map_err(|e| {
            RunError::new(format!(
                "could not create {}: {e}",
                request.work_dir.display()
            ))
        })?;
        let tool = |program: &str| Tool::new(program).dir(&request.work_dir);
        let version = tool(&self.doc)
            .arg("--version")
            .capture_stdout()
            .run_ok(&format!("{} --version failed", self.doc), log)?;
        let version = version_in(&version.stdout).unwrap_or_default();
        let mut export = SchemaExport::new("ansible", &version);
        // Both programs are checked for by name before anything is asked
        // of them, so a missing one is a plain message and not a note on
        // every module.
        export.pin(BUILTIN, &version);

        // The collections installed, by version. A galaxy that cannot list
        // them is not a failure: the modules are still described, and only
        // their collection's version goes unrecorded.
        let listed = tool(&self.galaxy)
            .args(["collection", "list", "--format", "json"])
            .capture_stdout()
            .run(log);
        let mut collections = BTreeMap::new();
        match listed {
            Ok(finished) if finished.success() => {
                collections = read_collections(&finished.stdout);
            }
            Ok(finished) => export.note(format!(
                "{} collection list failed, so no collection's version is pinned: {}",
                self.galaxy,
                finished.tail().trim()
            )),
            Err(e) => export.note(format!(
                "{} could not be run, so no collection's version is pinned: {}",
                self.galaxy, e.msg
            )),
        }
        for (collection, version) in &collections {
            export.pin(collection, version);
        }
        let core_version = version.clone();
        let source_of = |module: &str| {
            let collection = module.rsplit_once('.').map(|(c, _)| c).unwrap_or(module);
            let version = if collection == BUILTIN {
                Some(&core_version)
            } else {
                collections.get(collection)
            };
            match version {
                Some(version) => format!("{collection} {version}"),
                None => collection.to_owned(),
            }
        };

        // The modules the documents use, each described in full. One run a
        // module, so a module that is not installed is a note and not the
        // end of the sync.
        let mut schemas = Vec::new();
        let mut described = Vec::new();
        for module in request.types.iter().filter(|t| is_module(t)) {
            let shown = tool(&self.doc)
                .args(["-t", "module", "--json", module])
                .capture_stdout()
                .run(log)?;
            if !shown.success() {
                export.note(format!(
                    "module {module} could not be described: {}",
                    shown.tail().trim()
                ));
                continue;
            }
            match read_module(module, &shown.stdout) {
                Some(mut schema) => {
                    schema.source = source_of(module);
                    schemas.push(schema);
                    described.push(module.clone());
                }
                None => export.note(format!(
                    "module {module} is not in what {} printed about it",
                    self.doc
                )),
            }
        }

        // Every other module, by name.
        let listed = tool(&self.doc)
            .args(["-t", "module", "--list", "--json"])
            .capture_stdout()
            .run_ok(&format!("{} --list failed", self.doc), log)?;
        let mut named = 0;
        for (module, description) in read_list(&listed.stdout) {
            if described.contains(&module) {
                continue;
            }
            let mut schema = ResourceSchema::open(&module, &description).actions(&["run"]);
            schema.source = source_of(&module);
            schemas.push(schema);
            named += 1;
        }
        if named > 0 {
            export.note(format!(
                "{named} other modules listed by name only; a module is described in full once a \
                 document uses it and the schemas are synced again"
            ));
        }
        export.schemas = schemas;
        export.tidy();
        Ok(export)
    }
}

/// Whether a type the documents use is a module's full name, as the
/// compiler reads one.
fn is_module(name: &str) -> bool {
    name.split('.').count() == 3 && !name.contains('<')
}

/// Reads `ansible-galaxy collection list --format json`: paths to
/// collections to versions. A collection installed under several paths is
/// pinned at its newest version; which copy Ansible loads depends on its
/// search order, which the listing does not say.
fn read_collections(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let Ok(document) = decode_value(text.as_bytes()) else {
        return out;
    };
    for paths in document.as_map().into_iter().flat_map(|m| m.values()) {
        for (collection, detail) in paths.as_map().into_iter().flatten() {
            let Some(version) = detail
                .as_map()
                .and_then(|d| d.get("version"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            match out.get(collection) {
                Some(known) if version_parts(known) >= version_parts(version) => {}
                _ => {
                    out.insert(collection.clone(), version.to_owned());
                }
            }
        }
    }
    out
}

/// The numbers of a version, for ordering: `9.4.0` after `8.10.0`.
fn version_parts(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// Reads `ansible-doc -t module --list --json`: module names to their
/// short descriptions.
fn read_list(text: &str) -> Vec<(String, String)> {
    let Ok(document) = decode_value(text.as_bytes()) else {
        return Vec::new();
    };
    document
        .as_map()
        .into_iter()
        .flatten()
        .filter(|(name, _)| is_module(name))
        .map(|(name, description)| {
            (
                name.clone(),
                description.as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// Reads `ansible-doc -t module --json <module>`: the module's options as
/// params, with the kinds Ansible declares, the choices, which are
/// required, which are secrets (`no_log`), and the sub-options of a list
/// or a dictionary. `None` when the module is not in the document.
pub(crate) fn read_module(module: &str, text: &str) -> Option<ResourceSchema> {
    let document = decode_value(text.as_bytes()).ok()?;
    let entry = document.as_map()?.get(module)?;
    let doc = entry.as_map()?.get("doc")?.as_map()?;
    let mut description = text_of(doc.get("short_description"));
    let long = text_of(doc.get("description"));
    if !long.is_empty() {
        if !description.is_empty() && !description.ends_with('.') {
            description.push('.');
        }
        if !description.is_empty() {
            description.push(' ');
        }
        description.push_str(&long);
    }
    let mut params = Vec::new();
    if let Some(options) = doc.get("options").and_then(Value::as_map) {
        for (name, option) in options {
            params.push(read_option(name, option));
        }
    }
    params.sort_by(|a, b| a.name.cmp(&b.name));
    Some(
        ResourceSchema::closed(module, &description)
            .with_params(params)
            .actions(&["run"]),
    )
}

/// One option of a module. An alias is not listed: a document writes the
/// option's own name, which is what the compiled play then holds.
fn read_option(name: &str, option: &Value) -> ParamSchema {
    let Some(option) = option.as_map() else {
        return ParamSchema::new(name, ParamKind::Any, "");
    };
    let text = |key: &str| text_of(option.get(key));
    let kind = kind_of(text("type").as_str());
    let mut param = ParamSchema::new(name, kind, &text("description"));
    if matches!(option.get("required"), Some(Value::Bool(true))) {
        param = param.required();
    }
    if matches!(option.get("no_log"), Some(Value::Bool(true))) {
        param.sensitive = true;
    }
    if option.get("deprecated").is_some() {
        param.deprecated = true;
    }
    if let Some(choices) = option.get("choices") {
        param.values = match choices {
            Value::List(items) => items.iter().map(choice).collect(),
            // A newer ansible-doc writes the choices as a mapping of value
            // to its meaning.
            Value::Map(map) => map.keys().cloned().collect(),
            _ => Vec::new(),
        };
        // A choice is written as a string in a document; the canonical form
        // of a boolean or a number is the same text.
        param.values.retain(|v| !v.is_empty());
    }
    if let Some(sub) = option.get("suboptions").and_then(Value::as_map) {
        let mut nested: Vec<ParamSchema> = sub
            .iter()
            .map(|(name, option)| read_option(name, option))
            .collect();
        nested.sort_by(|a, b| a.name.cmp(&b.name));
        param = param.with_params(nested);
    } else if matches!(kind, ParamKind::Map) {
        // A dictionary with no sub-options documented takes any keys.
        param = param.open();
    }
    param
}

/// A choice as a document writes it: text as it is, anything else in its
/// canonical spelling.
fn choice(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.canonical(),
    }
}

/// The kind an option's `type` stands for. Ansible's own words, with
/// `raw`, `json` and `jsonarg` left as anything and the rest as text.
fn kind_of(declared: &str) -> ParamKind {
    match declared {
        "bool" => ParamKind::Bool,
        "int" => ParamKind::Integer,
        "float" => ParamKind::Number,
        "list" => ParamKind::List,
        "dict" => ParamKind::Map,
        "raw" | "json" | "jsonarg" => ParamKind::Any,
        // `str`, `path`, `bytes`, `bits`, `sid`, and the default when the
        // option declares nothing.
        _ => ParamKind::String,
    }
}

/// A description as one piece of text: `ansible-doc` writes one as a list
/// of paragraphs, or as a string.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::List(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `ansible-doc -t module --json ansible.builtin.lineinfile`
    /// prints, cut down to the shapes the reader has to handle.
    const LINEINFILE: &str = r#"{
  "ansible.builtin.lineinfile": {
    "doc": {
      "collection": "ansible.builtin",
      "description": [
        "This module ensures a particular line is in a file, or replace an existing line using a back-referenced regular expression.",
        "This is primarily useful when you want to change a single line in a file only."
      ],
      "module": "lineinfile",
      "options": {
        "backup": {
          "default": false,
          "description": ["Create a backup file including the timestamp information so you can get the original file back if you somehow clobbered it incorrectly."],
          "type": "bool"
        },
        "line": {
          "aliases": ["value"],
          "description": ["The line to insert/replace into the file."],
          "type": "str"
        },
        "mode": {
          "description": ["The permissions the resulting filesystem object should have."],
          "type": "raw"
        },
        "path": {
          "aliases": ["dest", "destfile", "name"],
          "description": ["The file to modify.", "Before Ansible 2.3 this option was only usable as O(dest), O(destfile) and O(name)."],
          "required": true,
          "type": "path",
          "version_added": "2.3"
        },
        "state": {
          "choices": ["absent", "present"],
          "default": "present",
          "description": ["Whether the line should be there or not."],
          "type": "str"
        },
        "unsafe_writes": {
          "default": false,
          "description": ["Influence when to use atomic operation to prevent data corruption or inconsistent reads from the target filesystem object."],
          "type": "bool",
          "version_added": "2.2"
        }
      },
      "short_description": "Manage lines in text files"
    },
    "examples": "- name: Ensure SELinux is set to enforcing mode\n  ansible.builtin.lineinfile:\n    path: /etc/selinux/config\n",
    "metadata": null,
    "return": {}
  }
}"#;

    #[test]
    fn a_modules_options_become_its_params() {
        let schema = read_module("ansible.builtin.lineinfile", LINEINFILE).unwrap();
        assert_eq!(schema.r#type, "ansible.builtin.lineinfile");
        assert!(!schema.open);
        assert_eq!(schema.actions, ["run"]);
        assert!(schema
            .description
            .starts_with("Manage lines in text files. This module"));
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["backup", "line", "mode", "path", "state", "unsafe_writes"]
        );
        let path = schema.find_param("path").unwrap();
        assert!(path.required);
        assert_eq!(path.kind, ParamKind::String);
        assert_eq!(
            path.description,
            "The file to modify. Before Ansible 2.3 this option was only usable as O(dest), \
             O(destfile) and O(name)."
        );
        assert_eq!(schema.find_param("backup").unwrap().kind, ParamKind::Bool);
        assert_eq!(schema.find_param("mode").unwrap().kind, ParamKind::Any);
        assert_eq!(
            schema.find_param("state").unwrap().values,
            ["absent", "present"]
        );
        // The module asked for is the one read, whatever else is printed.
        assert!(read_module("ansible.builtin.copy", LINEINFILE).is_none());
        assert!(read_module("ansible.builtin.lineinfile", "not json").is_none());
    }

    #[test]
    fn sub_options_secrets_and_choice_mappings_are_read() {
        let text = r#"{"community.mysql.mysql_user": {"doc": {"short_description": "Adds or removes a user", "options": {
            "password": {"description": ["Set the user's password."], "type": "str", "no_log": true},
            "priv": {"description": ["Privileges."], "type": "raw"},
            "tls_requires": {"description": ["Set requirement for secure transport."], "type": "dict", "suboptions": {
                "ssl": {"description": ["Require ssl."], "type": "bool"},
                "subject": {"description": ["The subject."], "type": "str"}}},
            "resource_limits": {"description": ["Limit the user."], "type": "dict"},
            "plugin": {"description": ["Auth plugin."], "type": "str", "choices": {"mysql_native_password": "the default", "caching_sha2_password": "newer"}},
            "column_case_sensitive": {"description": ["Keep case."], "type": "bool", "deprecated": {"why": "x", "version": "4.0.0"}},
            "ports": {"description": ["Ports."], "type": "list", "elements": "int"}
        }}}}"#;
        let schema = read_module("community.mysql.mysql_user", text).unwrap();
        assert!(schema.find_param("password").unwrap().sensitive);
        let tls = schema.find_param("tls_requires").unwrap();
        assert_eq!(tls.kind, ParamKind::Map);
        assert!(!tls.open);
        assert_eq!(tls.find_param("ssl").unwrap().kind, ParamKind::Bool);
        assert!(schema.find_param("resource_limits").unwrap().open);
        assert_eq!(
            schema.find_param("plugin").unwrap().values,
            ["caching_sha2_password", "mysql_native_password"]
        );
        assert!(
            schema
                .find_param("column_case_sensitive")
                .unwrap()
                .deprecated
        );
        assert_eq!(schema.find_param("ports").unwrap().kind, ParamKind::List);
    }

    #[test]
    fn the_list_and_the_collections_are_read() {
        let list = r#"{"ansible.builtin.add_host": "Add a host (and alternatively a group) to the ansible-playbook in-memory inventory", "ansible.builtin.apt": "Manages apt-packages", "not.a.module.name.here": "x"}"#;
        assert_eq!(
            read_list(list),
            [
                (
                    "ansible.builtin.add_host".to_owned(),
                    "Add a host (and alternatively a group) to the ansible-playbook in-memory inventory"
                        .to_owned()
                ),
                (
                    "ansible.builtin.apt".to_owned(),
                    "Manages apt-packages".to_owned()
                ),
            ]
        );
        let collections = r#"{"/usr/share/ansible/collections/ansible_collections": {"community.general": {"version": "9.4.0"}, "ansible.posix": {"version": "1.5.4"}}, "/home/me/.ansible/collections/ansible_collections": {"community.general": {"version": "8.0.0"}}}"#;
        let read = read_collections(collections);
        assert_eq!(read.get("community.general").unwrap(), "9.4.0");
        assert_eq!(read.get("ansible.posix").unwrap(), "1.5.4");
        assert!(read_collections("").is_empty());
    }
}
