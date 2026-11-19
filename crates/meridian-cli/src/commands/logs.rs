//! `meridian logs` and `meridian bug-report`: reading back the tool output a
//! run kept on this machine, and packing a run into one archive.

use crate::*;

use meridian_orchestrator::logs::{self, Line};
use serde_json::{json, Value as Json};

/// Which stream `meridian logs --stream` keeps.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum StreamFilter {
    /// What the tool wrote to standard output
    Stdout,
    /// What the tool wrote to standard error
    Stderr,
    /// What Meridian added: a failure, or how much was cut
    Meridian,
}

impl StreamFilter {
    fn word(self) -> &'static str {
        match self {
            StreamFilter::Stdout => "stdout",
            StreamFilter::Stderr => "stderr",
            StreamFilter::Meridian => "meridian",
        }
    }
}

/// What `meridian logs` was asked for.
pub(crate) struct LogQuery<'a> {
    pub(crate) run: Option<&'a str>,
    pub(crate) step: Option<&'a str>,
    pub(crate) endpoint: Option<&'a str>,
    pub(crate) stream: Option<StreamFilter>,
    pub(crate) grep: Option<&'a str>,
    pub(crate) tail: Option<usize>,
    pub(crate) all: bool,
}

/// The run asked for, or the last one.
fn find_run(backend: &dyn Backend, run: Option<&str>) -> Result<RunRecord, Failure> {
    match run {
        Some(id) => record::load_run(backend, id)
            .map_err(Failure::Message)?
            .ok_or_else(|| {
                Failure::Message(format!("there is no run {id} in {}", backend.describe()))
            }),
        None => record::last_run(backend, None)
            .map_err(Failure::Message)?
            .ok_or_else(|| {
                Failure::Message(format!(
                    "no run has been recorded in {}",
                    backend.describe()
                ))
            }),
    }
}

/// A step's log file, if this machine kept one. A run that was killed
/// before the step ended left it under the name without the endpoint.
fn step_file(state_dir: &Path, run: &str, step: &record::StepRecord) -> Option<PathBuf> {
    let dir = logs::run_dir(state_dir, run);
    [
        logs::file_name(&step.id, &step.endpoint),
        logs::file_name(&step.id, ""),
    ]
    .into_iter()
    .map(|name| dir.join(name))
    .find(|path| path.is_file())
}

pub(crate) fn show_logs(state: &State, query: &LogQuery<'_>) -> Result<(), Failure> {
    let backend = backend(state)?;
    let run = find_run(backend.as_ref(), query.run)?;
    let grep = query
        .grep
        .map(regex::Regex::new)
        .transpose()
        .map_err(|e| Failure::Usage(format!("--grep: {e}")))?;

    let mut steps: Vec<&record::StepRecord> = run
        .steps
        .iter()
        .filter(|s| query.step.is_none_or(|name| s.id.name == name))
        .filter(|s| query.endpoint.is_none_or(|e| s.endpoint == e))
        .collect();
    if steps.is_empty() && (query.step.is_some() || query.endpoint.is_some()) {
        return Err(Failure::Message(format!(
            "run {} has no step{}{}",
            run.id,
            query.step.map(|s| format!(" {s}")).unwrap_or_default(),
            query
                .endpoint
                .map(|e| format!(" on {e}"))
                .unwrap_or_default()
        )));
    }
    // Asked for nothing in particular: the failed steps, when there are any.
    if !query.all && query.step.is_none() {
        let failed: Vec<&record::StepRecord> = steps
            .iter()
            .copied()
            .filter(|s| s.status == record::StepStatus::Failed)
            .collect();
        if !failed.is_empty() {
            steps = failed;
        }
    }

    let mut shown = Vec::new();
    for step in steps {
        let file = step_file(&state.state_dir, &run.id, step);
        let mut lines = match &file {
            Some(path) => logs::read(path).map_err(Failure::Message)?,
            None => Vec::new(),
        };
        lines.retain(|line| {
            query.stream.is_none_or(|s| line.stream == s.word())
                && grep.as_ref().is_none_or(|re| re.is_match(&line.text))
        });
        if let Some(n) = query.tail {
            let skip = lines.len().saturating_sub(n);
            lines.drain(..skip);
        }
        shown.push((step, file, lines));
    }

    if json_output() {
        let steps: Vec<Json> = shown
            .iter()
            .map(|(step, file, lines)| {
                json!({
                    "name": step.id.name,
                    "kind": step.id.kind.as_str(),
                    "target": step.target,
                    "endpoint": if step.endpoint.is_empty() { Json::Null } else { step.endpoint.clone().into() },
                    "status": step.status.as_str(),
                    "reason": if step.reason.is_empty() { Json::Null } else { step.reason.clone().into() },
                    "file": file.as_ref().map_or(Json::Null, |p| p.display().to_string().into()),
                    "lines": lines.iter().map(line_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        emit(
            true,
            body([("run", run.id.clone().into()), ("steps", steps.into())]),
        );
        return Ok(());
    }
    for (step, file, lines) in &shown {
        let on = if step.endpoint.is_empty() {
            String::new()
        } else {
            format!(" on {}", step.endpoint)
        };
        let reason = if step.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", step.reason)
        };
        outln!(
            "== {} ({}, target {}{on}): {}{reason}",
            step.id.name,
            step.id.kind.as_str(),
            step.target,
            step.status.as_str()
        );
        match file {
            None => outln!("   no tool output was kept on this machine"),
            Some(path) => {
                outln!("   {}", path.display());
                for line in lines {
                    outln!("{} {} {}", line.time, line.stream, line.text);
                }
            }
        }
    }
    Ok(())
}

fn line_json(line: &Line) -> Json {
    json!({ "time": line.time, "stream": line.stream, "text": line.text })
}

/// Writes one archive of a run: its record and events, the tool output this
/// machine kept, and the versions of meridian and of the plugins the run
/// used. Everything in it is as it was kept, masked; nothing is sent.
pub(crate) fn bug_report(
    state: &State,
    run: Option<&str>,
    out: Option<&Path>,
) -> Result<(), Failure> {
    let backend = backend(state)?;
    let record = find_run(backend.as_ref(), run)?;
    let id = record.id.clone();
    let root = format!("meridian-bug-report-{id}");
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    for key in [record::run_key(&id), record::events_key(&id)] {
        if let Some(bytes) = backend.get(&key).map_err(Failure::Message)? {
            let name = key.rsplit('/').next().unwrap_or(&key).to_owned();
            files.push((name, bytes));
        }
    }
    let dir = logs::run_dir(&state.state_dir, &id);
    for name in logs::list(&state.state_dir, &id) {
        let path = dir.join(&name);
        let bytes = std::fs::read(&path)
            .map_err(|e| Failure::Message(format!("reading {}: {e}", path.display())))?;
        files.push((format!("logs/{name}"), bytes));
    }
    files.push(("version.txt".to_owned(), versions().into_bytes()));
    files.push((
        "plugins.json".to_owned(),
        plugins_of(&state.plugins, &record).into_bytes(),
    ));

    let path = out.map_or_else(
        || PathBuf::from(format!("{root}.tar.gz")),
        Path::to_path_buf,
    );
    let archive = pack(&root, &files)
        .map_err(|e| Failure::Message(format!("packing the bug report: {e}")))?;
    write_private(&path, &archive)
        .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;

    let names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    if json_output() {
        emit(
            true,
            body([
                ("run", id.into()),
                ("out", path.display().to_string().into()),
                ("files", names.into()),
                ("bytes", archive.len().into()),
            ]),
        );
    } else {
        outln!(
            "wrote {} ({}, {} bytes) for run {id}; nothing was sent anywhere",
            path.display(),
            count(names.len(), "file"),
            archive.len()
        );
    }
    Ok(())
}

/// The plugins of the targets the run used, as `meridian plugins --output
/// json` lists them. A plugin that cannot be started is said so in the file.
fn plugins_of(plugins: &PluginDirs, record: &RunRecord) -> String {
    let mut targets: Vec<&str> = record.steps.iter().map(|s| s.target.as_str()).collect();
    targets.sort_unstable();
    targets.dedup();
    let listed = match registry(plugins) {
        Err(Failure::Message(why)) | Err(Failure::Usage(why)) => json!({ "error": why }),
        Err(_) => json!({ "error": "the plugins could not be loaded" }),
        Ok(registry) => {
            let mut found = Vec::new();
            for name in &targets {
                match registry.target(name) {
                    Some(target) => {
                        let info = target.info();
                        found.push(output::plugin(name, info, &abilities(info)));
                    }
                    None => found.push(json!({ "name": name, "error": "no plugin found" })),
                }
            }
            let _ = registry.close();
            json!({ "plugins": found })
        }
    };
    serde_json::to_string_pretty(&listed).expect("a JSON value made here renders") + "\n"
}

fn pack(root: &str, files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<u8>> {
    use flate2::{Compression, GzBuilder};
    let gz = GzBuilder::new().write(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (name, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, format!("{root}/{name}"), content.as_slice())?;
    }
    let mut gz = tar.into_inner()?;
    gz.flush()?;
    gz.finish()
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}
