//! Each step's tool output, kept on the machine that ran it.
//!
//! A [`Keeper`] wraps the run's observer and writes every line a tool
//! printed to `runs/<id>/logs/<kind>/<document>[@<endpoint>].log` under the
//! state directory, as it arrives:
//!
//! ```text
//! 2026-11-18T09:15:11.204Z stderr inputs: {"address":"10.0.0.5","db":"(sensitive)"}
//! ```
//!
//! The lines reach the observer already masked, so a secret is never written.
//! Like `history/`, the files stay in the state directory and are never sent
//! to a backend. A step's file is capped: past the cap its head and its tail
//! are kept, and a `meridian` line says how much was cut between them.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use meridian_sdk::{LogLine, LogStream};

use crate::rollback::{RollbackOutcome, RollbackReport};
use crate::{time, Event, Observer, Outcome, StageId, StageKind, StageReport};

/// The directory under `runs/<id>/` the logs are kept in.
pub const LOGS_DIR: &str = "logs";

/// How much of one step's output is kept unless a caller says otherwise.
pub const DEFAULT_LOG_LIMIT: u64 = 10 * 1024 * 1024;

/// Where a run's logs are kept.
pub fn run_dir(state_dir: &Path, run: &str) -> PathBuf {
    state_dir.join("runs").join(run).join(LOGS_DIR)
}

/// A step's log file, relative to [`run_dir`]: `resourceset/web@10.0.0.5.log`.
/// Anything in the endpoint a file name cannot hold everywhere becomes `_`.
pub fn file_name(id: &StageId, endpoint: &str) -> String {
    let kind = match id.kind {
        StageKind::Infrastructure => "infrastructure",
        StageKind::ResourceSet => "resourceset",
    };
    if endpoint.is_empty() {
        format!("{kind}/{}.log", id.name)
    } else {
        format!("{kind}/{}@{}.log", id.name, safe(endpoint))
    }
}

fn safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// One line of a log file, read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub time: String,
    /// `stdout` or `stderr` for the tool's own lines; `meridian` for what
    /// Meridian added: a failure, or how much was cut.
    pub stream: String,
    pub text: String,
}

impl Line {
    /// Reads one line as [`Keeper`] wrote it. A line of another shape is
    /// kept whole as text.
    pub fn parse(line: &str) -> Line {
        let mut parts = line.splitn(3, ' ');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(time), Some(stream @ ("stdout" | "stderr" | "meridian")), text) => Line {
                time: time.to_owned(),
                stream: stream.to_owned(),
                text: text.unwrap_or_default().to_owned(),
            },
            _ => Line {
                time: String::new(),
                stream: String::new(),
                text: line.to_owned(),
            },
        }
    }
}

/// Reads a log file back, line by line.
pub fn read(path: &Path) -> Result<Vec<Line>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(Line::parse)
        .collect())
}

/// Every log file a run kept, as paths relative to [`run_dir`], sorted.
pub fn list(state_dir: &Path, run: &str) -> Vec<String> {
    let dir = run_dir(state_dir, run);
    let mut found = Vec::new();
    for kind in ["infrastructure", "resourceset"] {
        let Ok(entries) = std::fs::read_dir(dir.join(kind)) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".log") && entry.path().is_file() {
                    found.push(format!("{kind}/{name}"));
                }
            }
        }
    }
    found.sort();
    found
}

/// Removes the logs of every run not in `keep`, the runs whose records are
/// still kept: the logs go with their run.
pub fn prune(state_dir: &Path, keep: &[String]) -> Result<(), String> {
    let runs = state_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !time::is_run_id(&id) || keep.contains(&id) {
            continue;
        }
        let logs = runs.join(&id).join(LOGS_DIR);
        if logs.is_dir() {
            std::fs::remove_dir_all(&logs)
                .map_err(|e| format!("removing {}: {e}", logs.display()))?;
        }
        // Only an empty directory goes; a local record may still be in it.
        let _ = std::fs::remove_dir(runs.join(&id));
    }
    Ok(())
}

/// One step's open log file.
struct StepLog {
    path: PathBuf,
    file: File,
    /// Bytes in the file.
    written: u64,
    /// The most kept from the start, and the most kept from the end.
    half: u64,
    /// Lines past the head, held until the step ends: only the last `half`
    /// bytes of them are written.
    tail: VecDeque<String>,
    tail_bytes: u64,
    cut_lines: u64,
    cut_bytes: u64,
}

impl StepLog {
    fn open(path: PathBuf, limit: u64) -> std::io::Result<StepLog> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        // Tool output is masked, but it is still what the tool said about
        // the machine: readable by its owner only, like the record.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(StepLog {
            path,
            file,
            written,
            half: limit / 2,
            tail: VecDeque::new(),
            tail_bytes: 0,
            cut_lines: 0,
            cut_bytes: 0,
        })
    }

    fn line(&mut self, stream: &str, text: &str) -> std::io::Result<()> {
        let at = time::now();
        for part in text.split('\n') {
            let part = part.strip_suffix('\r').unwrap_or(part);
            let entry = format!("{at} {stream} {part}\n");
            let size = entry.len() as u64;
            if self.tail.is_empty() && self.written + size <= self.half {
                self.file.write_all(entry.as_bytes())?;
                self.written += size;
                continue;
            }
            self.tail_bytes += size;
            self.tail.push_back(entry);
            while self.tail_bytes > self.half {
                let Some(dropped) = self.tail.pop_front() else {
                    break;
                };
                self.tail_bytes -= dropped.len() as u64;
                self.cut_lines += 1;
                self.cut_bytes += dropped.len() as u64;
            }
        }
        Ok(())
    }

    /// Writes the tail, after a line saying what was cut before it.
    fn finish(mut self) -> std::io::Result<PathBuf> {
        if self.cut_lines > 0 {
            let note = format!(
                "{} meridian cut {} lines ({} bytes) here to keep the log within {} bytes\n",
                time::now(),
                self.cut_lines,
                self.cut_bytes,
                self.half * 2
            );
            self.file.write_all(note.as_bytes())?;
        }
        for entry in std::mem::take(&mut self.tail) {
            self.file.write_all(entry.as_bytes())?;
        }
        self.file.flush()?;
        Ok(self.path)
    }
}

/// An observer that keeps each step's tool output, and passes everything on
/// to the observer it wraps.
pub struct Keeper<'a> {
    inner: &'a mut dyn Observer,
    state_dir: PathBuf,
    limit: u64,
    run: String,
    open: BTreeMap<StageId, StepLog>,
    /// The endpoint each step was last reported on.
    hosts: BTreeMap<StageId, String>,
    /// Set once writing failed: the run goes on, keeping nothing more.
    broken: bool,
    warnings: Vec<String>,
}

impl<'a> Keeper<'a> {
    /// Keeps the logs under `state_dir`, at most `limit` bytes per step.
    pub fn new(inner: &'a mut dyn Observer, state_dir: &Path, limit: u64) -> Keeper<'a> {
        Keeper {
            inner,
            state_dir: state_dir.to_owned(),
            limit: limit.max(2),
            run: String::new(),
            open: BTreeMap::new(),
            hosts: BTreeMap::new(),
            broken: false,
            warnings: Vec::new(),
        }
    }

    /// Writes what is still held and closes every file. What could not be
    /// written is returned as warnings.
    pub fn close(&mut self) -> Vec<String> {
        let ids: Vec<StageId> = self.open.keys().cloned().collect();
        for id in ids {
            self.finish(&id, None);
        }
        std::mem::take(&mut self.warnings)
    }

    fn failed(&mut self, e: std::io::Error) {
        self.broken = true;
        self.open.clear();
        self.warnings.push(format!(
            "tool output is no longer kept in {}: {e}",
            run_dir(&self.state_dir, &self.run).display()
        ));
    }

    fn write(&mut self, id: &StageId, stream: &str, text: &str) {
        if self.broken || self.run.is_empty() {
            return;
        }
        if !self.open.contains_key(id) {
            let host = self.hosts.get(id).cloned().unwrap_or_default();
            let path = run_dir(&self.state_dir, &self.run).join(file_name(id, &host));
            match StepLog::open(path, self.limit) {
                Ok(log) => {
                    self.open.insert(id.clone(), log);
                }
                Err(e) => return self.failed(e),
            }
        }
        let written = self
            .open
            .get_mut(id)
            .map_or(Ok(()), |log| log.line(stream, text));
        if let Err(e) = written {
            self.failed(e);
        }
    }

    /// Closes a step's file and, when the step's endpoint is now known,
    /// moves the file to the name that carries it.
    fn finish(&mut self, id: &StageId, host: Option<&str>) {
        if let Some(host) = host.filter(|h| !h.is_empty()) {
            self.hosts.insert(id.clone(), host.to_owned());
        }
        let Some(log) = self.open.remove(id) else {
            return;
        };
        let path = match log.finish() {
            Ok(path) => path,
            Err(e) => return self.failed(e),
        };
        let host = self.hosts.get(id).cloned().unwrap_or_default();
        let wanted = run_dir(&self.state_dir, &self.run).join(file_name(id, &host));
        if wanted != path {
            if let Err(e) = move_onto(&path, &wanted) {
                self.failed(e);
            }
        }
    }
}

/// Moves `from` to `to`, adding to the end of `to` when it is already there.
fn move_onto(from: &Path, to: &Path) -> std::io::Result<()> {
    if !to.exists() {
        return std::fs::rename(from, to);
    }
    let bytes = std::fs::read(from)?;
    OpenOptions::new()
        .append(true)
        .open(to)?
        .write_all(&bytes)?;
    std::fs::remove_file(from)
}

fn stream(line: &LogLine) -> &'static str {
    match line.stream {
        LogStream::Stdout => "stdout",
        LogStream::Stderr => "stderr",
    }
}

impl Observer for Keeper<'_> {
    fn stage_started(&mut self, id: &StageId, target: &str) {
        self.inner.stage_started(id, target);
    }

    fn log(&mut self, id: &StageId, line: &LogLine) {
        self.write(id, stream(line), &line.text);
        self.inner.log(id, line);
    }

    fn stage_finished(&mut self, report: &StageReport) {
        if !report.host.is_empty() {
            self.hosts.insert(report.id.clone(), report.host.clone());
        }
        if let Outcome::Failed(why) = &report.outcome {
            let reason = report.reason.map_or("failed", |r| r.as_str());
            self.write(
                &report.id,
                "meridian",
                &format!("step failed ({reason}): {why}"),
            );
        }
        self.finish(&report.id, Some(&report.host));
        self.inner.stage_finished(report);
    }

    fn event(&mut self, event: &Event) {
        if self.run.is_empty() {
            self.run = event.run.clone();
        }
        self.inner.event(event);
    }

    fn rollback_finished(&mut self, report: &RollbackReport) {
        if let RollbackOutcome::Failed(why) = &report.outcome {
            self.write(&report.id, "meridian", &format!("rollback failed: {why}"));
        }
        self.finish(&report.id, Some(&report.host));
        self.inner.rollback_finished(report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Quiet;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meridian-logs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn id(name: &str) -> StageId {
        StageId {
            kind: StageKind::ResourceSet,
            name: name.to_owned(),
        }
    }

    fn started(keeper: &mut Keeper<'_>, run: &str) {
        keeper.event(&Event {
            run: run.to_owned(),
            ..Event::default()
        });
    }

    fn line(text: &str) -> LogLine {
        LogLine {
            stream: LogStream::Stderr,
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_cap_keeps_the_head_and_the_tail_and_says_what_was_cut() {
        let dir = scratch("cap");
        let mut quiet = Quiet;
        let mut keeper = Keeper::new(&mut quiet, &dir, 400);
        started(&mut keeper, "r1");
        for n in 0..100 {
            keeper.log(&id("web"), &line(&format!("line {n:03}")));
        }
        assert!(keeper.close().is_empty());
        let path = run_dir(&dir, "r1").join("resourceset/web.log");
        let lines = read(&path).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < 520, "{size}");
        assert_eq!(lines[0].text, "line 000");
        assert_eq!(lines.last().unwrap().text, "line 099");
        let cut: Vec<&Line> = lines.iter().filter(|l| l.stream == "meridian").collect();
        assert_eq!(cut.len(), 1, "{lines:#?}");
        assert!(cut[0].text.starts_with("cut "), "{}", cut[0].text);
        // The kept lines are in order, with only the middle gone.
        let numbers: Vec<&str> = lines
            .iter()
            .filter(|l| l.stream == "stderr")
            .map(|l| l.text.as_str())
            .collect();
        let mut sorted = numbers.clone();
        sorted.sort();
        assert_eq!(numbers, sorted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_moves_to_its_endpoint_when_the_step_ends_and_a_failure_is_noted() {
        let dir = scratch("endpoint");
        let mut quiet = Quiet;
        let mut keeper = Keeper::new(&mut quiet, &dir, DEFAULT_LOG_LIMIT);
        started(&mut keeper, "r2");
        keeper.log(&id("web"), &line("first\nsecond"));
        keeper.stage_finished(&StageReport {
            id: id("web"),
            target: "fake".to_owned(),
            host: "[2001:db8::1]:22".to_owned(),
            outcome: Outcome::Failed("could not write motd".to_owned()),
            reason: Some(crate::Reason::ToolFailed),
            credential: String::new(),
            artifact: None,
            warnings: Vec::new(),
            removed: Vec::new(),
            done_before_failing: Vec::new(),
            previous_good: String::new(),
            policy: None,
        });
        assert!(keeper.close().is_empty());
        assert_eq!(list(&dir, "r2"), ["resourceset/web@_2001_db8__1__22.log"]);
        let lines =
            read(&run_dir(&dir, "r2").join("resourceset/web@_2001_db8__1__22.log")).unwrap();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "first",
                "second",
                "step failed (tool-failed): could not write motd"
            ]
        );
        assert_eq!(lines[2].stream, "meridian");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_go_with_their_run() {
        let dir = scratch("prune");
        for run in ["2026-11-18T09-00-00Z-0001", "2026-11-18T09-00-01Z-0002"] {
            let logs = run_dir(&dir, run).join("resourceset");
            std::fs::create_dir_all(&logs).unwrap();
            std::fs::write(logs.join("web.log"), "x\n").unwrap();
        }
        prune(&dir, &["2026-11-18T09-00-01Z-0002".to_owned()]).unwrap();
        assert!(list(&dir, "2026-11-18T09-00-00Z-0001").is_empty());
        assert!(!dir.join("runs/2026-11-18T09-00-00Z-0001").exists());
        assert_eq!(
            list(&dir, "2026-11-18T09-00-01Z-0002"),
            ["resourceset/web.log"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_of_another_shape_reads_as_text() {
        assert_eq!(
            Line::parse("2026-11-18T09:15:11.204Z stdout create motd"),
            Line {
                time: "2026-11-18T09:15:11.204Z".to_owned(),
                stream: "stdout".to_owned(),
                text: "create motd".to_owned()
            }
        );
        assert_eq!(Line::parse("loose text").text, "loose text");
    }
}
