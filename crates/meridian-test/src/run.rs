//! Running the cases of a test document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use meridian_ir::{Bundle, Data};
use meridian_orchestrator::{
    run_selected, LocalBackend, Mode, Options, Outcome, Quiet, RunReport, Selector,
};
use meridian_plugin_host::{Registry, Stop};
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
    /// Leave a lifecycle case's environment in place instead of destroying
    /// it, to look at.
    pub keep: bool,
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
    /// A lifecycle phase that did not run: verify after a failed provision,
    /// destroy under `--keep`.
    Skipped,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Error => "error",
            Status::Skipped => "skipped",
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
    /// A lifecycle case's phases, in the order they ran; empty for the
    /// other levels.
    pub phases: Vec<PhaseResult>,
    pub millis: u128,
}

/// One phase of a lifecycle case.
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseResult {
    /// `provision`, `verify` or `destroy`.
    pub name: &'static str,
    pub status: Status,
    /// Why it failed, or why it was skipped, one line each.
    pub failures: Vec<String>,
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
        let (status, failures, stages, phases) = match run_case(file, case, index, runner) {
            Ok(ran) => {
                let failed = !ran.failures.is_empty()
                    || ran.phases.iter().any(|p| p.status == Status::Failed);
                let status = if failed {
                    Status::Failed
                } else {
                    Status::Passed
                };
                (status, ran.failures, ran.stages, ran.phases)
            }
            Err(why) => (Status::Error, vec![why], Vec::new(), Vec::new()),
        };
        cases.push(CaseResult {
            name: case.name.clone(),
            run: case.run,
            status,
            failures,
            stages,
            phases,
            millis: started.elapsed().as_millis(),
        });
    }
    FileResult {
        path: file.path.clone(),
        name: file.name.clone(),
        cases,
    }
}

#[derive(Default)]
struct Ran {
    /// Every failure, of every phase.
    failures: Vec<String>,
    stages: Vec<StageSummary>,
    phases: Vec<PhaseResult>,
}

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
    let scratch = runner
        .state_dir
        .join("tests")
        .join(slug(&file.name))
        .join(format!("{}-{}", index + 1, slug(&case.name)));
    if case.run == Run::Lifecycle {
        return lifecycle(file, case, runner, &bundle, &data, &selector, &scratch);
    }
    // A directory of the case's own, emptied first: a plan is always against
    // nothing applied, so what a tool would do is the same on every run.
    let _ = std::fs::remove_dir_all(&scratch);
    create(&scratch)?;
    let mode = match case.run {
        Run::Compile => Mode::Compile,
        _ => Mode::Plan,
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
    Ok(Ran {
        failures: check(case, &report.stages, &data),
        stages: summaries(&report),
        phases: Vec::new(),
    })
}

fn create(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create the scratch directory {}: {e}", dir.display()))
}

fn summaries(report: &RunReport) -> Vec<StageSummary> {
    report
        .stages
        .iter()
        .map(|s| StageSummary {
            name: s.id.name.clone(),
            target: s.target.clone(),
            outcome: outcome_word(s).to_owned(),
        })
        .collect()
}

/// Provision, verify and destroy. Destroy runs whatever came before, so a
/// failed or timed-out provision leaves nothing behind; when destroy itself
/// fails, the scratch directory is kept and named, so what is left can be
/// found and removed.
fn lifecycle(
    file: &TestFile,
    case: &Case,
    runner: &Runner<'_>,
    bundle: &Bundle,
    data: &Data,
    selector: &Selector,
    scratch: &Path,
) -> Result<Ran, String> {
    // A target that cannot destroy would leave the environment behind.
    let mut cannot: Vec<String> = Vec::new();
    for document in &bundle.documents {
        let target = document.target();
        match runner.registry.target(target) {
            Some(t) if t.info().destroys => {}
            Some(_) => {
                if !cannot.iter().any(|t| t == target) {
                    cannot.push(target.to_owned());
                }
            }
            None => return Err(format!("no plugin serves the target {target:?}")),
        }
    }
    if !cannot.is_empty() {
        return Err(format!(
            "a lifecycle case destroys what it provisions, and {} cannot destroy",
            cannot
                .iter()
                .map(|t| format!("the target {t:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // What an earlier run left behind is not thrown away unseen.
    if scratch.exists() {
        return Err(format!(
            "{} holds an environment an earlier run kept or did not destroy; destroy it \
             with `meridian destroy --yes --state-dir {}` and the same documents and data, \
             then remove the directory",
            scratch.display(),
            scratch.display()
        ));
    }
    create(scratch)?;
    let options = |mode_stop: Stop| Options {
        data: data.clone(),
        backend: Some(Arc::new(LocalBackend::new(scratch)) as Arc<_>),
        command: format!("test {} ({})", file.name, case.name),
        keep_runs: 1,
        stop: mode_stop,
        ..Options::new(scratch)
    };
    let run = |mode: Mode, options: &Options| {
        run_selected(bundle, runner.registry, mode, options, selector, &mut Quiet)
    };

    let mut ran = Ran::default();
    let timer = case.timeout.map(Timer::start);
    let stop = || match &timer {
        Some(t) => Stop::when(Arc::clone(&t.flag)),
        None => Stop::default(),
    };
    let timed_out = || timer.as_ref().is_some_and(Timer::expired);

    // Provision.
    let started = Instant::now();
    let applied = run(Mode::Apply, &options(stop()));
    let mut failures = Vec::new();
    if let Some(refusal) = &applied.refused {
        failures.push(format!("the apply was refused: {refusal}"));
    }
    failures.extend(stage_failures(&applied));
    if timed_out() {
        failures.push(format!("timed out after {}", shown(case.timeout)));
    }
    ran.stages = summaries(&applied);
    let provisioned = failures.is_empty();
    ran.phase("provision", started, failures);

    // Verify.
    let started = Instant::now();
    if provisioned {
        let mut failures = check(case, &applied.stages, data);
        if case.phases.no_drift {
            let planned = run(Mode::Plan, &options(stop()));
            failures.extend(drift(&planned));
        }
        if timed_out() {
            failures.push(format!("timed out after {}", shown(case.timeout)));
        }
        ran.phase("verify", started, failures);
    } else {
        ran.skip("verify", "provision failed".to_owned());
    }
    drop(timer);

    // Destroy, whatever came before.
    let started = Instant::now();
    if runner.keep {
        ran.skip(
            "destroy",
            format!(
                "kept by --keep: the environment's state is in {}",
                scratch.display()
            ),
        );
        return Ok(ran);
    }
    let destroy_stop = match case.timeout {
        Some(limit) => Stop::after(limit),
        None => Stop::default(),
    };
    let destroyed = run(Mode::Destroy, &options(destroy_stop));
    let mut failures = Vec::new();
    if let Some(refusal) = &destroyed.refused {
        failures.push(format!("the destroy was refused: {refusal}"));
    }
    failures.extend(stage_failures(&destroyed));
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(scratch);
    } else {
        failures.push(format!(
            "what is left is recorded in {}, which is kept",
            scratch.display()
        ));
    }
    ran.phase("destroy", started, failures);
    Ok(ran)
}

impl Ran {
    fn phase(&mut self, name: &'static str, started: Instant, failures: Vec<String>) {
        self.failures
            .extend(failures.iter().map(|f| format!("{name}: {f}")));
        self.phases.push(PhaseResult {
            name,
            status: if failures.is_empty() {
                Status::Passed
            } else {
                Status::Failed
            },
            failures,
            millis: started.elapsed().as_millis(),
        });
    }

    fn skip(&mut self, name: &'static str, why: String) {
        self.phases.push(PhaseResult {
            name,
            status: Status::Skipped,
            failures: vec![why],
            millis: 0,
        });
    }
}

/// Each stage that failed or did not run, one line each.
fn stage_failures(report: &RunReport) -> Vec<String> {
    report
        .stages
        .iter()
        .filter_map(|s| match &s.outcome {
            Outcome::Failed(why) => Some(format!("document {:?} failed: {why}", s.id.name)),
            Outcome::NotRun { because_of } => Some(format!(
                "document {:?} did not run, because {because_of} failed",
                s.id.name
            )),
            _ => None,
        })
        .collect()
}

/// A plan after provision that would change anything.
fn drift(report: &RunReport) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(refusal) = &report.refused {
        failures.push(format!("the drift plan was refused: {refusal}"));
    }
    for stage in &report.stages {
        match &stage.outcome {
            Outcome::Planned(plan) if plan.changes.is_empty() && !plan.unknown => {}
            Outcome::Planned(plan) if plan.unknown => failures.push(format!(
                "document {:?}: drift: none, and the tool cannot say what a plan would change",
                stage.id.name
            )),
            Outcome::Planned(plan) => failures.push(format!(
                "document {:?}: drift: none, and a plan after provision would change: {}",
                stage.id.name,
                plan.changes
                    .iter()
                    .map(|c| format!("{} {}", c.action.as_str(), c.resource))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Outcome::LeftOut { .. } => {}
            _ => failures.push(format!(
                "document {:?}: the drift plan came to {}",
                stage.id.name,
                outcome_word(stage)
            )),
        }
    }
    failures
}

fn shown(limit: Option<Duration>) -> String {
    let seconds = limit.map_or(0, |d| d.as_secs());
    match seconds {
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// Sets a flag when the case's time is up. Dropping it ends the wait.
struct Timer {
    flag: Arc<AtomicBool>,
    done: Option<mpsc::Sender<()>>,
}

impl Timer {
    fn start(limit: Duration) -> Timer {
        let flag = Arc::new(AtomicBool::new(false));
        let (done, wait) = mpsc::channel::<()>();
        let set = Arc::clone(&flag);
        std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(limit) {
                set.store(true, Ordering::SeqCst);
            }
        });
        Timer {
            flag,
            done: Some(done),
        }
    }

    fn expired(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
    }
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
