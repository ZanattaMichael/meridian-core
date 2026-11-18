//! Asking `salt-call` what each state function takes.
//!
//! Salt's `sys` module describes the state functions a minion has:
//! `sys.list_state_functions` names them all, `sys.state_argspec` gives a
//! function's arguments and which of them have defaults, and
//! `sys.state_doc` its docstring, which documents each argument under its
//! name. This half of the plugin runs `salt-call --local` for those,
//! describes the functions the documents use argument by argument, and
//! lists every other function by name, so completion offers it. Only the
//! installed Salt is asked; a custom state module in the file roots is
//! known once it is synced to the minion's extension modules.

use std::collections::BTreeMap;

use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, settings, version_in, Log, ParamKind, ParamSchema, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, TypeName, Value,
};

use crate::runner::settings as runner_settings;

/// Asks `salt-call --local` for the state functions the documents use.
pub struct SaltSchemas {
    /// The `salt-call` program.
    pub program: String,
    /// Arguments given to every call, from the environment.
    pub extra_args: Vec<String>,
}

impl SaltSchemas {
    /// The source as the environment configures it: the `salt-call` and
    /// `args` settings the runner reads.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        SaltSchemas {
            program: settings::text(setting("salt-call")),
            extra_args: settings::args(setting("args")),
        }
    }

    /// One `salt-call --local --out=json` call, answered as the value under
    /// `local`.
    fn call(&self, args: &[&str], log: &dyn Log) -> Result<Value, String> {
        let finished = Tool::new(&self.program)
            .args(["--local", "--out=json", "--log-level=quiet"])
            .args(self.extra_args.iter().cloned())
            .args(args.iter().copied())
            .capture_stdout()
            .run(log)
            .map_err(|e| e.msg)?;
        if !finished.success() {
            return Err(finished.tail().trim().to_owned());
        }
        let answer = decode_value(finished.stdout.as_bytes()).map_err(|e| e.to_string())?;
        answer
            .as_map()
            .and_then(|m| m.get("local"))
            .cloned()
            .ok_or_else(|| "the answer has no `local` entry".to_owned())
    }
}

impl SchemaSource for SaltSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let version = Tool::new(&self.program)
            .arg("--version")
            .capture_stdout()
            .run_ok(&format!("{} --version failed", self.program), log)?;
        let version = version_in(&version.stdout).unwrap_or_default();
        let mut export = SchemaExport::new("salt", &version);
        export.pin("salt", &version);
        let source = format!("salt {version}");

        let listed = self.call(&["sys.list_state_functions"], log).map_err(|e| {
            RunError::new(format!(
                "{} sys.list_state_functions failed: {e}",
                self.program
            ))
        })?;
        let known: Vec<String> = listed
            .as_list()
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let mut described = Vec::new();
        for written in &request.types {
            if !TypeName::read(written).is_native() || !written.contains('.') {
                continue;
            }
            if !known.iter().any(|k| k == written) {
                export.note(format!(
                    "{written} is not a state function the installed Salt has; a custom state \
                     module is known once it is synced to the minion"
                ));
                continue;
            }
            let argspec = match self.call(&["sys.state_argspec", written], log) {
                Ok(v) => v,
                Err(e) => {
                    export.note(format!("sys.state_argspec {written} failed: {e}"));
                    continue;
                }
            };
            let doc = self
                .call(&["sys.state_doc", written], log)
                .unwrap_or(Value::Null);
            match read_function(written, &argspec, &doc) {
                Some(mut schema) => {
                    schema.source = source.clone();
                    described.push(schema);
                }
                None => export.note(format!("sys.state_argspec {written} printed no arguments")),
            }
        }
        let mut named = 0usize;
        for name in &known {
            if described.iter().any(|s| &s.r#type == name) {
                continue;
            }
            named += 1;
            let mut schema = ResourceSchema::open(name, "");
            schema.source = source.clone();
            described.push(schema);
        }
        export.schemas = described;
        if named > 0 {
            export.note(format!(
                "{} listed by name only; a function is described in full once a document uses \
                 it and the schemas are synced again",
                count(named, "other state function")
            ));
        }
        export.tidy();
        Ok(export)
    }
}

/// One function from its argspec (`{"<fn>": {"args": [...], "defaults":
/// [...]}}`) and its docstring (`{"<fn>": "..."}`): an argument without a
/// default is required, except `name`, which the resource's id supplies;
/// the docstring's paragraph under each argument's name is its description.
pub(crate) fn read_function(name: &str, argspec: &Value, doc: &Value) -> Option<ResourceSchema> {
    let spec = argspec.as_map()?.get(name)?.as_map()?;
    let args: Vec<&str> = spec
        .get("args")
        .and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let defaults = spec
        .get("defaults")
        .and_then(Value::as_list)
        .map_or(0, <[Value]>::len);
    let doc_text = doc
        .as_map()
        .and_then(|m| m.get(name))
        .and_then(Value::as_str)
        .unwrap_or("");
    let descriptions = argument_docs(doc_text, &args);
    let mut params: Vec<ParamSchema> = args
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            let has_default = i >= args.len().saturating_sub(defaults);
            let mut param = ParamSchema::new(
                arg,
                ParamKind::Any,
                descriptions.get(*arg).map(String::as_str).unwrap_or(""),
            );
            param.required = !has_default && *arg != "name";
            param
        })
        .collect();
    params.sort_by(|a, b| a.name.cmp(&b.name));
    let description = first_paragraph(doc_text);
    let mut schema = ResourceSchema::closed(name, &description).with_params(params);
    // Every state takes the requisites and the global state arguments.
    schema.open = true;
    schema.actions = vec!["watch".to_owned()];
    Some(schema)
}

/// The first paragraph of a docstring, with the `.. versionadded::` and
/// `CLI Example` furniture left out.
fn first_paragraph(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if trimmed.starts_with("..") {
            continue;
        }
        out.push(trimmed);
    }
    out.join(" ")
}

/// A Salt docstring documents each argument as its name on a line of its
/// own, with the description indented beneath it.
fn argument_docs(text: &str, args: &[&str]) -> BTreeMap<String, String> {
    let mut docs: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let word = trimmed.split_whitespace().next().unwrap_or("");
        if args.contains(&word) && trimmed.len() == word.len() {
            if let Some((name, lines)) = current.take() {
                docs.insert(name, lines.join(" "));
            }
            current = Some((word.to_owned(), Vec::new()));
            continue;
        }
        if let Some((_, lines)) = &mut current {
            if trimmed.is_empty() {
                if !lines.is_empty() {
                    let (name, lines) = current.take().unwrap();
                    docs.insert(name, lines.join(" "));
                }
                continue;
            }
            if trimmed.starts_with("..") {
                continue;
            }
            lines.push(trimmed.to_owned());
        }
    }
    if let Some((name, lines)) = current.take() {
        docs.insert(name, lines.join(" "));
    }
    docs
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

    const ARGSPEC: &str = r##"{"file.managed": {"args": ["name", "source", "source_hash", "user", "group", "mode", "replace"], "defaults": [null, "", null, null, null, true], "kwargs": "kwargs", "varargs": null}}"##;

    const DOC: &str = r##"{"file.managed": "\n    Manage a given file, this function allows for a file to be downloaded from\n    the salt master and potentially run through a templating system.\n\n    name\n        The location of the file to manage, as an absolute path.\n\n    source\n        The source file to download to the minion, this source file can be\n        hosted on either the salt master server (``salt://``)\n\n        .. versionadded:: 2016.3.0\n\n    user\n        The user to own the file, this defaults to the user salt is running as\n        on the minion\n\n    replace\n        If set to ``False`` and the file already exists, the file will not be\n        modified even if changes would otherwise be made.\n\n    CLI Example:\n\n    .. code-block:: bash\n\n        salt '*' state.single file.managed /etc/motd\n"}"##;

    #[test]
    fn a_state_functions_arguments_become_params() {
        let schema = read_function(
            "file.managed",
            &decode_value(ARGSPEC.as_bytes()).unwrap(),
            &decode_value(DOC.as_bytes()).unwrap(),
        )
        .unwrap();
        assert_eq!(schema.r#type, "file.managed");
        assert!(schema.open, "requisites and global arguments are taken");
        assert_eq!(schema.actions, ["watch"]);
        assert!(
            schema
                .description
                .starts_with("Manage a given file, this function allows"),
            "{}",
            schema.description
        );
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "group",
                "mode",
                "name",
                "replace",
                "source",
                "source_hash",
                "user"
            ]
        );
        let p = |n: &str| schema.find_param(n).unwrap();
        assert!(!p("name").required, "the id supplies it");
        assert!(!p("source").required, "it has a default");
        assert!(p("source")
            .description
            .starts_with("The source file to download to the minion"));
        assert!(!p("source").description.contains("versionadded"));
        assert_eq!(
            p("user").description,
            "The user to own the file, this defaults to the user salt is running as on the minion"
        );
        assert!(p("replace").description.starts_with("If set to ``False``"));
        assert_eq!(p("mode").description, "");
        // An argument before the defaults start is required.
        let spec = decode_value(br##"{"pkg.installed": {"args": ["name", "version", "refresh"], "defaults": [null, null]}}"##).unwrap();
        let pkg = read_function("pkg.installed", &spec, &Value::Null).unwrap();
        assert!(
            !pkg.find_param("name").unwrap().required
                && !pkg.find_param("version").unwrap().required
        );
        let spec =
            decode_value(br##"{"x.y": {"args": ["name", "needed", "other"], "defaults": [1]}}"##)
                .unwrap();
        let xy = read_function("x.y", &spec, &Value::Null).unwrap();
        assert!(xy.find_param("needed").unwrap().required);
        assert!(!xy.find_param("other").unwrap().required);
        assert!(read_function("missing.fn", &spec, &Value::Null).is_none());
    }
}
