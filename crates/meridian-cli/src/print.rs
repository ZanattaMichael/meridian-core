//! Printing: the guarded stdout the whole command writes through, and how
//! a run, a stage and a rollback are shown as they happen.

use super::*;

/// Where results are printed. It is the process's stdout until a write fails;
/// from then on everything written to it is dropped, so a command keeps
/// running to its own end and exits with its own status.
///
/// A reader that closed the pipe (`| head`) is the usual cause, and is no
/// fault of the command: nothing is said. Any other failure, such as a full
/// disk behind a redirect, is reported once on stderr and fails the command.
pub(crate) struct Stdout;

pub(crate) static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);
pub(crate) static STDOUT_FAILED: AtomicBool = AtomicBool::new(false);

impl Stdout {
    pub(crate) fn line(mut self, args: std::fmt::Arguments<'_>) {
        let _ = self.write_fmt(args).and_then(|()| self.write_all(b"\n"));
    }

    pub(crate) fn text(mut self, args: std::fmt::Arguments<'_>) {
        let _ = self.write_fmt(args);
    }

    /// Whether a write to stdout failed for a reason other than a closed
    /// pipe, which the command's exit status must say.
    pub(crate) fn failed() -> bool {
        STDOUT_FAILED.load(Ordering::Relaxed)
    }

    fn note(error: std::io::Error) {
        if STDOUT_CLOSED.swap(true, Ordering::Relaxed) {
            return;
        }
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            STDOUT_FAILED.store(true, Ordering::Relaxed);
            eprintln!("error: writing to standard output: {error}");
        }
    }
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if STDOUT_CLOSED.load(Ordering::Relaxed) {
            return Ok(buf.len());
        }
        match std::io::stdout().lock().write(buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                Stdout::note(e);
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if STDOUT_CLOSED.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Err(e) = std::io::stdout().lock().flush() {
            Stdout::note(e);
        }
        Ok(())
    }
}

pub(crate) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

pub(crate) fn print_run(run: &RunRecord, holds_lock: bool) {
    let status = match (run.status, holds_lock) {
        (record::RunStatus::Running, false) => {
            "interrupted: it never said how it ended, and it does not hold the lock"
        }
        (status, _) => status.as_str(),
    };
    outln!("run {}: {}, {status}", run.id, run.mode);
    if !run.command.is_empty() {
        outln!("  command:  meridian {}", run.command);
    }
    let by = match (run.user.as_str(), run.host.as_str()) {
        ("", "") => String::new(),
        (user, host) => format!(
            " by {}@{}",
            if user.is_empty() { "?" } else { user },
            if host.is_empty() { "?" } else { host }
        ),
    };
    outln!("  started:  {}{by}", run.started);
    if !run.finished.is_empty() {
        outln!("  finished: {}", run.finished);
    }
    if !run.resumes.is_empty() {
        outln!("  resumes:  {}", run.resumes);
    }
    for step in &run.steps {
        let on = if step.endpoint.is_empty() {
            String::new()
        } else {
            format!(" on {}", step.endpoint)
        };
        let mut line = format!(
            "  {} ({}, target {}{on}): {}",
            step.id.name,
            step.id.kind.as_str(),
            step.target,
            step.status.as_str()
        );
        if !step.reason.is_empty() {
            line.push_str(&format!(" ({})", step.reason));
        }
        if step.changes > 0 {
            line.push_str(&format!(", {}", count(step.changes as usize, "change")));
        }
        outln!("{line}");
    }
}

/// Prints a run as it happens.
pub(crate) struct Printer<'a> {
    pub(crate) mode: Mode,
    pub(crate) out: Option<&'a Path>,
    /// Whether standard output carries the events, which moves the report
    /// for people to standard error.
    pub(crate) events: bool,
    /// Whether standard output carries the one JSON result, which moves the
    /// report for people to standard error too.
    pub(crate) json: bool,
    /// Every event so far, which is what the run's reports are made from.
    pub(crate) seen: Vec<Event>,
    /// Where the events are sent as they happen.
    pub(crate) sinks: Dispatcher,
}

impl Observer for Printer<'_> {
    fn log(&mut self, id: &StageId, line: &LogLine) {
        // The tools' own chatter goes to stderr, so stdout stays a clean
        // record of what Meridian found and did.
        let mark = match line.stream {
            LogStream::Stdout => "|",
            LogStream::Stderr => "!",
        };
        eprintln!("  {} {mark} {}", id.name, line.text);
    }

    fn stage_finished(&mut self, report: &StageReport) {
        if self.events || self.json {
            let _ = self.print(&mut std::io::stderr().lock(), report);
        } else {
            let _ = self.print(&mut Stdout, report);
        }
    }

    fn event(&mut self, event: &Event) {
        if self.events {
            let _ = writeln!(Stdout, "{}", event.to_json());
        }
        self.sinks.send(event);
        self.seen.push(event.clone());
    }

    fn rollback_finished(&mut self, report: &RollbackReport) {
        if self.events || self.json {
            let _ = print_rollback(&mut std::io::stderr().lock(), report);
        } else {
            let _ = print_rollback(&mut Stdout, report);
        }
    }
}

/// The first characters of an artifact hash, enough to tell two apart.
pub(crate) fn short(hash: &str) -> &str {
    let hex = hash.strip_prefix("sha256:").unwrap_or(hash);
    &hex[..hex.len().min(12)]
}

pub(crate) fn print_rollback(out: &mut dyn Write, report: &RollbackReport) -> std::io::Result<()> {
    let on = if report.host.is_empty() {
        String::new()
    } else {
        format!(" on {}", report.host)
    };
    writeln!(
        out,
        "rolling back {} ({}, target {}{on})",
        report.id.name,
        report.id.kind.as_str(),
        report.target
    )?;
    match &report.outcome {
        RollbackOutcome::RolledBack { to, changes } => {
            for change in changes {
                writeln!(out, "  {}", change_line(change))?;
            }
            writeln!(
                out,
                "  rolled back to what was applied before (artifact {}), {}",
                short(to),
                count(changes.len(), "change")
            )
        }
        RollbackOutcome::Unchanged => writeln!(
            out,
            "  nothing to put back: the run applied what was already applied"
        ),
        RollbackOutcome::Unavailable(why) => writeln!(out, "  NOT ROLLED BACK: {why}"),
        RollbackOutcome::Failed(why) => {
            let mut lines = why.lines();
            writeln!(
                out,
                "  ROLLBACK FAILED: {}",
                lines.next().unwrap_or_default()
            )?;
            for line in lines {
                writeln!(out, "    {line}")?;
            }
            Ok(())
        }
        RollbackOutcome::NotAttempted { because_of } => writeln!(
            out,
            "  not attempted, because rolling back {because_of} failed"
        ),
    }
}

/// What one analyzer said: its counts, then each finding on a line of its
/// own with where it is, in the document first.
fn print_analysis(out: &mut dyn Write, analysis: &Analysis) -> std::io::Result<()> {
    if !analysis.asked {
        // The stage's failure says why; here only that it did not look.
        writeln!(out, "  analysis ({}): could not look", analysis.analyzer)?;
        return Ok(());
    }
    writeln!(
        out,
        "  analysis ({}): {}",
        analysis.analyzer,
        analysis.counts().describe()
    )?;
    for finding in &analysis.findings {
        let place = finding.place();
        writeln!(
            out,
            "    {}{} {}{}: {}",
            if finding.accepted { "accepted " } else { "" },
            finding.severity,
            finding.rule,
            if place.is_empty() {
                String::new()
            } else {
                format!(" at {place}")
            },
            finding.message
        )?;
    }
    for measure in &analysis.measures {
        writeln!(
            out,
            "    {}: {}{}",
            measure.name,
            measure.value,
            if measure.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", measure.unit)
            }
        )?;
    }
    Ok(())
}

impl Printer<'_> {
    fn print(&self, out: &mut dyn Write, report: &StageReport) -> std::io::Result<()> {
        let on = if report.host.is_empty() {
            String::new()
        } else if report.credential.is_empty() {
            format!(" on {}", report.host)
        } else {
            // Which identity touches the machine is part of what a plan
            // shows, so it can be reviewed.
            format!(" on {} as {}", report.host, report.credential)
        };
        writeln!(
            out,
            "{} ({}, target {}{on})",
            report.id.name,
            report.id.kind.as_str(),
            report.target
        )?;
        for warning in &report.warnings {
            writeln!(out, "  warning: {warning}")?;
        }
        if let Some(policy) = &report.policy {
            for warning in &policy.warn {
                writeln!(out, "  policy warning: {warning}")?;
            }
            if policy.allowed() {
                writeln!(out, "  policy: allowed")?;
            }
        }
        for analysis in &report.analyses {
            print_analysis(out, analysis)?;
        }
        // Every decision a condition made, so that something missing from
        // the compiled output is never a surprise.
        if !report.removed.is_empty() {
            writeln!(out, "  when: {} removed", report.removed.len())?;
            for removed in &report.removed {
                writeln!(
                    out,
                    "    {}: {} is false",
                    removed.subject, removed.condition
                )?;
            }
        }

        match &report.outcome {
            Outcome::Compiled => {
                let files = report.artifact.as_ref().map(|a| a.files.len()).unwrap_or(0);
                match (self.mode, self.out) {
                    (Mode::Compile, Some(dir)) => {
                        let dir = dir.join(&report.id.name);
                        let written = if meridian_ir::is_document_name(&report.id.name) {
                            write_artifact(&dir, report)
                        } else {
                            // Validation refuses such a name; this is the
                            // last line of defence for the output directory.
                            Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                format!("{:?} is not a safe directory name", report.id.name),
                            ))
                        };
                        match written {
                            Ok(()) => writeln!(
                                out,
                                "  compiled {} into {}",
                                count(files, "file"),
                                dir.display()
                            )?,
                            Err(e) => writeln!(out, "  error: writing {}: {e}", dir.display())?,
                        }
                    }
                    (Mode::Compile, None) => {
                        writeln!(out, "  compiled {}", count(files, "file"))?;
                        if let Some(artifact) = &report.artifact {
                            for path in artifact.files.keys() {
                                writeln!(out, "    {path}")?;
                            }
                        }
                    }
                    _ => writeln!(
                        out,
                        "  compiled {}; this target only compiles, so there is no preview",
                        count(files, "file")
                    )?,
                }
            }
            Outcome::Planned(plan) => {
                for change in &plan.changes {
                    writeln!(out, "  {}", change_line(change))?;
                }
                if plan.unknown {
                    writeln!(out, "  this tool cannot preview; applying may change things")?;
                } else if plan.changes.is_empty() {
                    writeln!(out, "  in sync, nothing to change")?;
                } else {
                    writeln!(out, "  {} to make", count(plan.changes.len(), "change"))?;
                }
            }
            Outcome::Applied { changes, outputs } => {
                for change in changes {
                    writeln!(out, "  {}", change_line(change))?;
                }
                if changes.is_empty() {
                    writeln!(out, "  applied, nothing needed changing")?;
                } else {
                    writeln!(out, "  applied {}", count(changes.len(), "change"))?;
                }
                for output in outputs {
                    writeln!(out, "  output {} = {}", output.name, shown(output))?;
                }
            }
            Outcome::Deferred { waiting_on, output } => writeln!(
                out,
                "  deferred: needs the output {output:?} of {waiting_on}, which has not been applied yet"
            )?,
            Outcome::Drift(drift) => match drift {
                Drift::NeverApplied => writeln!(out, "  never applied, so nothing to compare")?,
                Drift::InSync => writeln!(out, "  in sync with what was applied")?,
                Drift::Unknown => writeln!(
                    out,
                    "  unknown: this tool cannot preview, so it cannot say whether anything \
                     drifted"
                )?,
                Drift::Drifted(changes) => {
                    for change in changes {
                        writeln!(out, "  {}", change_line(change))?;
                    }
                    writeln!(
                        out,
                        "  DRIFTED: {} from what was applied",
                        count(changes.len(), "difference")
                    )?;
                }
                Drift::Pending {
                    changes,
                    last_apply_failed,
                } => {
                    for change in changes {
                        writeln!(out, "  {}", change_line(change))?;
                    }
                    let why = if *last_apply_failed {
                        "the last apply failed part way"
                    } else {
                        "the document has changed since the last apply"
                    };
                    writeln!(
                        out,
                        "  pending: {why}, so {} waiting to be applied and cannot be told \
                         from drift",
                        match changes.len() {
                            0 => "nothing is".to_owned(),
                            1 => "1 change is".to_owned(),
                            n => format!("{n} changes are"),
                        }
                    )?;
                }
            },
            Outcome::Skipped { applied_in } => writeln!(
                out,
                "  already applied by run {applied_in}, and unchanged since; left alone"
            )?,
            Outcome::LeftOut { condition } => {
                writeln!(out, "  skipped: its when ({condition}) is false")?
            }
            Outcome::Failed(why) => {
                // What the tool had already done is part of what happened.
                for change in &report.done_before_failing {
                    writeln!(out, "  {}", change_line(change))?;
                }
                // The first line beside the label, the rest indented under it.
                let mut lines = why.lines();
                writeln!(out, "  FAILED: {}", lines.next().unwrap_or_default())?;
                for line in lines {
                    writeln!(out, "    {line}")?;
                }
            }
            Outcome::NotRun { because_of } => {
                writeln!(out, "  not run, because {because_of} failed")?
            }
        }
        Ok(())
    }
}

pub(crate) fn change_line(change: &meridian_sdk::Change) -> String {
    let sign = match change.action {
        ChangeAction::Create => '+',
        ChangeAction::Update => '~',
        ChangeAction::Delete => '-',
        ChangeAction::Replace => '!',
    };
    format!("{sign} {change}")
}

/// An output's value as shown to a person: never a sensitive one.
pub(crate) fn shown(output: &Output) -> String {
    if output.sensitive {
        MASK.to_owned()
    } else {
        output.value.canonical()
    }
}

/// Writes a compiled artifact under `dir`, refusing any path that would land
/// outside it. An artifact's paths come from a plugin, and a plugin is not
/// trusted to choose where on the operator's disk a file goes.
pub(crate) fn write_artifact(dir: &Path, report: &StageReport) -> std::io::Result<()> {
    let Some(artifact) = &report.artifact else {
        return Ok(());
    };
    for (relative, content) in &artifact.files {
        let path = Path::new(relative);
        let safe = !relative.is_empty()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !safe {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("the plugin produced the unsafe path {relative:?}"),
            ));
        }
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    Ok(())
}
