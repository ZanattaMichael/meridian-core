//! Running analyzers over what a stage compiled to, before its tool sees
//! it.
//!
//! An analyzer is a program named `meridian-analyze-<name>`, found where
//! plugins are found. For each stage that compiles it is started once, with
//! the compiled files written to a directory of their own, and one JSON
//! document, the [input](input), on standard input. It answers on standard
//! output with one JSON object:
//!
//! ```json
//! {"findings": [{"rule": "CKV_AWS_18", "severity": "medium", "message": "...",
//!                "file": "main.tf.json", "line": 12, "resource": "logs"}],
//!  "measures": [{"name": "monthly-cost", "value": 12.5, "unit": "USD"}]}
//! ```
//!
//! and exits 0. Either list may be missing. Anything else means the
//! analyzer could not look, and a stage that could not be analyzed fails as
//! one that was analyzed and found wanting would: a scanner that was not run
//! has not passed anything.
//!
//! A finding that names a resource, or a line of a compiled file Meridian
//! can trace to one, is reported against the line of the Meridian document
//! that resource was written on, which is the line a person can act on.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use meridian_sdk::tool::write_artifact;
use meridian_sdk::{decode_value, Artifact, ResourceGraph, Value};

/// What every analyzer's file name starts with.
pub const PROGRAM_PREFIX: &str = "meridian-analyze-";

/// What a setting's name is prefixed with in an analyzer's environment.
pub const SETTING_PREFIX: &str = "MERIDIAN_ANALYZE_";

/// How bad a finding is. The order is the order of the words: `info` is
/// the least, `critical` the most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

impl Severity {
    /// Every severity, least first.
    pub const ALL: [Severity; 5] = [
        Severity::Info,
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = String;

    /// Reads a severity. The words the scanners use for the same levels
    /// (`error`, `warning`, `note`, `unknown`) are read too, so an analyzer
    /// may pass its tool's word on.
    fn from_str(word: &str) -> Result<Severity, String> {
        match word.trim().to_ascii_lowercase().as_str() {
            "critical" => Ok(Severity::Critical),
            "high" | "error" => Ok(Severity::High),
            "medium" | "moderate" | "warning" => Ok(Severity::Medium),
            "low" => Ok(Severity::Low),
            "info" | "informational" | "note" | "unknown" | "none" => Ok(Severity::Info),
            other => Err(format!(
                "{other:?} is not a severity; the severities are critical, high, medium, low and \
                 info"
            )),
        }
    }
}

/// At what severity a finding fails the stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FailOn {
    /// No finding fails the stage; they are only reported.
    Never,
    /// A finding at this severity or above fails the stage.
    #[default]
    High,
    AtLeast(Severity),
}

impl FailOn {
    /// The severity a finding has to reach, when one does.
    pub fn threshold(self) -> Option<Severity> {
        match self {
            FailOn::Never => None,
            FailOn::High => Some(Severity::High),
            FailOn::AtLeast(severity) => Some(severity),
        }
    }

    pub fn as_str(self) -> String {
        match self.threshold() {
            None => "none".to_owned(),
            Some(severity) => severity.as_str().to_owned(),
        }
    }
}

impl FromStr for FailOn {
    type Err = String;

    fn from_str(word: &str) -> Result<FailOn, String> {
        match word.trim().to_ascii_lowercase().as_str() {
            "none" | "never" => Ok(FailOn::Never),
            other => other.parse().map(FailOn::AtLeast).map_err(|_| {
                format!(
                    "{word:?} is not a severity to fail on; write critical, high, medium, low, \
                     info or none"
                )
            }),
        }
    }
}

/// One analyzer a run asks about every stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analyzer {
    /// Its name, as in `meridian-analyze-<name>`.
    pub name: String,
    /// Its program.
    pub program: PathBuf,
    /// Its settings, as given with `--analyze-config <name>.KEY=VALUE`.
    /// Each reaches the program as `MERIDIAN_ANALYZE_<KEY>`.
    pub settings: BTreeMap<String, String>,
}

impl Analyzer {
    /// Finds `meridian-analyze-<name>` in `dirs`, the directories plugins
    /// are looked for in.
    pub fn find(name: &str, dirs: &[PathBuf]) -> Result<Analyzer, String> {
        let safe = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe {
            return Err(format!("{name:?} is not an analyzer name"));
        }
        let file = format!("{PROGRAM_PREFIX}{name}");
        for dir in dirs {
            for candidate in [dir.join(&file), dir.join(format!("{file}.exe"))] {
                if candidate.is_file() {
                    return Ok(Analyzer {
                        name: name.to_owned(),
                        program: candidate,
                        settings: BTreeMap::new(),
                    });
                }
            }
        }
        let searched: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        Err(format!(
            "no analyzer {name:?}: there is no program named {file} in {}",
            if searched.is_empty() {
                "any plugin directory".to_owned()
            } else {
                searched.join(", ")
            }
        ))
    }

    /// Reads a `<name>.KEY=VALUE` setting, and says which analyzer it is
    /// for. The key is kept as typed; the program sees it upper-cased.
    pub fn parse_setting(text: &str) -> Result<(String, String, String), String> {
        let (name, rest) = text.split_once('.').ok_or_else(|| {
            format!("{text:?} is not an analyzer setting; write it as NAME.KEY=VALUE")
        })?;
        let (key, value) = rest.split_once('=').ok_or_else(|| {
            format!("{text:?} is not an analyzer setting; write it as NAME.KEY=VALUE")
        })?;
        let fine = !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if name.is_empty() || !fine {
            return Err(format!(
                "{text:?} is not an analyzer setting; the key may hold letters, digits, - and _"
            ));
        }
        Ok((name.to_owned(), key.to_owned(), value.to_owned()))
    }

    /// Runs the analyzer over one stage's compiled files.
    ///
    /// This never fails: an analyzer that could not look is an
    /// [`Analysis`] that says so, and that did not pass.
    pub fn analyze(&self, scratch: &Path, subject: &Subject<'_>) -> Analysis {
        let mut analysis = Analysis {
            analyzer: self.name.clone(),
            asked: false,
            findings: Vec::new(),
            measures: Vec::new(),
            error: String::new(),
        };
        match self.ask(scratch, subject) {
            Ok((findings, measures)) => {
                analysis.asked = true;
                analysis.findings = findings
                    .into_iter()
                    .map(|found| locate(found, subject, &self.name))
                    .collect();
                analysis.measures = measures;
            }
            Err(why) => {
                analysis.error = format!(
                    "the analyzer {:?} could not look at this stage, and a scanner that was not \
                     run has not passed anything: {why}",
                    self.name
                );
            }
        }
        analysis
    }

    fn ask(
        &self,
        scratch: &Path,
        subject: &Subject<'_>,
    ) -> Result<(Vec<Found>, Vec<Measure>), String> {
        let program = self.program.display();
        // The compiled files, in a directory of their own that exists only
        // while the analyzer runs. Everything a scanner reads is a file.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = scratch.join(format!(
            "meridian-analyze-{}-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            sanitize(subject.name),
            sanitize(&self.name)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        write_artifact(&dir, subject.artifact)
            .map_err(|e| format!("writing the compiled files for {program}: {e}"))?;
        let dir = dir.canonicalize().unwrap_or(dir);
        let cleanup = Cleanup(dir.clone());

        let mut command = Command::new(&self.program);
        command
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &self.settings {
            command.env(setting_variable(key), value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("starting {program}: {e}"))?;

        // Written from its own thread: an analyzer may answer before it has
        // read everything, and a full pipe on either side would otherwise
        // stop both.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let document = input(subject, &dir, &self.settings)
            .canonical()
            .into_bytes();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&document);
        });
        let mut stderr = child.stderr.take().expect("stderr was piped");
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let mut answer = Vec::new();
        let read = child
            .stdout
            .take()
            .expect("stdout was piped")
            .read_to_end(&mut answer);
        let status = child
            .wait()
            .map_err(|e| format!("waiting for {program}: {e}"))?;
        let _ = writer.join();
        let errors = errors.join().unwrap_or_default();
        drop(cleanup);
        read.map_err(|e| format!("reading the answer of {program}: {e}"))?;

        if !status.success() {
            let said = last_line(&errors);
            return Err(match status.code() {
                Some(code) => format!("{program} exited with status {code}{said}"),
                None => format!("{program} was stopped by a signal{said}"),
            });
        }
        parse_answer(&answer).map_err(|why| format!("the answer of {program} {why}"))
    }
}

/// Removes the directory the compiled files were written to, however the
/// analysis ends.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The environment variable a setting reaches the program as.
pub fn setting_variable(key: &str) -> String {
    let mut name = SETTING_PREFIX.to_owned();
    name.extend(key.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        }
    }));
    name
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The last thing an analyzer said on standard error, as a clause to end a
/// sentence with. An analyzer's own failure is about the analyzer or its
/// tool, so it is shown.
fn last_line(errors: &str) -> String {
    match errors.lines().rev().find(|line| !line.trim().is_empty()) {
        Some(line) => format!(": {}", line.trim()),
        None => String::new(),
    }
}

/// What a stage's analyzer input is built from.
pub struct Subject<'a> {
    /// The run's mode: `compile`, `plan` or `apply`.
    pub mode: &'a str,
    pub run: &'a str,
    /// `Infrastructure` or `ResourceSet`.
    pub kind: &'a str,
    pub name: &'a str,
    pub target: &'a str,
    /// The target's category, as its plugin declares it.
    pub category: &'a str,
    /// The machine the stage configures; empty when there is none.
    pub host: &'a str,
    pub labels: &'a BTreeMap<String, String>,
    /// The resources as they were handed to the target, with the position
    /// each was written at.
    pub graph: &'a ResourceGraph,
    pub artifact: &'a Artifact,
}

/// The document an analyzer is given. Its shape is part of Meridian's
/// interface, documented in `docs/reference/analyzers.md`: a field is
/// added, never renamed or removed.
pub fn input(subject: &Subject<'_>, dir: &Path, settings: &BTreeMap<String, String>) -> Value {
    let text = |s: &str| Value::from(s);
    let map = |fields: Vec<(&str, Value)>| {
        Value::Map(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    };
    let files = subject
        .artifact
        .files
        .keys()
        .map(|path| text(path))
        .collect();
    map(vec![
        (
            "run",
            map(vec![
                ("id", text(subject.run)),
                ("mode", text(subject.mode)),
            ]),
        ),
        (
            "stage",
            map(vec![
                ("name", text(subject.name)),
                ("kind", text(subject.kind)),
                ("target", text(subject.target)),
                ("category", text(subject.category)),
                ("host", text(subject.host)),
                (
                    "labels",
                    Value::Map(
                        subject
                            .labels
                            .iter()
                            .map(|(key, value)| (key.clone(), text(value)))
                            .collect(),
                    ),
                ),
            ]),
        ),
        (
            "artifact",
            map(vec![
                ("dir", text(&dir.display().to_string())),
                ("files", Value::List(files)),
            ]),
        ),
        (
            "settings",
            Value::Map(
                settings
                    .iter()
                    .map(|(key, value)| (key.clone(), text(value)))
                    .collect(),
            ),
        ),
    ])
}

/// A finding as the analyzer reported it, before it is traced to the
/// document.
#[derive(Clone, Debug, Default, PartialEq)]
struct Found {
    rule: String,
    severity: Severity,
    message: String,
    file: String,
    line: u64,
    resource: String,
    url: String,
}

/// One thing an analyzer found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finding {
    /// Which analyzer reported it.
    pub analyzer: String,
    /// The rule's own id, as the scanner names it: `CKV_AWS_18`,
    /// `AVD-AWS-0088`.
    pub rule: String,
    pub severity: Severity,
    /// What the rule says. An analyzer names the rule and does not quote a
    /// file, because the message is kept in the run's record.
    pub message: String,
    /// The compiled file the finding is in, as a path inside the artifact;
    /// empty when the finding is about the stage as a whole.
    pub file: String,
    /// The line of that file, counted from 1; 0 when not known.
    pub line: u64,
    /// The id of the resource the finding is about, when the analyzer said
    /// or Meridian could tell.
    pub resource: String,
    /// The Meridian document the resource was written in, and the line it
    /// starts on; empty and 0 when the finding could not be traced.
    pub document: String,
    pub document_line: u64,
    /// Where the rule is explained, when the analyzer says.
    pub url: String,
    /// Whether the baseline accepts this finding. An accepted finding is
    /// reported and does not fail the stage.
    pub accepted: bool,
}

impl Finding {
    /// Where the finding is, in a few words: `site.yaml:12 (main.tf.json:40)`.
    pub fn place(&self) -> String {
        let compiled = match (self.file.is_empty(), self.line) {
            (true, _) => String::new(),
            (false, 0) => self.file.clone(),
            (false, line) => format!("{}:{line}", self.file),
        };
        let document = match (self.document.is_empty(), self.document_line) {
            (true, _) => String::new(),
            (false, 0) => self.document.clone(),
            (false, line) => format!("{}:{line}", self.document),
        };
        match (document.is_empty(), compiled.is_empty()) {
            (true, true) => String::new(),
            (true, false) => compiled,
            (false, true) => document,
            (false, false) => format!("{document} ({compiled})"),
        }
    }
}

/// A number an analyzer measured, such as a monthly cost.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measure {
    /// What was measured, in the analyzer's words: `monthly-cost`,
    /// `monthly-cost-delta`.
    pub name: String,
    pub value: f64,
    /// Its unit, such as `USD`; may be empty.
    pub unit: String,
}

/// What one analyzer said about one stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    pub analyzer: String,
    /// Whether the analyzer looked. False when [`Analysis::error`] says why
    /// it could not.
    pub asked: bool,
    pub findings: Vec<Finding>,
    pub measures: Vec<Measure>,
    /// Why the analyzer could not look; empty when it did.
    pub error: String,
}

impl Analysis {
    /// The findings the baseline does not accept, at `threshold` or above.
    pub fn failing(&self, threshold: Option<Severity>) -> Vec<&Finding> {
        let Some(threshold) = threshold else {
            return Vec::new();
        };
        self.findings
            .iter()
            .filter(|finding| !finding.accepted && finding.severity >= threshold)
            .collect()
    }

    /// Whether the analysis lets the stage go on: the analyzer looked, and
    /// nothing it found reaches the threshold.
    pub fn passed(&self, threshold: Option<Severity>) -> bool {
        self.asked && self.failing(threshold).is_empty()
    }

    /// How many findings there are at each severity, accepted ones left
    /// out, and how many were accepted.
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for finding in &self.findings {
            if finding.accepted {
                counts.accepted += 1;
                continue;
            }
            match finding.severity {
                Severity::Critical => counts.critical += 1,
                Severity::High => counts.high += 1,
                Severity::Medium => counts.medium += 1,
                Severity::Low => counts.low += 1,
                Severity::Info => counts.info += 1,
            }
        }
        counts
    }
}

/// How many findings there were, by severity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub critical: u64,
    pub high: u64,
    pub medium: u64,
    pub low: u64,
    pub info: u64,
    pub accepted: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.critical + self.high + self.medium + self.low + self.info
    }

    /// The counts in a few words: `1 high, 2 medium`; `none` when there are
    /// no findings.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (count, word) in [
            (self.critical, "critical"),
            (self.high, "high"),
            (self.medium, "medium"),
            (self.low, "low"),
            (self.info, "info"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {word}"));
            }
        }
        if self.accepted > 0 {
            parts.push(format!("{} accepted", self.accepted));
        }
        if parts.is_empty() {
            "none".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

/// Reads an analyzer's answer.
fn parse_answer(json: &[u8]) -> Result<(Vec<Found>, Vec<Measure>), String> {
    if json.iter().all(u8::is_ascii_whitespace) {
        return Err("is empty".to_owned());
    }
    let value = decode_value(json).map_err(|e| format!("is not JSON: {e}"))?;
    let Some(map) = value.as_map() else {
        return Err(format!(
            "is {}, and it has to be an object with `findings` and `measures`",
            value.describe()
        ));
    };
    if let Some(key) = map.keys().find(|k| *k != "findings" && *k != "measures") {
        return Err(format!(
            "has the key {key:?}, and only `findings` and `measures` are understood"
        ));
    }
    let list = |key: &str| -> Result<Vec<&BTreeMap<String, Value>>, String> {
        let Some(value) = map.get(key) else {
            return Ok(Vec::new());
        };
        let Some(list) = value.as_list() else {
            return Err(format!(
                "has `{key}` as {}, and it has to be a list of objects",
                value.describe()
            ));
        };
        list.iter()
            .map(|item| {
                item.as_map().ok_or_else(|| {
                    format!(
                        "has {} in `{key}`, and each has to be an object",
                        item.describe()
                    )
                })
            })
            .collect()
    };
    let text = |fields: &BTreeMap<String, Value>, key: &str| -> Result<String, String> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(text)) => Ok(text.clone()),
            Some(other) => Err(format!(
                "has a finding whose `{key}` is {}, and it has to be a string",
                other.describe()
            )),
        }
    };
    let mut findings = Vec::new();
    for fields in list("findings")? {
        let rule = text(fields, "rule")?;
        if rule.is_empty() {
            return Err("has a finding without a `rule`".to_owned());
        }
        let severity = match fields.get("severity") {
            None | Some(Value::Null) => Severity::Medium,
            Some(Value::String(word)) => word
                .parse()
                .map_err(|why| format!("has a finding whose severity {why}"))?,
            Some(other) => {
                return Err(format!(
                    "has a finding whose `severity` is {}, and it has to be a string",
                    other.describe()
                ))
            }
        };
        let line = match fields.get("line") {
            None | Some(Value::Null) => 0,
            Some(Value::Uint(n)) => *n,
            Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
            Some(Value::Float(n)) if n.fract() == 0.0 && *n >= 0.0 => *n as u64,
            Some(other) => {
                return Err(format!(
                    "has a finding whose `line` is {}, and it has to be a number",
                    other.describe()
                ))
            }
        };
        findings.push(Found {
            rule,
            severity,
            message: text(fields, "message")?,
            file: text(fields, "file")?,
            line,
            resource: text(fields, "resource")?,
            url: text(fields, "url")?,
        });
    }
    let mut measures = Vec::new();
    for fields in list("measures")? {
        let name = text(fields, "name")?;
        if name.is_empty() {
            return Err("has a measure without a `name`".to_owned());
        }
        let value = match fields.get("value") {
            Some(Value::Float(n)) => *n,
            Some(Value::Int(n)) => *n as f64,
            Some(Value::Uint(n)) => *n as f64,
            Some(other) => {
                return Err(format!(
                    "has a measure whose `value` is {}, and it has to be a number",
                    other.describe()
                ))
            }
            None => return Err(format!("has the measure {name:?} without a `value`")),
        };
        if !value.is_finite() {
            return Err(format!(
                "has the measure {name:?} with a value that is not a number"
            ));
        }
        measures.push(Measure {
            name,
            value,
            unit: text(fields, "unit")?,
        });
    }
    Ok((findings, measures))
}

/// Traces a finding to the document: by the resource the analyzer named,
/// or by the compiled line, which is traced to the nearest resource whose
/// id is written on or above it in the compiled file.
fn locate(found: Found, subject: &Subject<'_>, analyzer: &str) -> Finding {
    let mut finding = Finding {
        analyzer: analyzer.to_owned(),
        rule: found.rule,
        severity: found.severity,
        message: found.message,
        file: found.file,
        line: found.line,
        resource: found.resource,
        document: String::new(),
        document_line: 0,
        url: found.url,
        accepted: false,
    };
    let known = |id: &str| subject.graph.resources.iter().any(|r| r.id == id);
    if finding.resource.is_empty() || !known(&finding.resource) {
        // The analyzer named nothing, or a name the document does not
        // have (a tool's own derived name): the compiled line decides.
        if let Some(id) = resource_at(subject, &finding.file, finding.line) {
            finding.resource = id;
        }
    }
    if let Some(resource) = subject
        .graph
        .resources
        .iter()
        .find(|r| r.id == finding.resource)
    {
        finding.document = resource.pos.file.clone();
        finding.document_line = resource.pos.line as u64;
    }
    finding
}

/// The resource a line of a compiled file belongs to: the last resource id
/// that appears as a whole word on or above that line. Emitters write a
/// resource's id into what they emit wherever the tool allows it, so this
/// finds the right one far more often than not, and a wrong guess only
/// changes which document line a finding points at.
fn resource_at(subject: &Subject<'_>, file: &str, line: u64) -> Option<String> {
    if file.is_empty() || line == 0 {
        return None;
    }
    let content = subject.artifact.files.get(file)?;
    let ids: Vec<&str> = subject
        .graph
        .resources
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    let lines: Vec<&str> = content.lines().take(line as usize).collect();
    lines.into_iter().rev().find_map(|text| {
        ids.iter()
            .filter(|id| contains_word(text, id))
            .max_by_key(|id| id.len())
            .map(|id| (*id).to_owned())
    })
}

/// Whether `word` appears in `text` with nothing of an identifier on
/// either side of it.
fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let part = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = text[..start].chars().next_back().is_none_or(|c| !part(c));
        let after = text[end..].chars().next().is_none_or(|c| !part(c));
        if before && after {
            return true;
        }
        from = end;
    }
    false
}

/// Findings a project has looked at and accepts. An accepted finding is
/// still reported, and does not fail the stage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    pub entries: Vec<Accepted>,
}

/// One accepted finding, or kind of finding. A field left empty matches
/// anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Accepted {
    pub analyzer: String,
    pub rule: String,
    pub resource: String,
    pub stage: String,
    pub target: String,
    /// Why it is accepted; shown with the finding.
    pub reason: String,
}

impl Baseline {
    /// Reads a baseline: a list under `accepted`, each entry an object with
    /// `rule` and any of `analyzer`, `resource`, `stage`, `target` and
    /// `reason`.
    pub fn from_value(value: &Value) -> Result<Baseline, String> {
        let Some(map) = value.as_map() else {
            return Err(format!(
                "a baseline is a mapping with `accepted`, and this is {}",
                value.describe()
            ));
        };
        if let Some(key) = map.keys().find(|k| *k != "accepted") {
            return Err(format!(
                "a baseline has only `accepted`, and this has {key:?}"
            ));
        }
        let Some(list) = map.get("accepted") else {
            return Ok(Baseline::default());
        };
        let Some(list) = list.as_list() else {
            return Err(format!(
                "`accepted` has to be a list, and it is {}",
                list.describe()
            ));
        };
        let mut entries = Vec::new();
        for (index, item) in list.iter().enumerate() {
            let Some(fields) = item.as_map() else {
                return Err(format!(
                    "entry {} of `accepted` is {}, and it has to be a mapping",
                    index + 1,
                    item.describe()
                ));
            };
            let text = |key: &str| -> Result<String, String> {
                match fields.get(key) {
                    None | Some(Value::Null) => Ok(String::new()),
                    Some(Value::String(text)) => Ok(text.clone()),
                    Some(other) => Err(format!(
                        "entry {} of `accepted` has `{key}` as {}, and it has to be a string",
                        index + 1,
                        other.describe()
                    )),
                }
            };
            if let Some(key) = fields.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "analyzer" | "rule" | "resource" | "stage" | "target" | "reason"
                )
            }) {
                return Err(format!(
                    "entry {} of `accepted` has {key:?}; the fields are analyzer, rule, \
                     resource, stage, target and reason",
                    index + 1
                ));
            }
            let entry = Accepted {
                analyzer: text("analyzer")?,
                rule: text("rule")?,
                resource: text("resource")?,
                stage: text("stage")?,
                target: text("target")?,
                reason: text("reason")?,
            };
            if entry.rule.is_empty() {
                return Err(format!("entry {} of `accepted` names no `rule`", index + 1));
            }
            entries.push(entry);
        }
        Ok(Baseline { entries })
    }

    /// The entry that accepts `finding` in stage `stage` on `target`, if
    /// one does.
    pub fn accepts(&self, finding: &Finding, stage: &str, target: &str) -> Option<&Accepted> {
        let fits = |want: &str, have: &str| want.is_empty() || want == have;
        self.entries.iter().find(|entry| {
            entry.rule == finding.rule
                && fits(&entry.analyzer, &finding.analyzer)
                && fits(&entry.resource, &finding.resource)
                && fits(&entry.stage, stage)
                && fits(&entry.target, target)
        })
    }

    /// Marks every finding of `analyses` the baseline accepts.
    pub fn apply(&self, analyses: &mut [Analysis], stage: &str, target: &str) {
        for analysis in analyses {
            for finding in &mut analysis.findings {
                finding.accepted = self.accepts(finding, stage, target).is_some();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_and_anything_else_is_refused() {
        let (findings, measures) = parse_answer(
            br#"{"findings": [{"rule": "R1", "severity": "HIGH", "message": "m", "file": "a.json", "line": 3, "resource": "x"}],
                "measures": [{"name": "monthly-cost", "value": 12.5, "unit": "USD"}]}"#,
        )
        .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].line, 3);
        assert_eq!(measures[0].value, 12.5);

        let (none, nothing) = parse_answer(b"{}").unwrap();
        assert!(none.is_empty() && nothing.is_empty());

        for (answer, says) in [
            ("", "is empty"),
            ("nope", "is not JSON"),
            ("[]", "has to be an object"),
            (r#"{"findings": "x"}"#, "has to be a list"),
            (r#"{"findings": [1]}"#, "has to be an object"),
            (
                r#"{"findings": [{"severity": "high"}]}"#,
                "without a `rule`",
            ),
            (
                r#"{"findings": [{"rule": "r", "severity": "huge"}]}"#,
                "not a severity",
            ),
            (
                r#"{"findings": [{"rule": "r", "line": "3"}]}"#,
                "has to be a number",
            ),
            (r#"{"measures": [{"name": "n"}]}"#, "without a `value`"),
            (r#"{"deny": []}"#, "only `findings` and `measures`"),
        ] {
            let error = parse_answer(answer.as_bytes()).unwrap_err();
            assert!(error.contains(says), "{answer:?} gave {error:?}");
        }
    }

    #[test]
    fn severities_order_and_read_the_scanners_words() {
        assert!(Severity::Critical > Severity::High && Severity::High > Severity::Info);
        assert_eq!("error".parse::<Severity>().unwrap(), Severity::High);
        assert_eq!("warning".parse::<Severity>().unwrap(), Severity::Medium);
        assert_eq!("UNKNOWN".parse::<Severity>().unwrap(), Severity::Info);
        assert_eq!("none".parse::<FailOn>().unwrap(), FailOn::Never);
        assert_eq!(
            "low".parse::<FailOn>().unwrap(),
            FailOn::AtLeast(Severity::Low)
        );
        assert!("loud".parse::<FailOn>().is_err());
        assert_eq!(FailOn::default().threshold(), Some(Severity::High));
    }

    #[test]
    fn a_setting_is_read_and_reaches_the_program_upper_cased() {
        let (name, key, value) = Analyzer::parse_setting("checkov.skip-check=CKV_1,CKV_2").unwrap();
        assert_eq!(
            (name.as_str(), key.as_str(), value.as_str()),
            ("checkov", "skip-check", "CKV_1,CKV_2")
        );
        assert_eq!(
            setting_variable("skip-check"),
            "MERIDIAN_ANALYZE_SKIP_CHECK"
        );
        assert!(Analyzer::parse_setting("checkov=x").is_err());
        assert!(Analyzer::parse_setting("checkov.=x").is_err());
    }

    #[test]
    fn a_word_is_found_only_whole() {
        assert!(contains_word(r#""logs": {"#, "logs"));
        assert!(!contains_word(r#""logs_bucket": {"#, "logs"));
        assert!(contains_word("aws_s3_bucket.logs.id", "logs"));
        assert!(!contains_word("", "logs"));
    }

    #[test]
    fn a_baseline_accepts_by_rule_and_the_fields_it_names() {
        let baseline = Baseline::from_value(
            &decode_value(
                br#"{"accepted": [{"rule": "R1", "resource": "logs", "reason": "public on purpose"},
                                 {"rule": "R2", "analyzer": "trivy"}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        let finding = |analyzer: &str, rule: &str, resource: &str| Finding {
            analyzer: analyzer.to_owned(),
            rule: rule.to_owned(),
            resource: resource.to_owned(),
            ..Finding::default()
        };
        assert!(baseline
            .accepts(&finding("checkov", "R1", "logs"), "site", "fake")
            .is_some());
        assert!(baseline
            .accepts(&finding("checkov", "R1", "other"), "site", "fake")
            .is_none());
        assert!(baseline
            .accepts(&finding("trivy", "R2", ""), "site", "fake")
            .is_some());
        assert!(baseline
            .accepts(&finding("checkov", "R2", ""), "site", "fake")
            .is_none());

        for (text, says) in [
            ("[]", "is a mapping"),
            (r#"{"accept": []}"#, "only `accepted`"),
            (r#"{"accepted": [{"resource": "x"}]}"#, "names no `rule`"),
            (
                r#"{"accepted": [{"rule": "r", "file": "x"}]}"#,
                "the fields are",
            ),
        ] {
            let error = Baseline::from_value(&decode_value(text.as_bytes()).unwrap()).unwrap_err();
            assert!(error.contains(says), "{text:?} gave {error:?}");
        }
    }

    #[test]
    fn counts_describe_themselves() {
        let analysis = Analysis {
            findings: vec![
                Finding {
                    severity: Severity::High,
                    ..Finding::default()
                },
                Finding {
                    severity: Severity::High,
                    accepted: true,
                    ..Finding::default()
                },
                Finding {
                    severity: Severity::Low,
                    ..Finding::default()
                },
            ],
            asked: true,
            ..Analysis::default()
        };
        assert_eq!(analysis.counts().describe(), "1 high, 1 low, 1 accepted");
        assert_eq!(analysis.failing(Some(Severity::High)).len(), 1);
        assert!(analysis.passed(Some(Severity::Critical)));
        assert!(!analysis.passed(Some(Severity::Low)));
        assert!(analysis.passed(None));
        assert_eq!(Counts::default().describe(), "none");
    }
}
