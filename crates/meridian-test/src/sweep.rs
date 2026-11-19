//! What a lifecycle environment leaves in its state directory so it can be
//! destroyed later, and `meridian lab sweep`, which does that.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use meridian_ir::Data;
use meridian_orchestrator::{run_selected, LocalBackend, Mode, Options, Quiet, Selector};
use meridian_plugin_host::Registry;
use meridian_value::Value;

/// The file in an environment's state directory that says how to destroy it.
pub const RECORD_FILE: &str = "environment.json";

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How an environment was made: enough to destroy it with nothing else.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The documents, as absolute paths.
    pub documents: Vec<PathBuf>,
    /// The data, `meridian.run.id` included.
    pub data: Data,
    /// The label selector, if the case had one.
    pub select: Option<String>,
    pub run_id: String,
    /// When it was made, in seconds since the Unix epoch.
    pub started: u64,
    /// When a sweep may destroy it; `None` only with `--older-than`.
    pub expires: Option<u64>,
    /// The test and case, or the lab cell, for the report.
    pub name: String,
}

impl Record {
    pub(crate) fn write(&self, dir: &Path) -> Result<(), String> {
        let text = |s: &str| Value::String(s.to_owned());
        let mut map = BTreeMap::new();
        map.insert(
            "documents".to_owned(),
            Value::List(
                self.documents
                    .iter()
                    .map(|p| text(&p.display().to_string()))
                    .collect(),
            ),
        );
        map.insert("data".to_owned(), Value::Map(self.data.clone()));
        map.insert(
            "select".to_owned(),
            self.select.as_deref().map_or(Value::Null, text),
        );
        map.insert("runId".to_owned(), text(&self.run_id));
        map.insert("started".to_owned(), Value::Uint(self.started));
        map.insert(
            "expires".to_owned(),
            self.expires.map_or(Value::Null, Value::Uint),
        );
        map.insert("name".to_owned(), text(&self.name));
        let path = dir.join(RECORD_FILE);
        std::fs::write(&path, Value::Map(map).pretty())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Reads the record in `dir`.
    pub fn read(dir: &Path) -> Result<Record, String> {
        let path = dir.join(RECORD_FILE);
        let shown = path.display().to_string();
        let raw = std::fs::read(&path).map_err(|e| format!("cannot read {shown}: {e}"))?;
        let map = meridian_value::decode_map(&raw).map_err(|e| format!("{shown}: {e}"))?;
        let bad = |field: &str| format!("{shown}: {field} is missing or not what it should be");
        let text = |field: &str| match map.get(field) {
            Some(Value::String(s)) => Ok(s.clone()),
            _ => Err(bad(field)),
        };
        let number = |field: &str| match map.get(field) {
            Some(Value::Uint(n)) => Ok(Some(*n)),
            Some(Value::Int(n)) if *n >= 0 => Ok(Some(*n as u64)),
            Some(Value::Null) | None => Ok(None),
            _ => Err(bad(field)),
        };
        let documents = match map.get("documents") {
            Some(Value::List(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(s) => Ok(PathBuf::from(s)),
                    _ => Err(bad("documents")),
                })
                .collect::<Result<_, _>>()?,
            _ => return Err(bad("documents")),
        };
        let data = match map.get("data") {
            Some(Value::Map(data)) => data.clone(),
            _ => return Err(bad("data")),
        };
        let select = match map.get("select") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        Ok(Record {
            documents,
            data,
            select,
            run_id: text("runId")?,
            started: number("started")?.ok_or_else(|| bad("started"))?,
            expires: number("expires")?,
            name: text("name")?,
        })
    }
}

/// What a sweep did with one environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SweepOutcome {
    /// Destroyed, and its directory removed.
    Destroyed,
    /// Not due yet: no expiry passed, and `--older-than` not reached.
    NotDue,
    /// It could not be destroyed; the directory is kept.
    Failed(String),
}

/// One environment a sweep found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swept {
    pub dir: PathBuf,
    pub name: String,
    pub run_id: String,
    pub outcome: SweepOutcome,
}

/// Every environment record under `tests/` and `lab/` in the state
/// directory, in path order.
pub fn environments(state_dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        if dir.join(RECORD_FILE).is_file() {
            found.push(dir.to_path_buf());
            return;
        }
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for sub in dirs {
            walk(&sub, depth - 1, found);
        }
    }
    let mut found = Vec::new();
    // tests/<test>/<case>, lab/<lab>/<run>/<cell>.
    walk(&state_dir.join("tests"), 2, &mut found);
    walk(&state_dir.join("lab"), 3, &mut found);
    found
}

/// Destroys every environment whose expiry has passed, or, with
/// `older_than`, that was made at least that long ago. A destroyed
/// environment's directory is removed; one that fails stays, to try again.
pub fn sweep(state_dir: &Path, registry: &Registry, older_than: Option<Duration>) -> Vec<Swept> {
    let now = now();
    environments(state_dir)
        .into_iter()
        .map(|dir| {
            let record = match Record::read(&dir) {
                Ok(record) => record,
                Err(why) => {
                    return Swept {
                        dir,
                        name: String::new(),
                        run_id: String::new(),
                        outcome: SweepOutcome::Failed(why),
                    }
                }
            };
            let expired = record.expires.is_some_and(|at| at <= now);
            let old = older_than.is_some_and(|age| record.started + age.as_secs() <= now);
            let outcome = if expired || old {
                match destroy(&dir, &record, registry) {
                    Ok(()) => match std::fs::remove_dir_all(&dir) {
                        Ok(()) => SweepOutcome::Destroyed,
                        Err(e) => SweepOutcome::Failed(format!(
                            "destroyed, and the directory could not be removed: {e}"
                        )),
                    },
                    Err(why) => SweepOutcome::Failed(why),
                }
            } else {
                SweepOutcome::NotDue
            };
            Swept {
                dir,
                name: record.name,
                run_id: record.run_id,
                outcome,
            }
        })
        .collect()
}

fn destroy(dir: &Path, record: &Record, registry: &Registry) -> Result<(), String> {
    let bundle = meridian_ir::load(&record.documents)
        .map_err(|e| format!("the documents do not load: {e}"))?;
    let selector = match &record.select {
        Some(text) => Selector::parse(text)?,
        None => Selector::all(),
    };
    let options = Options {
        data: record.data.clone(),
        backend: Some(Arc::new(LocalBackend::new(dir)) as Arc<_>),
        command: format!("lab sweep ({})", record.name),
        keep_runs: 1,
        ..Options::new(dir)
    };
    let report = run_selected(
        &bundle,
        registry,
        Mode::Destroy,
        &options,
        &selector,
        &mut Quiet,
    );
    if let Some(refusal) = &report.refused {
        return Err(format!("the destroy was refused: {refusal}"));
    }
    let failures = crate::run::stage_failures(&report);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_reads_back_as_written() {
        let dir = std::env::temp_dir().join(format!("meridian-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut data = Data::new();
        meridian_ir::set_run_id(&mut data, "t1");
        data.insert("replicas".to_owned(), Value::Int(2));
        let record = Record {
            documents: vec![PathBuf::from("/x/site.yaml")],
            data,
            select: Some("tier=web".to_owned()),
            run_id: "t1".to_owned(),
            started: 10,
            expires: Some(20),
            name: "site: web".to_owned(),
        };
        record.write(&dir).unwrap();
        assert_eq!(Record::read(&dir).unwrap(), record);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
