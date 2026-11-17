//! Where the server keeps what it is sent.
//!
//! It keeps events and nothing else, one file per run, in the layout a run's
//! own record has:
//!
//! ```text
//! <data>/projects/<project>/runs/<run>/events.jsonl
//! ```
//!
//! Everything the server shows is folded from those files when it is asked
//! for, so the server has no state of its own to drift from the events, and
//! `meridian report --from` over one of the files says what the server says.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use meridian_orchestrator::{event, time, Event};

/// How many events one run may hold. A run of this size is a mistake or an
/// attack, and the disk is not the place to find out which.
pub const MAX_EVENTS_PER_RUN: usize = 200_000;

/// Why something sent to the store was not kept.
#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    /// The request is wrong, and sending it again will not help.
    Invalid(String),
    /// The run already holds as many events as a run may.
    Full,
    /// The disk said no.
    Io(String),
}

/// What an append did.
#[derive(Debug, PartialEq, Eq)]
pub struct Appended {
    /// Events added to the run.
    pub accepted: usize,
    /// Events the run already had. Sending a batch twice is harmless.
    pub ignored: usize,
}

/// A project or a run is named in a URL and becomes a directory, so only
/// plain names are taken.
pub fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !text.starts_with('-')
}

pub struct Store {
    root: PathBuf,
    /// How many runs a project keeps; the oldest go first.
    keep: usize,
    /// One writer at a time. Reads take no lock: a file is appended to in
    /// whole lines.
    writing: Mutex<()>,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>, keep: usize) -> Store {
        Store {
            root: root.into(),
            keep: keep.max(1),
            writing: Mutex::new(()),
        }
    }

    fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    fn runs_dir(&self, project: &str) -> PathBuf {
        self.projects_dir().join(project).join("runs")
    }

    fn events_file(&self, project: &str, run: &str) -> PathBuf {
        self.runs_dir(project).join(run).join("events.jsonl")
    }

    /// Adds the events in `body`, one JSON object per line, to a run. Every
    /// line must be an event of that run, or nothing is added. An event the
    /// run already has, by its sequence number, is left out, so a sender
    /// that retries does no harm.
    pub fn append(&self, project: &str, run: &str, body: &str) -> Result<Appended, Reject> {
        if !is_name(project) {
            return Err(Reject::Invalid(format!(
                "{project:?} is not a project name"
            )));
        }
        if !time::is_run_id(run) {
            return Err(Reject::Invalid(format!("{run:?} is not a run id")));
        }
        let mut incoming = Vec::new();
        for (n, line) in body.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let event = Event::from_json(line)
                .map_err(|why| Reject::Invalid(format!("line {}: {why}", n + 1)))?;
            if event.run != run {
                return Err(Reject::Invalid(format!(
                    "line {}: the event belongs to run {:?}, not {run:?}",
                    n + 1,
                    event.run
                )));
            }
            incoming.push(event);
        }
        if incoming.is_empty() {
            return Err(Reject::Invalid("the request holds no events".to_owned()));
        }

        let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        let io = |e: std::io::Error| Reject::Io(e.to_string());
        let file = self.events_file(project, run);
        let existing = self.events(project, run).unwrap_or_default();
        let is_new = existing.is_empty();
        let mut have: Vec<u64> = existing.iter().map(|e| e.seq).collect();
        let mut fresh = Vec::new();
        let mut ignored = 0;
        for event in incoming {
            if have.contains(&event.seq) {
                ignored += 1;
            } else {
                have.push(event.seq);
                fresh.push(event);
            }
        }
        if existing.len() + fresh.len() > MAX_EVENTS_PER_RUN {
            return Err(Reject::Full);
        }
        if !fresh.is_empty() {
            if let Some(dir) = file.parent() {
                fs::create_dir_all(dir).map_err(io)?;
            }
            // What is stored is the event written out again, not the bytes
            // that arrived: the file then holds the envelope and nothing a
            // sender slipped in beside it.
            let mut out = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
                .map_err(io)?;
            out.write_all(event::to_jsonl(&fresh).as_bytes())
                .map_err(io)?;
        }
        if is_new {
            self.prune(project);
        }
        Ok(Appended {
            accepted: fresh.len(),
            ignored,
        })
    }

    /// Removes a project's oldest runs beyond the number it keeps.
    fn prune(&self, project: &str) {
        let runs = self.runs(project);
        for old in runs.iter().take(runs.len().saturating_sub(self.keep)) {
            if let Some(dir) = self.found_run(project, old) {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    /// The directory inside `dir` that is named `name`, found by reading
    /// `dir` and never by joining `name` onto it. A path the store reads is
    /// therefore always one the file system itself listed inside the data
    /// directory, whatever a request asked for: the check on names is not
    /// the only thing between a URL and the disk.
    fn child(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.filter_map(Result::ok) {
            // The name only chooses among what was listed. The path comes
            // from the listing.
            if entry.file_name().to_str() == Some(name) {
                let path = entry.path();
                return path.is_dir().then_some(path);
            }
        }
        None
    }

    /// The directory of a project that exists.
    fn found_project(&self, project: &str) -> Option<PathBuf> {
        if !is_name(project) {
            return None;
        }
        Self::child(&self.projects_dir(), project)
    }

    /// The directory of a run that exists.
    fn found_run(&self, project: &str, run: &str) -> Option<PathBuf> {
        if !time::is_run_id(run) {
            return None;
        }
        Self::child(&self.found_project(project)?.join("runs"), run)
    }

    fn names(dir: &Path, fits: fn(&str) -> bool) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| fits(name))
            .collect();
        names.sort();
        names
    }

    /// Every project, by name.
    pub fn projects(&self) -> Vec<String> {
        Self::names(&self.projects_dir(), is_name)
    }

    /// A project's runs, oldest first. A run's id starts with when it
    /// started, so the order of the names is the order of the runs.
    pub fn runs(&self, project: &str) -> Vec<String> {
        match self.found_project(project) {
            Some(dir) => Self::names(&dir.join("runs"), time::is_run_id),
            None => Vec::new(),
        }
    }

    /// A run's events, in the order of their sequence numbers; `None` when
    /// there is no such run.
    pub fn events(&self, project: &str, run: &str) -> Option<Vec<Event>> {
        let text = fs::read_to_string(self.found_run(project, run)?.join("events.jsonl")).ok()?;
        let mut events = event::from_jsonl(&text);
        // Batches can arrive out of order when a sender retries.
        events.sort_by_key(|event| event.seq);
        Some(events)
    }
}
