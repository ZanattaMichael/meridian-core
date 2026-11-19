//! `meridian inspect`: one resource as its tool reads it now, beside what
//! the document declares. It compiles the one step that declares the
//! resource and asks its plugin to read it; it changes nothing, takes no
//! lock and keeps no record.

use crate::*;

use meridian_orchestrator::inspect::{self, Ask, Inspected};
use meridian_orchestrator::Quiet;
use serde_json::{json, Value as Json};

/// What `meridian inspect` was asked for.
pub(crate) struct InspectQuery<'a> {
    pub(crate) resource: &'a str,
    pub(crate) files: &'a [PathBuf],
    pub(crate) step: Option<&'a str>,
    pub(crate) endpoint: Option<&'a str>,
    pub(crate) data: &'a [PathBuf],
    pub(crate) set: &'a [String],
}

pub(crate) fn inspect(state: &State, query: &InspectQuery<'_>) -> Result<(), Failure> {
    let bundle = load(query.files, &state.state_dir)?;
    let mut data = read_data(query.data, &state.state_dir)?;
    apply_sets(&mut data, query.set)?;
    let step = inspect::step_of(&bundle, query.resource, query.step).map_err(Failure::Message)?;
    let mut command = vec!["inspect".to_owned(), query.resource.to_owned()];
    command.extend(query.files.iter().map(|f| f.display().to_string()));
    let options = Options {
        data,
        command: command.join(" "),
        inspect: Some(Ask {
            step: step.clone(),
            resource: query.resource.to_owned(),
            endpoint: query.endpoint.unwrap_or_default().to_owned(),
        }),
        stop: Stop {
            when: Some(interrupt::install()),
            ..Stop::default()
        },
        ..Options::new(&state.state_dir)
    };
    let registry = registry(&state.plugins)?;
    let report = run_selected(
        &bundle,
        &registry,
        Mode::Inspect,
        &options,
        &Selector::all(),
        &mut Quiet,
    );
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    if let Some(refused) = &report.refused {
        return Err(Failure::Message(refused.to_string()));
    }
    let Some(stage) = report.stages.into_iter().next() else {
        return Err(Failure::Message(format!("{step} was not run")));
    };
    for warning in &stage.warnings {
        eprintln!("warning: {}", warning.msg);
    }
    let read = match stage.outcome {
        Outcome::Inspected(read) => read,
        Outcome::Failed(why) => return Err(Failure::Message(why)),
        Outcome::Deferred { waiting_on, output } => {
            return Err(Failure::Message(format!(
                "{step} needs the output {output:?} of {waiting_on}, which has not been \
                 applied, so there is nothing to read yet"
            )))
        }
        Outcome::LeftOut { condition } => {
            return Err(Failure::Message(format!(
                "{step} is left out by its spec.when ({condition}), so there is nothing to read"
            )))
        }
        _ => return Err(Failure::Message(format!("{step} was not inspected"))),
    };
    if json_output() {
        emit(
            true,
            body([
                (
                    "step",
                    json!({ "name": step.name, "kind": step.kind.as_str() }),
                ),
                ("target", Json::from(stage.target)),
                ("inspection", inspection_json(&read)),
            ]),
        );
    } else {
        print_inspection(&step, &stage.target, &read);
    }
    Ok(())
}

fn inspection_json(read: &Inspected) -> Json {
    let map = |data: &Data| -> Json {
        Json::Object(data.iter().map(|(k, v)| (k.clone(), value(v))).collect())
    };
    json!({
        "resource": read.resource,
        "type": read.r#type,
        "endpoint": read.endpoint,
        "found": read.found,
        "inDesiredState": read.in_desired_state,
        "command": read.command,
        "desired": map(&read.desired),
        "actual": map(&read.actual),
        "differs": read.differs,
        "notCompared": read
            .not_compared
            .iter()
            .map(|(name, why)| json!({ "param": name, "why": why.as_str() }))
            .collect::<Vec<_>>(),
    })
}

fn print_inspection(step: &StageId, target: &str, read: &Inspected) {
    let on = if read.endpoint.is_empty() {
        String::new()
    } else {
        format!(" on {}", read.endpoint)
    };
    outln!(
        "{} ({}) in {}, target {target}{on}",
        read.resource,
        read.r#type,
        step.name
    );
    outln!("  read with: {}", read.command);
    let verdict = match (read.found, read.in_desired_state) {
        (false, _) => "not found".to_owned(),
        (true, Some(true)) => "found; the tool says it is as declared".to_owned(),
        (true, Some(false)) => "found; the tool says it differs from the document".to_owned(),
        (true, None) => "found; the tool does not say whether it is as declared".to_owned(),
    };
    outln!("  {verdict}");
    let names: Vec<&String> = read
        .desired
        .keys()
        .chain(
            read.actual
                .keys()
                .filter(|k| !read.desired.contains_key(*k)),
        )
        .collect();
    if names.is_empty() {
        return;
    }
    let width = names
        .iter()
        .map(|n| n.len())
        .chain(["property".len()])
        .max()
        .unwrap_or(0);
    outln!("");
    outln!("  {:width$}  {:24}  actual", "property", "desired");
    for name in names {
        let shown = |data: &Data| data.get(name).map_or("-".to_owned(), |v| v.canonical());
        let mark = if read.differs.contains(name) {
            "  (differs)"
        } else if let Some((_, why)) = read.not_compared.iter().find(|(n, _)| n == name) {
            match why {
                inspect::NotCompared::Sensitive => "  (sensitive, not compared)",
                inspect::NotCompared::NotReported => "  (not reported)",
            }
        } else {
            ""
        };
        outln!(
            "  {name:width$}  {:24}  {}{mark}",
            shown(&read.desired),
            shown(&read.actual)
        );
    }
    if !read.differs.is_empty() {
        outln!("");
        outln!("  differs: {}", read.differs.join(", "));
    }
}
