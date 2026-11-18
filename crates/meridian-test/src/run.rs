//! Running the cases of a test document.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use meridian_ir::Data;
use meridian_orchestrator::{run_selected, LocalBackend, Mode, Options, Quiet, Selector};
use meridian_plugin_host::Registry;
use meridian_value::Value;

use crate::assert::{check, outcome_word};
use crate::document::{Case, Run, TestFile};

/// Turns paths written `@module:path` into the files they name.
pub type Resolver<'a> = &'a dyn Fn(&[PathBuf]) -> Result<Vec<PathBuf>, String>;

/// What a test needs from the command that runs it.
pub struct Runner<'a> {
    /// The plugins, already started.
    pub registry: &'a Registry,
    /// The state directory. Each case runs under a scratch directory of its
    /// own beneath `tests/` in it, emptied first, so no case sees another's
    /// state and no test touches the project's own.
    pub state_dir: PathBuf,
    /// Turns a path written `@module:path` into the file it names. Every
    /// other path is given back as it is.
    pub resolve: Resolver<'a>,
}

/// How a case ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Every expectation held and every assertion was true.
    Passed,
    /// At least one did not.
    Failed,
    /// The case could not be run: a document did not load, a data file did
    /// not read, the run was refused.
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Error => "error",
        }
    }
}

/// What one document came to in a case, for the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSummary {
    pub name: String,
    pub target: String,
    pub outcome: String,
}

/// One case's result.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseResult {
    pub name: String,
    pub run: Run,
    pub status: Status,
    /// Why it failed or could not run, one line each; empty when it passed.
    pub failures: Vec<String>,
    pub stages: Vec<StageSummary>,
    pub millis: u128,
}

/// One test document's results.
#[derive(Clone, Debug, PartialEq)]
pub struct FileResult {
    pub path: PathBuf,
    pub name: String,
    pub cases: Vec<CaseResult>,
}

impl FileResult {
    pub fn passed(&self) -> bool {
        self.cases.iter().all(|c| c.status == Status::Passed)
    }
}

/// Runs every case of the document whose name holds `filter`, or every
/// case. A case that fails does not stop the next: all of them are
/// reported.
pub fn run_file(file: &TestFile, runner: &Runner<'_>, filter: Option<&str>) -> FileResult {
    let mut cases = Vec::new();
    for (index, case) in file.cases.iter().enumerate() {
        if filter.is_some_and(|text| !case.name.contains(text)) {
            continue;
        }
        let started = Instant::now();
        let (status, failures, stages) = match run_case(file, case, index, runner) {
            Ok((failures, stages)) if failures.is_empty() => (Status::Passed, failures, stages),
            Ok((failures, stages)) => (Status::Failed, failures, stages),
            Err(why) => (Status::Error, vec![why], Vec::new()),
        };
        cases.push(CaseResult {
            name: case.name.clone(),
            run: case.run,
            status,
            failures,
            stages,
            millis: started.elapsed().as_millis(),
        });
    }
    FileResult {
        path: file.path.clone(),
        name: file.name.clone(),
        cases,
    }
}

type Ran = (Vec<String>, Vec<StageSummary>);

fn run_case(
    file: &TestFile,
    case: &Case,
    index: usize,
    runner: &Runner<'_>,
) -> Result<Ran, String> {
    let documents = (runner.resolve)(&file.documents)?;
    let bundle =
        meridian_ir::load(&documents).map_err(|e| format!("the documents do not load: {e}"))?;
    bundle
        .validate()
        .map_err(|e| format!("the documents are not valid: {e}"))?;
    let mut data = Data::new();
    let mut data_files = file.data.clone();
    data_files.extend(case.data.iter().cloned());
    for path in (runner.resolve)(&data_files)? {
        let shown = path.display().to_string();
        let raw =
            std::fs::read(&path).map_err(|e| format!("reading the data file {shown}: {e}"))?;
        match meridian_ir::parse_value(&shown, &raw) {
            Ok(Value::Map(map)) => data.extend(map),
            Ok(Value::Null) => {}
            Ok(other) => {
                return Err(format!(
                    "the data file {shown} must hold a mapping of names to values, and it holds {}",
                    other.describe()
                ))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    merge_into(&mut data, &file.set);
    merge_into(&mut data, &case.set);
    let selector = match case.select.as_deref().or(file.select.as_deref()) {
        Some(text) => Selector::parse(text).map_err(|e| format!("select: {e}"))?,
        None => Selector::all(),
    };
    // A directory of the case's own, emptied first: a plan is always against
    // nothing applied, so what a tool would do is the same on every run.
    let scratch = runner
        .state_dir
        .join("tests")
        .join(slug(&file.name))
        .join(format!("{}-{}", index + 1, slug(&case.name)));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| {
        format!(
            "cannot create the scratch directory {}: {e}",
            scratch.display()
        )
    })?;
    let mode = match case.run {
        Run::Compile => Mode::Compile,
        Run::Plan => Mode::Plan,
    };
    let options = Options {
        data: data.clone(),
        backend: match mode {
            Mode::Compile => None,
            _ => Some(Arc::new(LocalBackend::new(&scratch)) as Arc<_>),
        },
        command: format!("test {} ({})", file.name, case.name),
        keep_runs: 1,
        ..Options::new(&scratch)
    };
    let report = run_selected(
        &bundle,
        runner.registry,
        mode,
        &options,
        &selector,
        &mut Quiet,
    );
    if let Some(refusal) = &report.refused {
        return Err(format!("the run was refused: {refusal}"));
    }
    let stages = report
        .stages
        .iter()
        .map(|s| StageSummary {
            name: s.id.name.clone(),
            target: s.target.clone(),
            outcome: outcome_word(s).to_owned(),
        })
        .collect();
    Ok((check(case, &report.stages, &data), stages))
}

/// Merges `set` over `data`: a mapping inside a mapping is merged key by
/// key, anything else replaces what was there, as `--set a.b=1` does.
fn merge_into(data: &mut BTreeMap<String, Value>, set: &BTreeMap<String, Value>) {
    for (key, value) in set {
        match (data.get_mut(key), value) {
            (Some(Value::Map(inner)), Value::Map(over)) => merge_into(inner, over),
            _ => {
                data.insert(key.clone(), value.clone());
            }
        }
    }
}

/// A name as a directory name: letters, digits and `-`, nothing else.
fn slug(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        "case".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn set_merges_mappings_and_replaces_the_rest() {
        let mut data: Data = [(
            "a".to_owned(),
            Value::Map([("x".to_owned(), Value::Int(1))].into_iter().collect()),
        )]
        .into_iter()
        .collect();
        let set: Data = [
            (
                "a".to_owned(),
                Value::Map([("y".to_owned(), Value::Int(2))].into_iter().collect()),
            ),
            ("b".to_owned(), Value::Bool(true)),
        ]
        .into_iter()
        .collect();
        merge_into(&mut data, &set);
        let a = data["a"].as_map().unwrap();
        assert_eq!(a["x"], Value::Int(1));
        assert_eq!(a["y"], Value::Int(2));
        assert_eq!(data["b"], Value::Bool(true));
    }

    #[test]
    fn a_slug_is_safe_as_a_directory_name() {
        assert_eq!(slug("Prod compiles nginx!"), "prod-compiles-nginx");
        assert_eq!(slug("../.."), "case");
    }

    #[test]
    fn the_scratch_path_of_a_case_stays_under_the_state_directory() {
        let dir = Path::new("state")
            .join("tests")
            .join(slug("../x"))
            .join(format!("{}-{}", 1, slug("..")));
        assert!(dir.starts_with("state/tests"));
    }
}
