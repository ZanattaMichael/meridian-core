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
pub type Resolver<'a> = &'a (dyn Fn(&[PathBuf]) -> Result<Vec<PathBuf>, String> + Sync);

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
    /// What the documents read as `meridian.run.id`: `test` for a compile
    /// or plan case, a new id for each run of a lifecycle case. Empty when
    /// the case could not run.
    pub run_id: String,
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
        cases.push(result_of(file, index, runner, None));
    }
    FileResult {
        path: file.path.clone(),
        name: file.name.clone(),
        cases,
    }
}

/// Runs one case and says how it ended. `place` puts a lifecycle case
/// somewhere other than `tests/`, under a given run id, as a lab cell does.
pub(crate) fn result_of(
    file: &TestFile,
    index: usize,
    runner: &Runner<'_>,
    place: Option<&Place>,
) -> CaseResult {
    let case = &file.cases[index];
    let started = Instant::now();
    let (status, failures, stages, phases, run_id) =
        match run_case(file, case, index, runner, place) {
            Ok(ran) => {
                let failed = !ran.failures.is_empty()
                    || ran.phases.iter().any(|p| p.status == Status::Failed);
                let status = if failed {
                    Status::Failed
                } else {
                    Status::Passed
                };
                (status, ran.failures, ran.stages, ran.phases, ran.run_id)
            }
            Err(why) => (
                Status::Error,
                vec![why],
                Vec::new(),
                Vec::new(),
                String::new(),
            ),
        };
    CaseResult {
        name: case.name.clone(),
        run: case.run,
        status,
        failures,
        stages,
        run_id,
        phases,
        millis: started.elapsed().as_millis(),
    }
}

/// Where a lifecycle case runs when it is not a test's own: a lab cell.
#[derive(Clone, Debug)]
pub(crate) struct Place {
    /// Its state directory, which must not exist yet.
    pub scratch: PathBuf,
    /// What the documents read as `meridian.run.id`.
    pub run_id: String,
    /// What the documents read as `meridian.lab`.
    pub lab: Value,
    /// When `meridian lab sweep` may destroy what is left, in seconds since
    /// the Unix epoch.
    pub expires: Option<u64>,
}

#[derive(Default)]
struct Ran {
    /// Every failure, of every phase.
    failures: Vec<String>,
    stages: Vec<StageSummary>,
    phases: Vec<PhaseResult>,
    run_id: String,
}

/// The run id of a compile or plan case: the same every time, so what a
/// file is expected to hold does not change from run to run.
pub const TEST_RUN_ID: &str = "test";

fn run_case(
    file: &TestFile,
    case: &Case,
    index: usize,
    runner: &Runner<'_>,
    place: Option<&Place>,
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
    let reserved = meridian_ir::RESERVED_DATA_KEY;
    if data.contains_key(reserved) {
        return Err(format!(
            "a data file sets {reserved}, which is Meridian's own key: meridian test gives each \
             case its run id"
        ));
    }
    merge_into(&mut data, &file.set);
    merge_into(&mut data, &case.set);
    let selector = match case.select.as_deref().or(file.select.as_deref()) {
        Some(text) => Selector::parse(text).map_err(|e| format!("select: {e}"))?,
        None => Selector::all(),
    };
    let scratch = match place {
        Some(place) => place.scratch.clone(),
        None => runner
            .state_dir
            .join("tests")
            .join(slug(&file.name))
            .join(format!("{}-{}", index + 1, slug(&case.name))),
    };
    if case.run == Run::Lifecycle {
        let ready = Ready {
            bundle: &bundle,
            documents: &documents,
            data: &data,
            selector: &selector,
            select: case.select.as_deref().or(file.select.as_deref()),
            scratch: &scratch,
        };
        return lifecycle(file, case, runner, &ready, place);
    }
    meridian_ir::set_run_id(&mut data, TEST_RUN_ID);
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
        run_id: TEST_RUN_ID.to_owned(),
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
/// What a lifecycle case runs over, once loaded.
struct Ready<'a> {
    bundle: &'a Bundle,
    documents: &'a [PathBuf],
    data: &'a Data,
    selector: &'a Selector,
    select: Option<&'a str>,
    scratch: &'a Path,
}

fn lifecycle(
    file: &TestFile,
    case: &Case,
    runner: &Runner<'_>,
    ready: &Ready<'_>,
    place: Option<&Place>,
) -> Result<Ran, String> {
    let (bundle, selector, scratch) = (ready.bundle, ready.selector, ready.scratch);
    // One id for every phase, so destroy finds what provision named.
    let run_id = match place {
        Some(place) => place.run_id.clone(),
        None => new_run_id(scratch),
    };
    let mut data = ready.data.clone();
    meridian_ir::set_run_id(&mut data, &run_id);
    if let (Some(place), Some(Value::Map(meridian))) =
        (place, data.get_mut(meridian_ir::RESERVED_DATA_KEY))
    {
        meridian.insert("lab".to_owned(), place.lab.clone());
    }
    let data = &data;
    // A target that cannot destroy would leave the environment behind. Only
    // the documents this case runs count: a lab cell leaves the others out
    // with `when`.
    let mut cannot: Vec<String> = Vec::new();
    for document in bundle.documents.iter().filter(|d| runs(d, selector, data)) {
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
        let earlier = std::fs::read_to_string(scratch.join(RUN_ID_FILE))
            .map(|id| format!(" --run-id {}", id.trim()))
            .unwrap_or_default();
        return Err(format!(
            "{} holds an environment an earlier run kept or did not destroy; destroy it \
             with `meridian destroy --yes --state-dir {}{earlier}` and the same documents and \
             data, then remove the directory",
            scratch.display(),
            scratch.display()
        ));
    }
    create(scratch)?;
    // Written first, so whoever cleans up after a failure knows the id the
    // environment's names and tags carry, and `meridian lab sweep` can
    // destroy it.
    std::fs::write(scratch.join(RUN_ID_FILE), format!("{run_id}\n"))
        .map_err(|e| format!("cannot write {}: {e}", scratch.join(RUN_ID_FILE).display()))?;
    let record = crate::sweep::Record {
        documents: ready
            .documents
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect(),
        data: data.clone(),
        select: ready.select.map(str::to_owned),
        run_id: run_id.clone(),
        started: crate::sweep::now(),
        expires: place.and_then(|p| p.expires),
        name: format!("{}: {}", file.name, case.name),
    };
    record.write(scratch)?;
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

    let mut ran = Ran {
        run_id: run_id.clone(),
        ..Ran::default()
    };
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
                "kept by --keep: the environment's state is in {}, its run id is {run_id}",
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
            "what is left is recorded in {}, which is kept; its run id is {run_id}",
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
pub(crate) fn stage_failures(report: &RunReport) -> Vec<String> {
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

/// Whether a document is part of a run under this selector and data: its
/// labels match and its `when` holds. One that cannot be decided counts,
/// so its error is reported by the run.
fn runs(document: &meridian_ir::Document, selector: &Selector, data: &Data) -> bool {
    let labelled = meridian_ir::resolved_labels(document, data)
        .map_or(true, |labels| selector.matches(&labels));
    let when = document.when();
    labelled
        && (when.is_empty()
            || meridian_ir::Condition::parse(when)
                .and_then(|c| c.evaluate(data))
                .unwrap_or(true))
}

/// The file in a lifecycle case's scratch directory that holds its run id.
const RUN_ID_FILE: &str = "run-id";

/// A run id no other run is likely to have: `t` and 8 hex digits, from the
/// time, the process and the case's directory.
fn new_run_id(scratch: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    scratch.hash(&mut hasher);
    format!("t{:08x}", hasher.finish() as u32)
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
pub(crate) fn slug(name: &str) -> String {
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

/// What proving one fault against a lifecycle environment came to.
#[derive(Clone, Debug, PartialEq)]
pub struct FaultResult {
    pub name: String,
    pub status: Status,
    /// Why it failed or could not run, one line each, phase-prefixed.
    pub failures: Vec<String>,
    pub phases: Vec<PhaseResult>,
    /// The id of the check run, the one expected to fail.
    pub run_id: String,
    pub millis: u128,
}

/// Proves one fault against the lifecycle case of `file` at `case_index`:
/// provision the environment, inject the fault, check that the run now fails
/// the way `fault.expect` says, and destroy whatever came before. The phases
/// are reported like a lifecycle case's; the check run is expected to fail,
/// and does not fail the proof when it fails as expected.
pub fn run_fault(
    fault: &crate::fault::FaultFile,
    file: &TestFile,
    case_index: usize,
    runner: &Runner<'_>,
) -> FaultResult {
    let started = Instant::now();
    let Some(case) = file.cases.get(case_index) else {
        return fault_error(
            &fault.name,
            format!("no case at index {}", case_index + 1),
            started,
        );
    };
    if case.run != Run::Lifecycle {
        return fault_error(
            &fault.name,
            format!(
                "case {:?} is not a lifecycle case; a fault needs an environment \
                 a run provisions and destroys",
                case.name
            ),
            started,
        );
    }
    let (status, failures, phases, run_id) = match fault_run(fault, file, case, runner) {
        Ok(ran) => {
            let failed =
                !ran.failures.is_empty() || ran.phases.iter().any(|p| p.status == Status::Failed);
            (
                if failed {
                    Status::Failed
                } else {
                    Status::Passed
                },
                ran.failures,
                ran.phases,
                ran.run_id,
            )
        }
        Err(why) => (Status::Error, vec![why], Vec::new(), String::new()),
    };
    FaultResult {
        name: fault.name.clone(),
        status,
        failures,
        phases,
        run_id,
        millis: started.elapsed().as_millis(),
    }
}

fn fault_error(name: &str, why: String, started: Instant) -> FaultResult {
    FaultResult {
        name: name.to_owned(),
        status: Status::Error,
        failures: vec![why],
        phases: Vec::new(),
        run_id: String::new(),
        millis: started.elapsed().as_millis(),
    }
}

/// The data a fault's phases run with: the file's and the case's data files
/// read and merged over the sets, with the run id set. Re-read after the
/// injection, which may have changed the files.
fn case_data(
    file: &TestFile,
    case: &Case,
    runner: &Runner<'_>,
    run_id: &str,
) -> Result<Data, String> {
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
    let reserved = meridian_ir::RESERVED_DATA_KEY;
    if data.contains_key(reserved) {
        return Err(format!(
            "a data file sets {reserved}, which is Meridian's own key: meridian test gives each \
             case its run id"
        ));
    }
    merge_into(&mut data, &file.set);
    merge_into(&mut data, &case.set);
    meridian_ir::set_run_id(&mut data, run_id);
    Ok(data)
}

fn fault_run(
    fault: &crate::fault::FaultFile,
    file: &TestFile,
    case: &Case,
    runner: &Runner<'_>,
) -> Result<Ran, String> {
    let documents = (runner.resolve)(&file.documents)?;
    let bundle =
        meridian_ir::load(&documents).map_err(|e| format!("the documents do not load: {e}"))?;
    bundle
        .validate()
        .map_err(|e| format!("the documents are not valid: {e}"))?;
    let selector = match case.select.as_deref().or(file.select.as_deref()) {
        Some(text) => Selector::parse(text).map_err(|e| format!("select: {e}"))?,
        None => Selector::all(),
    };
    let scratch = runner
        .state_dir
        .join("faults")
        .join(slug(&fault.name))
        .join(slug(&file.name))
        .join(slug(&case.name));
    let run_id = new_run_id(&scratch);
    // Read fresh for every run after the injection: a fault may break the
    // world by changing what the documents read, and the check must see it.
    let mut data = case_data(file, case, runner, &run_id)?;
    let reload = |data: &mut Data| -> Result<(), String> {
        *data = case_data(file, case, runner, &run_id)?;
        Ok(())
    };
    // What an earlier proof left behind is not thrown away unseen.
    if scratch.exists() {
        let earlier = std::fs::read_to_string(scratch.join(RUN_ID_FILE))
            .map(|id| format!(" --run-id {}", id.trim()))
            .unwrap_or_default();
        return Err(format!(
            "{} holds an environment an earlier proof kept or did not destroy; destroy it \
             with `meridian destroy --yes --state-dir {}{earlier}` and the same documents and \
             data, then remove the directory",
            scratch.display(),
            scratch.display()
        ));
    }
    let mut cannot: Vec<String> = Vec::new();
    for document in bundle
        .documents
        .iter()
        .filter(|d| runs(d, &selector, &data))
    {
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
            "a fault proof destroys what it provisions, and {} cannot destroy",
            cannot
                .iter()
                .map(|t| format!("the target {t:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    create(&scratch)?;
    std::fs::write(scratch.join(RUN_ID_FILE), format!("{run_id}\n"))
        .map_err(|e| format!("cannot write {}: {e}", scratch.join(RUN_ID_FILE).display()))?;
    let record = crate::sweep::Record {
        documents: documents
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect(),
        data: data.clone(),
        select: case
            .select
            .as_deref()
            .or(file.select.as_deref())
            .map(str::to_owned),
        run_id: run_id.clone(),
        started: crate::sweep::now(),
        expires: None,
        name: format!("fault {}: {}: {}", fault.name, file.name, case.name),
    };
    record.write(&scratch)?;
    let options = |data: &Data| Options {
        data: data.clone(),
        backend: Some(Arc::new(LocalBackend::new(&scratch)) as Arc<_>),
        command: format!("fault {} ({})", fault.name, case.name),
        keep_runs: 1,
        ..Options::new(&scratch)
    };
    let run = |mode: Mode, data: &Data| {
        run_selected(
            &bundle,
            runner.registry,
            mode,
            &options(data),
            &selector,
            &mut Quiet,
        )
    };
    let mut ran = Ran::default();

    // Provision: the environment the fault is about to break.
    let started = Instant::now();
    let applied = run(Mode::Apply, &data);
    let mut failures = Vec::new();
    if let Some(refusal) = &applied.refused {
        failures.push(format!("the apply was refused: {refusal}"));
    }
    failures.extend(stage_failures(&applied));
    let provisioned = failures.is_empty();
    ran.phase("provision", started, failures);

    // Inject: break the environment. A failed tool run is the injection
    // working, not a failed phase; only a refusal or a command that cannot
    // run fails the phase.
    let started = Instant::now();
    let mut failures = Vec::new();
    if provisioned {
        match &fault.inject {
            crate::fault::Inject::Command(words) => {
                let shown = words.join(" ");
                match std::process::Command::new(&words[0])
                    .args(&words[1..])
                    .current_dir(&scratch)
                    .output()
                {
                    Ok(output) if output.status.success() => {}
                    Ok(output) => failures.push(format!(
                        "the injection command {shown:?} exited with {}",
                        output.status
                    )),
                    Err(e) => {
                        failures.push(format!("the injection command {shown:?} did not run: {e}"))
                    }
                }
            }
            crate::fault::Inject::Document(path) => {
                let shown = path.display().to_string();
                match meridian_ir::load(std::slice::from_ref(path)) {
                    Ok(injected) => match injected.validate() {
                        Ok(()) => {
                            let report = run_selected(
                                &injected,
                                runner.registry,
                                Mode::Apply,
                                &options(&data),
                                &Selector::all(),
                                &mut Quiet,
                            );
                            if let Some(refusal) = &report.refused {
                                failures
                                    .push(format!("the injection document was refused: {refusal}"));
                            }
                        }
                        Err(e) => failures
                            .push(format!("the injection document {shown} is not valid: {e}")),
                    },
                    Err(e) => {
                        failures.push(format!("the injection document {shown} does not load: {e}"))
                    }
                }
            }
        }
    } else {
        failures.push("provision failed; there is nothing to inject the fault into".to_owned());
    }
    let injected = failures.is_empty();
    ran.phase("inject", started, failures);

    // Check: the run over the broken environment must fail the way the
    // fault expects, naming the cause word and the resource.
    let started = Instant::now();
    let mut failures = Vec::new();
    let mut check_id = String::new();
    if injected {
        reload(&mut data)?;
        let checked = run(Mode::Apply, &data);
        if let Some(refusal) = &checked.refused {
            failures.push(format!("the check apply was refused: {refusal}"));
        } else if checked
            .stages
            .iter()
            .all(|s| !matches!(s.outcome, Outcome::Failed(_)))
        {
            failures.push(
                "the fault did not surface: the apply over the broken environment succeeded"
                    .to_owned(),
            );
        } else {
            let backend = LocalBackend::new(&scratch);
            let runs = meridian_orchestrator::record::list_runs(&backend)?;
            check_id = runs.last().cloned().unwrap_or_default();
            let kept = meridian_orchestrator::logs::read_failures(&scratch, &check_id);
            fn text<'m>(map: &'m BTreeMap<String, Value>, key: &str) -> &'m str {
                match map.get(key) {
                    Some(Value::String(s)) => s.as_str(),
                    _ => "",
                }
            }
            // Test runs keep no logs (Quiet), so the failures a machine
            // would keep are absent: fall back to the run's events, which
            // hold the resource and the cause of every failure.
            let from_events: Vec<(String, String, String)> = if kept.is_empty() {
                let backend = LocalBackend::new(&scratch);
                meridian_orchestrator::record::load_events(&backend, &check_id)?
                    .into_iter()
                    .filter(|e| e.kind == meridian_orchestrator::event::RESOURCE_FAILED)
                    .map(|e| {
                        (
                            text(&e.detail, "cause").to_owned(),
                            text(&e.detail, "resource").to_owned(),
                            e.endpoint,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let matched = if !kept.is_empty() {
                kept.iter().any(|f| {
                    text(f, "cause") == fault.expect.cause
                        && text(f, "resource") == fault.expect.resource
                        && fault
                            .expect
                            .endpoint
                            .as_deref()
                            .is_none_or(|endpoint| text(f, "endpoint") == endpoint)
                })
            } else {
                from_events.iter().any(|(cause, resource, endpoint)| {
                    cause == &fault.expect.cause
                        && resource == &fault.expect.resource
                        && fault
                            .expect
                            .endpoint
                            .as_deref()
                            .is_none_or(|wanted| endpoint == wanted)
                })
            };
            if !matched {
                let seen: Vec<String> = if !kept.is_empty() {
                    kept.iter()
                        .map(|f| {
                            format!(
                                "{}/{}/{}",
                                text(f, "cause"),
                                text(f, "resource"),
                                text(f, "endpoint")
                            )
                        })
                        .collect()
                } else {
                    from_events
                        .iter()
                        .map(|(cause, resource, endpoint)| format!("{cause}/{resource}/{endpoint}"))
                        .collect()
                };
                let seen = seen.join(", ");
                failures.push(format!(
                    "the run failed, but not the way the fault expects: wanted cause {:?} \
                     on resource {:?}{}, and the failures name: {}",
                    fault.expect.cause,
                    fault.expect.resource,
                    fault
                        .expect
                        .endpoint
                        .as_deref()
                        .map(|e| format!(" on endpoint {e:?}"))
                        .unwrap_or_default(),
                    if seen.is_empty() { "nothing" } else { &seen }
                ));
            }
        }
    }
    let checked_ok = failures.is_empty();
    ran.run_id = check_id.clone();
    ran.phase("check", started, failures);

    // Diagnose: the command the issue says must find the fault.
    let started = Instant::now();
    let mut failures = Vec::new();
    if checked_ok {
        if let Some(first) = &fault.expect.diagnose_first {
            let backend = LocalBackend::new(&scratch);
            match meridian_orchestrator::record::load_run(&backend, &check_id)? {
                Some(run) => {
                    let events = meridian_orchestrator::record::load_events(&backend, &check_id)?;
                    let kept = meridian_orchestrator::logs::read_failures(&scratch, &check_id);
                    let found = meridian_orchestrator::diagnose::diagnose(
                        &run,
                        &events,
                        &kept,
                        meridian_orchestrator::diagnose::Filter::default(),
                    );
                    match found
                        .failed
                        .first()
                        .and_then(|step| step.entries.first())
                        .and_then(|entry| entry.failures.first())
                    {
                        Some(failure) if &failure.resource == first => {}
                        _ => failures.push(format!(
                            "diagnose did not name {first:?} first over the check run"
                        )),
                    }
                }
                None => failures.push(format!(
                    "the check run {check_id} left no record to diagnose"
                )),
            }
        }
    }
    ran.phase("diagnose", started, failures);

    // Destroy, whatever came before.
    let started = Instant::now();
    if runner.keep {
        ran.skip(
            "destroy",
            format!(
                "kept by --keep: the environment's state is in {}, its run id is {run_id}",
                scratch.display()
            ),
        );
        return Ok(ran);
    }
    let destroyed = run(Mode::Destroy, &data);
    let mut failures = Vec::new();
    if let Some(refusal) = &destroyed.refused {
        failures.push(format!("the destroy was refused: {refusal}"));
    }
    failures.extend(stage_failures(&destroyed));
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&scratch);
    } else {
        failures.push(format!(
            "what is left is recorded in {}, which is kept; its run id is {run_id}",
            scratch.display()
        ));
    }
    ran.phase("destroy", started, failures);
    Ok(ran)
}
