//! `meridian lab`: a matrix of lifecycle environments, run side by side.
//!
//! A `Lab` document names Test files and a matrix of values. Each lifecycle
//! case of each file, under each combination of the matrix's values, is a
//! cell. A cell runs the case under a state directory of its own, with its
//! own run id, and reads the combination as `meridian.lab.<axis>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use meridian_value::Value;

use crate::document::{duration, Reader, Run, TestFile};
use crate::run::{result_of, slug, CaseResult, Place, Runner};

/// The `kind` of a lab document.
pub const LAB_KIND: &str = "Lab";

/// How long a cell's environment lives before `meridian lab sweep` may
/// destroy it, unless the lab says otherwise.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(2 * 3600);

/// Names a matrix axis cannot take: they are the lab's own.
const RESERVED_AXES: &[&str] = &["run", "cell"];

/// One `Lab` document.
#[derive(Clone, Debug, PartialEq)]
pub struct LabFile {
    pub path: PathBuf,
    /// `metadata.name`.
    pub name: String,
    /// `spec.environments`: Test files, resolved against the lab's own
    /// directory. Their lifecycle cases are the environments.
    pub environments: Vec<PathBuf>,
    /// `spec.matrix`: each axis and its values, in name order.
    pub matrix: BTreeMap<String, Vec<String>>,
    /// `spec.exclude`: combinations left out; each names some axes.
    pub exclude: Vec<BTreeMap<String, String>>,
    /// `spec.concurrency`: how many cells run at once.
    pub concurrency: usize,
    /// `spec.expiry`: how long a cell's environment lives.
    pub expiry: Duration,
}

/// Reads the one `Lab` document in the file.
pub fn load_lab(path: &Path) -> Result<LabFile, String> {
    let shown = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|e| format!("{shown}: cannot read file: {e}"))?;
    let roots = meridian_ir::parse_stream(&shown, &bytes).map_err(|e| e.to_string())?;
    let [root] = roots.as_slice() else {
        return Err(format!(
            "{shown}: a lab file holds exactly one Lab document"
        ));
    };
    let base = path.parent().unwrap_or(Path::new("."));
    let r = Reader { at: &shown, base };
    let top = r.map(root, "")?;
    r.only(top, "", &["apiVersion", "kind", "metadata", "spec"])?;
    if top.get("apiVersion").and_then(Value::as_str) != Some(meridian_ir::API_VERSION_V1) {
        return Err(r.refuse(
            "apiVersion",
            format!("expected {:?}", meridian_ir::API_VERSION_V1),
        ));
    }
    if top.get("kind").and_then(Value::as_str) != Some(LAB_KIND) {
        return Err(r.refuse(
            "kind",
            format!("expected {LAB_KIND:?}, which is what meridian lab runs"),
        ));
    }
    let metadata = r.map(
        top.get("metadata")
            .ok_or_else(|| r.refuse("metadata", "missing required field"))?,
        "metadata",
    )?;
    r.only(metadata, "metadata", &["name", "description"])?;
    let name = r.text(
        metadata
            .get("name")
            .ok_or_else(|| r.refuse("metadata.name", "missing required field"))?,
        "metadata.name",
    )?;
    if slug(&name) != name {
        return Err(r.refuse(
            "metadata.name",
            "lowercase letters, digits and -, as it names a directory",
        ));
    }
    let spec = r.map(
        top.get("spec")
            .ok_or_else(|| r.refuse("spec", "missing required field"))?,
        "spec",
    )?;
    r.only(
        spec,
        "spec",
        &["environments", "matrix", "exclude", "concurrency", "expiry"],
    )?;
    let environments = r.paths(spec.get("environments"), "spec.environments")?;
    if environments.is_empty() {
        return Err(r.refuse(
            "spec.environments",
            "must name at least one Test file whose lifecycle cases are the environments",
        ));
    }
    let mut matrix = BTreeMap::new();
    if let Some(value) = spec.get("matrix") {
        for (axis, values) in r.map(value, "spec.matrix")? {
            let at = format!("spec.matrix.{axis}");
            if RESERVED_AXES.contains(&axis.as_str()) || !is_axis(axis) {
                return Err(r.refuse(
                    &at,
                    format!(
                        "an axis is named with letters, digits and _, and not {}",
                        RESERVED_AXES.join(" or ")
                    ),
                ));
            }
            let Value::List(items) = values else {
                return Err(r.refuse(&at, "expected a list of values"));
            };
            let words = items
                .iter()
                .enumerate()
                .map(|(i, v)| scalar(&r, v, &format!("{at}[{i}]")))
                .collect::<Result<Vec<_>, _>>()?;
            if words.is_empty() {
                return Err(r.refuse(&at, "must hold at least one value"));
            }
            matrix.insert(axis.clone(), words);
        }
    }
    let mut exclude = Vec::new();
    if let Some(value) = spec.get("exclude") {
        let Value::List(items) = value else {
            return Err(r.refuse("spec.exclude", "expected a list of combinations"));
        };
        for (i, item) in items.iter().enumerate() {
            let at = format!("spec.exclude[{i}]");
            let mut combination = BTreeMap::new();
            for (axis, v) in r.map(item, &at)? {
                let Some(values) = matrix.get(axis) else {
                    return Err(r.refuse(&format!("{at}.{axis}"), "no such axis in spec.matrix"));
                };
                let word = scalar(&r, v, &format!("{at}.{axis}"))?;
                if !values.contains(&word) {
                    return Err(r.refuse(
                        &format!("{at}.{axis}"),
                        format!("{word:?} is not one of the axis's values"),
                    ));
                }
                combination.insert(axis.clone(), word);
            }
            exclude.push(combination);
        }
    }
    let concurrency = match spec.get("concurrency") {
        None => 1,
        Some(Value::Int(n)) if *n >= 1 => *n as usize,
        Some(Value::Uint(n)) if *n >= 1 => *n as usize,
        Some(_) => return Err(r.refuse("spec.concurrency", "expected a whole number of 1 or more")),
    };
    let expiry = match spec.get("expiry") {
        None => DEFAULT_EXPIRY,
        Some(v) => duration(&r.text(v, "spec.expiry")?).map_err(|e| r.refuse("spec.expiry", e))?,
    };
    Ok(LabFile {
        path: path.to_path_buf(),
        name,
        environments,
        matrix,
        exclude,
        concurrency,
        expiry,
    })
}

fn is_axis(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A matrix value as text: text, a number or true/false.
fn scalar(r: &Reader<'_>, value: &Value, path: &str) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Uint(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        other => Err(r.refuse(
            path,
            format!(
                "expected text, a number or true/false, found {}",
                other.describe()
            ),
        )),
    }
}

/// One cell: a lifecycle case under one combination of the matrix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Its number in the lab, from 1.
    pub number: usize,
    /// Which of the loaded Test documents, and which case of it.
    pub test: usize,
    pub case: usize,
    /// The combination: a value for each axis.
    pub values: BTreeMap<String, String>,
    /// `<test>: <case> [axis=value, ...]`.
    pub name: String,
}

/// Every cell of the lab, in order: each Test document, each of its
/// lifecycle cases, each combination with the axes in name order and the
/// values in the order written. An excluded combination is left out, and
/// so is one that does not match every `filter` (`axis=value`).
pub fn cells(
    lab: &LabFile,
    tests: &[TestFile],
    filters: &[(String, String)],
) -> Result<Vec<Cell>, String> {
    for (axis, _) in filters {
        if !lab.matrix.contains_key(axis) {
            return Err(format!(
                "--filter {axis}: the lab has no such axis (it has: {})",
                if lab.matrix.is_empty() {
                    "none".to_owned()
                } else {
                    lab.matrix.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            ));
        }
    }
    let mut combinations: Vec<BTreeMap<String, String>> = vec![BTreeMap::new()];
    for (axis, values) in &lab.matrix {
        combinations = combinations
            .into_iter()
            .flat_map(|done| {
                values.iter().map(move |value| {
                    let mut next = done.clone();
                    next.insert(axis.clone(), value.clone());
                    next
                })
            })
            .collect();
    }
    combinations.retain(|c| {
        !lab.exclude
            .iter()
            .any(|ex| ex.iter().all(|(axis, value)| c.get(axis) == Some(value)))
            && filters
                .iter()
                .all(|(axis, value)| c.get(axis) == Some(value))
    });
    let mut out = Vec::new();
    for (t, test) in tests.iter().enumerate() {
        for (c, case) in test.cases.iter().enumerate() {
            if case.run != Run::Lifecycle {
                continue;
            }
            for values in &combinations {
                let shown = values
                    .iter()
                    .map(|(a, v)| format!("{a}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push(Cell {
                    number: out.len() + 1,
                    test: t,
                    case: c,
                    values: values.clone(),
                    name: if shown.is_empty() {
                        format!("{}: {}", test.name, case.name)
                    } else {
                        format!("{}: {} [{shown}]", test.name, case.name)
                    },
                });
            }
        }
    }
    Ok(out)
}

/// One cell's result.
#[derive(Clone, Debug, PartialEq)]
pub struct CellResult {
    pub cell: Cell,
    pub result: CaseResult,
}

/// Runs the cells, `lab.concurrency` at a time, under
/// `<state>/lab/<lab>/<run>/<number>-<case>`, and gives the results in cell
/// order. One cell failing does not stop the others.
pub fn run_lab(
    lab: &LabFile,
    tests: &[TestFile],
    cells: &[Cell],
    runner: &Runner<'_>,
    run: &str,
) -> Vec<CellResult> {
    let base = runner.state_dir.join("lab").join(&lab.name).join(run);
    let expires = crate::sweep::now() + lab.expiry.as_secs();
    let results = at_most(lab.concurrency, cells, |cell| {
        let test = &tests[cell.test];
        let mut lab_values: BTreeMap<String, Value> = cell
            .values
            .iter()
            .map(|(a, v)| (a.clone(), Value::String(v.clone())))
            .collect();
        lab_values.insert("run".to_owned(), Value::String(run.to_owned()));
        lab_values.insert("cell".to_owned(), Value::Uint(cell.number as u64));
        let place = Place {
            scratch: base.join(format!(
                "{}-{}",
                cell.number,
                slug(&test.cases[cell.case].name)
            )),
            run_id: format!("{run}-{}", cell.number),
            lab: Value::Map(lab_values),
            expires: Some(expires),
        };
        result_of(test, cell.case, runner, Some(&place))
    });
    cells
        .iter()
        .cloned()
        .zip(results)
        .map(|(cell, result)| CellResult { cell, result })
        .collect()
}

/// Calls `work` on every item, at most `limit` at a time, and gives the
/// results in the items' order.
fn at_most<T: Sync, R: Send>(limit: usize, items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..limit.clamp(1, items.len().max(1)) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(item) = items.get(i) else { break };
                let result = work(item);
                if let Ok(mut done) = done.lock() {
                    done[i] = Some(result);
                }
            });
        }
    });
    done.into_inner()
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .collect()
}

/// A lab run's id: `l` and 8 hex digits. With a cell's number after it, it
/// is the cell's run id, at most 20 characters.
pub fn new_lab_run_id(lab: &LabFile) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    lab.name.hash(&mut hasher);
    format!("l{:08x}", hasher.finish() as u32)
}

/// The file under `<state>/lab/<lab>/` that holds the last run's results,
/// for `meridian lab list`.
pub const LAST_RUN_FILE: &str = "last-run.json";

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(matrix: &[(&str, &[&str])], exclude: &[&[(&str, &str)]]) -> LabFile {
        LabFile {
            path: PathBuf::from("x.lab.yaml"),
            name: "x".to_owned(),
            environments: vec![],
            matrix: matrix
                .iter()
                .map(|(a, vs)| {
                    (
                        (*a).to_owned(),
                        vs.iter().map(|v| (*v).to_owned()).collect(),
                    )
                })
                .collect(),
            exclude: exclude
                .iter()
                .map(|c| {
                    c.iter()
                        .map(|(a, v)| ((*a).to_owned(), (*v).to_owned()))
                        .collect()
                })
                .collect(),
            concurrency: 1,
            expiry: DEFAULT_EXPIRY,
        }
    }

    fn test_file(cases: &[(&str, Run)]) -> TestFile {
        TestFile {
            path: PathBuf::from("t.test.yaml"),
            name: "t".to_owned(),
            documents: vec![],
            data: vec![],
            set: Default::default(),
            select: None,
            cases: cases
                .iter()
                .map(|(name, run)| crate::document::Case {
                    name: (*name).to_owned(),
                    run: *run,
                    data: vec![],
                    set: Default::default(),
                    select: None,
                    expect: Default::default(),
                    assert: vec![],
                    phases: Default::default(),
                    timeout: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_matrix_expands_leaves_out_exclusions_and_filters_and_only_lifecycle_cases_count() {
        let lab = lab(
            &[
                ("target", &["terraform", "opentofu", "pulumi"]),
                ("provider", &["aws", "gcp"]),
            ],
            &[&[("target", "pulumi"), ("provider", "gcp")]],
        );
        let tests = [test_file(&[
            ("web", Run::Lifecycle),
            ("compiles", Run::Compile),
        ])];
        let all = cells(&lab, &tests, &[]).unwrap();
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "t: web [provider=aws, target=terraform]",
                "t: web [provider=aws, target=opentofu]",
                "t: web [provider=aws, target=pulumi]",
                "t: web [provider=gcp, target=terraform]",
                "t: web [provider=gcp, target=opentofu]",
            ]
        );
        assert_eq!(all[4].number, 5);
        let some = cells(
            &lab,
            &tests,
            &[("target".to_owned(), "opentofu".to_owned())],
        )
        .unwrap();
        assert_eq!(some.len(), 2);
        assert!(cells(&lab, &tests, &[("region".to_owned(), "x".to_owned())]).is_err());
    }

    #[test]
    fn work_runs_at_most_the_limit_at_once_and_comes_back_in_order() {
        use std::sync::atomic::AtomicUsize;
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let items: Vec<usize> = (0..12).collect();
        let out = at_most(3, &items, |i| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            running.fetch_sub(1, Ordering::SeqCst);
            i * 2
        });
        assert_eq!(out, items.iter().map(|i| i * 2).collect::<Vec<_>>());
        assert_eq!(most.load(Ordering::SeqCst), 3);
        assert_eq!(at_most(0, &items[..2], |i| *i), [0, 1]);
    }

    #[test]
    fn no_matrix_is_one_cell_per_case() {
        let tests = [test_file(&[("a", Run::Lifecycle), ("b", Run::Lifecycle)])];
        let all = cells(&lab(&[], &[]), &tests, &[]).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].name, "t: b");
    }
}
