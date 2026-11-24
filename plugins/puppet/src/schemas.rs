//! Asking `puppet describe` what each resource type takes.
//!
//! Puppet documents every resource type it knows, core and from installed
//! modules, through `puppet describe`: `--list` names them all with a line
//! each, and `puppet describe <type>` prints the type's description and a
//! `Parameters` section, one entry per attribute with its description and,
//! where the type has them, the values it accepts. This half of the plugin
//! runs those, describes the types the documents use attribute by
//! attribute, and lists every other type by name with its one-line
//! description, so completion offers it. OpenVox is the same program under
//! another name, and reads its schemas through this source too.
//!
//! What `puppet describe` does not know: a defined type (`mysql::db`), which
//! lives in a module's manifests and has no description command. A document
//! that uses one gets a note, and the editor knows it by name as before.

use meridian_sdk::tool::Tool;
use meridian_sdk::{
    is_shared_type, settings, version_in, Log, ParamKind, ParamSchema, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, TypeName, NATIVE_PREFIX,
};

use crate::runner::settings as runner_settings;

/// Asks `puppet describe` for the types the documents use.
pub struct PuppetSchemas {
    /// The `puppet` program.
    pub program: String,
    /// Arguments given to every call, from the environment.
    pub extra_args: Vec<String>,
    /// What the export names as the tool: `puppet` or `openvox`.
    pub tool: String,
}

impl PuppetSchemas {
    /// The source as the environment configures it: the `puppet` and `args`
    /// settings the runner reads.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        PuppetSchemas {
            program: settings::text(setting("puppet")),
            extra_args: settings::args(setting("args")),
            tool: "puppet".to_owned(),
        }
    }

    /// A source over another program, for OpenVox.
    pub fn new(program: &str, extra_args: Vec<String>, tool: &str) -> Self {
        PuppetSchemas {
            program: program.to_owned(),
            extra_args,
            tool: tool.to_owned(),
        }
    }

    fn puppet(&self, args: &[&str]) -> Tool {
        Tool::new(&self.program)
            .args(args.iter().copied())
            .args(self.extra_args.iter().cloned())
            .capture_stdout()
    }
}

impl SchemaSource for PuppetSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let version = self
            .puppet(&["--version"])
            .run_ok(&format!("{} --version failed", self.program), log)?;
        let version = version_in(&version.stdout).unwrap_or_default();
        let mut export = SchemaExport::new(&self.tool, &version);
        export.pin(&self.tool, &version);
        let source = format!("{} {version}", self.tool);

        let listed = self
            .puppet(&["describe", "--list"])
            .run_ok(&format!("{} describe --list failed", self.program), log)?;
        let known = read_list(&listed.stdout);

        // The types the documents use, each described in full. A type
        // `describe` does not know is a note: a defined type from a module,
        // or a typo the compile left to Puppet.
        let mut described = Vec::new();
        for written in &request.types {
            let name = TypeName::read(written);
            if !name.is_native() {
                continue;
            }
            let puppet_name = name.tool_name();
            if !known.iter().any(|(n, _)| n == puppet_name) {
                export.note(format!(
                    "{written} is not a resource type {} describe knows; a defined type from a \
                     module has no description to read",
                    self.program
                ));
                continue;
            }
            let shown = self.puppet(&["describe", puppet_name]).run(log)?;
            if !shown.success() {
                export.note(format!(
                    "{} describe {puppet_name} failed: {}",
                    self.program,
                    shown.tail().trim()
                ));
                continue;
            }
            match read_type(puppet_name, &shown.stdout) {
                Some(mut schema) => {
                    schema.r#type = written.clone();
                    schema.source = source.clone();
                    described.push(schema);
                }
                None => export.note(format!(
                    "{} describe {puppet_name} printed nothing this reader understands",
                    self.program
                )),
            }
        }
        let mut named = 0usize;
        for (name, description) in &known {
            let written = written_name(name);
            if described.iter().any(|s| s.r#type == written) {
                continue;
            }
            named += 1;
            let mut schema = ResourceSchema::open(&written, description);
            schema.source = source.clone();
            described.push(schema);
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

/// The name a document writes for Puppet's type `name`: `native/<name>` when
/// a shared type has taken the bare word.
fn written_name(name: &str) -> String {
    if is_shared_type(name) {
        format!("{NATIVE_PREFIX}{name}")
    } else {
        name.to_owned()
    }
}

/// `puppet describe --list`: after a heading line, one `name - description`
/// line per type.
pub(crate) fn read_list(text: &str) -> Vec<(String, String)> {
    let mut types = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((name, description)) = line.split_once(" - ") else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b':')
        {
            continue;
        }
        types.push((
            name.to_owned(),
            description.trim().trim_end_matches("...").trim().to_owned(),
        ));
    }
    types
}

/// `puppet describe <type>`: the name underlined with `=`, the description,
/// then `Parameters` underlined with `-`, with each attribute as
/// `- **name**` followed by its indented description. `Valid values are
/// ...` in a description lists the values. The `Providers` section ends
/// the parameters.
pub(crate) fn read_type(name: &str, text: &str) -> Option<ResourceSchema> {
    let mut lines = text.lines().peekable();
    // The description: everything between the underline and the first
    // section heading.
    let mut description = Vec::new();
    let mut in_description = false;
    let mut found = false;
    while let Some(line) = lines.peek() {
        let trimmed = line.trim();
        if !found {
            if trimmed == name {
                found = true;
                lines.next();
                // The `====` underline.
                if lines.peek().is_some_and(|l| l.trim().starts_with('=')) {
                    lines.next();
                }
                in_description = true;
                continue;
            }
            lines.next();
            continue;
        }
        if in_description {
            if is_heading(trimmed, &mut lines.clone()) {
                break;
            }
            if !trimmed.is_empty() {
                description.push(trimmed.to_owned());
            }
            lines.next();
        }
    }
    if !found {
        return None;
    }
    let description = first_paragraph(&description.join(" "));
    let mut params: Vec<ParamSchema> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    let mut in_parameters = false;
    let finish = |current: &mut Option<(String, Vec<String>)>, params: &mut Vec<ParamSchema>| {
        if let Some((pname, lines)) = current.take() {
            params.push(param_from(&pname, &lines.join(" ")));
        }
    };
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed == "Parameters" {
            in_parameters = true;
            lines.next(); // the `----` underline
            continue;
        }
        if trimmed == "Providers" || trimmed == "Features" || trimmed == "Properties" {
            finish(&mut current, &mut params);
            in_parameters = trimmed == "Properties";
            lines.next();
            continue;
        }
        if !in_parameters {
            continue;
        }
        // `- **name**`, or `- **name** (*namevar*)` for the attribute the
        // title supplies.
        if let Some(pname) = trimmed
            .strip_prefix("- **")
            .and_then(|t| t.split_once("**"))
            .map(|(name, _)| name)
        {
            finish(&mut current, &mut params);
            current = Some((pname.to_owned(), Vec::new()));
            continue;
        }
        if let Some((_, lines)) = &mut current {
            if !trimmed.is_empty() {
                lines.push(trimmed.to_owned());
            }
        }
    }
    finish(&mut current, &mut params);
    params.sort_by(|a, b| a.name.cmp(&b.name));
    params.dedup_by(|a, b| a.name == b.name);
    // A Puppet type takes any attribute its providers add, so a name the
    // description lacks is Puppet's to refuse.
    let mut schema = ResourceSchema::closed(name, &description).with_params(params);
    schema.actions = vec!["refresh".to_owned()];
    Some(schema)
}

fn is_heading(line: &str, lines: &mut std::iter::Peekable<std::str::Lines<'_>>) -> bool {
    if !matches!(line, "Parameters" | "Providers" | "Features" | "Properties") {
        return false;
    }
    lines.next();
    lines.peek().is_some_and(|l| l.trim().starts_with('-'))
}

/// One attribute from its description: the values after `Valid values
/// are`, as Puppet writes them in backticks, and the first sentence or two
/// of the rest.
fn param_from(name: &str, text: &str) -> ParamSchema {
    let mut values = Vec::new();
    let mut description = text.to_owned();
    if let Some(at) = text.find("Valid values are ") {
        let rest = &text[at + "Valid values are ".len()..];
        let end = rest.find(". ").map_or(rest.len(), |e| e + 1);
        let listed = &rest[..end];
        for item in listed.split(',') {
            let item = item.trim().trim_end_matches('.').trim();
            let item = item.trim_start_matches("and ").trim();
            if let Some(value) = item.strip_prefix('`').and_then(|v| v.strip_suffix('`')) {
                values.push(value.to_owned());
            }
        }
        description = format!("{}{}", &text[..at], &rest[end..]).trim().to_owned();
    }
    let mut param = ParamSchema::new(name, ParamKind::Any, &first_paragraph(&description));
    if !values.is_empty() {
        param.kind = ParamKind::String;
        param.values = values;
    }
    if name == "ensure" {
        // Meridian's `state` is written as `ensure`; the attribute is still
        // offered, as Puppet takes it.
        param.kind = ParamKind::String;
    }
    param
}

/// The first sentences of a description, cut at a paragraph break Puppet
/// writes as a blank line, which the line reader turned into nothing; so
/// the first two sentences, at most.
fn first_paragraph(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = text.len();
    let mut sentences = 0;
    for (i, _) in text.match_indices(". ") {
        sentences += 1;
        if sentences == 2 {
            end = i + 1;
            break;
        }
    }
    text[..end].to_owned()
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

    const LIST: &str = "\
These are the types known to puppet:
augeas          - Apply a change or an array of changes to the  ...
cron            - Installs and manages cron jobs
exec            - Executes external commands
file            - Manages files, including their content, ownersh ...
mount           - Manages mounted filesystems, including putting  ...
";

    const CRON: &str = "

cron
====
Installs and manages cron jobs.  Every cron resource created by Puppet
requires a command and at least one periodic attribute (hour, minute,
month, monthday, weekday, or special).  While the name of the cron job is
not part of the actual job, it is used by Puppet to store and retrieve
it.

If you specify a cron job that matches an existing job in every way
except name, then the jobs will be considered equivalent.


Parameters
----------

- **command**
    The command to execute in the cron job.  The environment
    provided to the command varies by local system rules, and it is
    best to always provide a fully qualified command.

- **ensure**
    The basic property that the resource should be in.
    Valid values are `present`, `absent`.

- **hour**
    The hour at which to run the cron job. Cron
    understands cron-style ranges.

- **name** (*namevar*)
    The symbolic name of the cron job.  This name
    is used for human reference only.

- **target**
    The name of the crontab file in which the cron job should be stored.
    Defaults to the user's name.

Providers
---------
    crontab
";

    #[test]
    fn the_list_names_every_type_with_a_line_each() {
        let types = read_list(LIST);
        let names: Vec<&str> = types.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["augeas", "cron", "exec", "file", "mount"]);
        assert_eq!(types[1].1, "Installs and manages cron jobs");
        assert_eq!(types[0].1, "Apply a change or an array of changes to the");
        assert_eq!(written_name("file"), "native/file");
        assert_eq!(written_name("cron"), "native/cron");
        assert_eq!(written_name("augeas"), "augeas");
    }

    #[test]
    fn a_described_type_becomes_a_schema() {
        let schema = read_type("cron", CRON).unwrap();
        assert_eq!(schema.r#type, "cron");
        assert!(
            schema
                .description
                .starts_with("Installs and manages cron jobs."),
            "{}",
            schema.description
        );
        assert!(
            !schema.description.contains("equivalent"),
            "the first sentences only: {}",
            schema.description
        );
        assert_eq!(schema.actions, ["refresh"]);
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["command", "ensure", "hour", "name", "target"]);
        let ensure = schema.find_param("ensure").unwrap();
        assert_eq!(ensure.values, ["present", "absent"]);
        assert_eq!(ensure.kind, ParamKind::String);
        assert_eq!(
            ensure.description,
            "The basic property that the resource should be in."
        );
        let command = schema.find_param("command").unwrap();
        assert_eq!(command.kind, ParamKind::Any);
        assert!(command
            .description
            .starts_with("The command to execute in the cron job."));
        assert!(read_type("mount", CRON).is_none(), "another type's text");
    }
}
