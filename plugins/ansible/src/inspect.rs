//! `meridian inspect` for Ansible: the playbook cut down to the one task,
//! run in check mode with its result printed. Check mode changes nothing;
//! the task's module reports what it found and whether it would change it.

use meridian_import::yaml::{self, Kind, Node, Rules};
use meridian_sdk::tool::write_artifact;
use meridian_sdk::{decode_value, Artifact, Data, Inspection, RunContext, RunError, Value};
use meridian_yaml::Yaml;

use crate::runner::AnsibleRunner;
use crate::{INVENTORY_PATH, PLAYBOOK_PATH};

/// The playbook an inspection runs, beside the compiled one.
pub const INSPECT_PATH: &str = "inspect.yml";

/// What the module returns that is about the run, not the resource.
const NOT_PROPERTIES: &[&str] = &[
    "changed",
    "diff",
    "invocation",
    "warnings",
    "deprecations",
    "ansible_facts",
    "failed",
    "skipped",
    "skip_reason",
];

pub(crate) fn inspect(
    runner: &AnsibleRunner,
    context: &RunContext,
    artifact: &Artifact,
    resource: &str,
) -> Result<Inspection, RunError> {
    for needed in [PLAYBOOK_PATH, INVENTORY_PATH] {
        if !artifact.files.contains_key(needed) {
            return Err(RunError::new(format!(
                "the artifact has no {needed}; it was not compiled by this plugin"
            )));
        }
    }
    let (one, declared_state) = only_task(&artifact.files[PLAYBOOK_PATH], resource)?;
    write_artifact(&context.work_dir, artifact)?;
    std::fs::write(context.work_dir.join(INSPECT_PATH), one)
        .map_err(|e| RunError::new(format!("writing {INSPECT_PATH}: {e}")))?;
    let (tool, secrets, key) = runner.tool(context, true)?;
    let finished = tool
        .arg("-v")
        // One result a line, as JSON, whatever the operator configured.
        .env("ANSIBLE_CALLBACK_RESULT_FORMAT", "json")
        .env("ANSIBLE_CALLBACK_FORMAT_PRETTY", "0")
        .arg(INSPECT_PATH)
        .capture_stdout()
        .run(&meridian_sdk::NoLog)?;
    drop(secrets);
    drop(key);
    let command =
        format!("ansible-playbook --check --diff -v {PLAYBOOK_PATH}, task {resource} only");
    let read = read_result(&finished.stdout, resource);
    if !finished.success() {
        let why = read
            .and_then(|r| r.failed)
            .unwrap_or_else(|| "ansible-playbook failed".to_owned());
        return Err(
            finished.into_error(format!("ansible-playbook could not read {resource}: {why}"))
        );
    }
    let Some(read) = read else {
        return Err(
            finished.into_error(format!("ansible-playbook printed no result for {resource}"))
        );
    };
    let in_desired_state = read.status.and_then(Status::in_desired_state);
    let mut actual = read.actual;
    // A module that found nothing to change found the state it was given.
    if let (Some(true), Some(state)) = (in_desired_state, declared_state) {
        actual
            .entry("state".to_owned())
            .or_insert(Value::String(state));
    }
    Ok(Inspection {
        found: true,
        actual,
        in_desired_state,
        command,
    })
}

/// The compiled playbook with every play's tasks cut to the one named
/// `resource`, a handler included, and the handlers dropped; and the
/// `state` the task gives its module, if any.
pub fn only_task(playbook: &str, resource: &str) -> Result<(String, Option<String>), RunError> {
    let plays = yaml::read(PLAYBOOK_PATH, playbook, Rules::Yaml11)
        .map_err(|e| RunError::new(format!("reading {PLAYBOOK_PATH}: {e}")))?;
    let plays = plays.first().and_then(Node::as_seq).unwrap_or(&[]);
    let mut kept = Vec::new();
    let mut found = false;
    let mut state = None;
    for play in plays {
        let Some(entries) = play.as_map() else {
            continue;
        };
        let named: Vec<&Node> = ["tasks", "handlers"]
            .iter()
            .flat_map(|key| play.get(key).and_then(Node::as_seq).unwrap_or(&[]))
            .filter(|t| t.get("name").and_then(Node::as_str) == Some(resource))
            .collect();
        // The module is the key after the name.
        state = state.or_else(|| {
            named
                .iter()
                .find_map(|t| t.as_map()?.get(1)?.1.get("state")?.scalar_text())
        });
        let tasks: Vec<Yaml> = named.into_iter().map(yaml_of).collect();
        if tasks.is_empty() {
            continue;
        }
        found = true;
        let mut pairs: Vec<(String, Yaml)> = entries
            .iter()
            .filter(|(k, _)| k != "tasks" && k != "handlers")
            .map(|(k, v)| (k.clone(), yaml_of(v)))
            .collect();
        pairs.push(("tasks".to_owned(), Yaml::Seq(tasks)));
        kept.push(Yaml::Map(pairs));
    }
    if !found {
        return Err(RunError::new(format!(
            "{PLAYBOOK_PATH} has no task {resource:?}"
        )));
    }
    Ok((
        format!("---\n{}", meridian_yaml::write(&Yaml::Seq(kept))),
        state,
    ))
}

fn yaml_of(node: &Node) -> Yaml {
    match &node.kind {
        Kind::Null => Yaml::Null,
        Kind::Bool(b) => Yaml::Bool(*b),
        Kind::Int(i) => Yaml::Int(*i),
        Kind::Uint(u) => Yaml::Uint(*u),
        Kind::Float(f) => Yaml::Float(*f),
        Kind::Str(s) => Yaml::str(s),
        Kind::Seq(items) => Yaml::Seq(items.iter().map(yaml_of).collect()),
        Kind::Map(entries) => Yaml::Map(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), yaml_of(v)))
                .collect(),
        ),
    }
}

/// How the task's result line began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Changed,
    Skipped,
}

impl Status {
    fn in_desired_state(self) -> Option<bool> {
        match self {
            Status::Ok => Some(true),
            Status::Changed => Some(false),
            // A task check mode cannot preview says nothing.
            Status::Skipped => None,
        }
    }
}

/// The result of the task named `resource`: its status and the module's
/// return values, less those about the run itself.
pub fn read_result(stdout: &str, resource: &str) -> Option<ReadResult> {
    let mut in_task = false;
    let mut pending: Option<(Option<Status>, String)> = None;
    let mut read: Option<ReadResult> = None;
    for line in stdout.lines() {
        if let Some((status, mut json)) = pending.take() {
            json.push('\n');
            json.push_str(line);
            match decode_value(json.as_bytes()) {
                Ok(value) => read = Some(ReadResult::of(status, &value)),
                Err(_) => pending = Some((status, json)),
            }
            continue;
        }
        for banner in ["TASK [", "RUNNING HANDLER ["] {
            if let Some(rest) = line.strip_prefix(banner) {
                let name = rest
                    .rfind("] *")
                    .or_else(|| rest.rfind(']'))
                    .map(|end| &rest[..end]);
                in_task = name == Some(resource);
            }
        }
        if !in_task || read.is_some() {
            continue;
        }
        let status = if line.starts_with("ok: [") {
            Some(Status::Ok)
        } else if line.starts_with("changed: [") {
            Some(Status::Changed)
        } else if line.starts_with("skipping: [") {
            Some(Status::Skipped)
        } else if line.starts_with("fatal: [") || line.starts_with("failed: [") {
            None
        } else {
            continue;
        };
        let Some((_, json)) = line.split_once("=> ") else {
            read = Some(ReadResult::of(status, &Value::Null));
            continue;
        };
        match decode_value(json.trim().as_bytes()) {
            Ok(value) => read = Some(ReadResult::of(status, &value)),
            Err(_) => pending = Some((status, json.trim().to_owned())),
        }
    }
    read
}

/// One task's result.
#[derive(Debug, Default, PartialEq)]
pub struct ReadResult {
    /// `None` when the task failed.
    pub status: Option<Status>,
    pub actual: Data,
    pub failed: Option<String>,
}

impl ReadResult {
    fn of(status: Option<Status>, value: &Value) -> ReadResult {
        let map = value.as_map().cloned().unwrap_or_default();
        let failed = match status {
            Some(_) => None,
            None => Some(
                map.get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("the task failed")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
            ),
        };
        let actual = map
            .into_iter()
            .filter(|(k, v)| {
                !k.starts_with('_') && !NOT_PROPERTIES.contains(&k.as_str()) && *v != Value::Null
            })
            .collect();
        ReadResult {
            status,
            actual,
            failed,
        }
    }
}
