//! A report as a file: JSON, JUnit XML, Markdown or HTML.
//!
//! Every renderer reads the same [`Report`], so the four always agree. Text
//! that came from an event is escaped for the format it lands in, because a
//! step or a resource is named by whoever wrote the document.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use meridian_sdk::Value;

use crate::model::{explain, Phase, Report, RunState, Step, StepState};
use crate::time::human;

/// The name of the JSON report's shape. It changes only when a field is
/// removed or changes meaning; new fields are added without a new name.
pub const SCHEMA: &str = "meridian.report/v1";

/// A format a report can be written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// One JSON document, for programs.
    Json,
    /// JUnit XML, which nearly every CI system shows as test results: a
    /// step is a test case.
    Junit,
    /// Markdown, for a pull request comment or a CI job summary.
    Markdown,
    /// One self-contained HTML page, to keep as a build artifact.
    Html,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Json, Format::Junit, Format::Markdown, Format::Html];

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Junit => "junit",
            Format::Markdown => "markdown",
            Format::Html => "html",
        }
    }

    /// Reads a format's name. `md` and `xml` are accepted for the two
    /// formats people call by their file extension.
    pub fn parse(name: &str) -> Result<Format, String> {
        match name {
            "json" => Ok(Format::Json),
            "junit" | "xml" => Ok(Format::Junit),
            "markdown" | "md" => Ok(Format::Markdown),
            "html" => Ok(Format::Html),
            other => Err(format!(
                "{other:?} is not a report format; the formats are json, junit, markdown and html"
            )),
        }
    }
}

/// Renders `report` in `format`.
pub fn render(report: &Report, format: Format) -> String {
    match format {
        Format::Json => json(report),
        Format::Junit => junit(report),
        Format::Markdown => markdown(report),
        Format::Html => html(report),
    }
}

fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn duration(ms: Option<u64>) -> Value {
    ms.map_or(Value::Null, Value::Uint)
}

fn json(report: &Report) -> String {
    let text = |s: &str| Value::from(s);
    let summary = report.summary();
    let steps = report
        .steps
        .iter()
        .map(|step| {
            map([
                ("name", text(&step.name)),
                ("kind", text(&step.kind)),
                ("target", text(&step.target)),
                ("endpoint", text(&step.endpoint)),
                ("status", text(step.status.as_str())),
                ("reason", text(&step.reason)),
                ("artifact", text(&step.artifact)),
                ("started", text(&step.started)),
                ("finished", text(&step.finished)),
                ("durationMs", duration(step.duration_ms())),
                ("waitingOn", text(&step.waiting_on)),
                ("waitingFor", text(&step.waiting_for)),
                ("drift", text(&step.drift)),
                ("rollback", text(&step.rollback)),
                ("policy", text(&step.policy)),
                ("policyDenials", Value::Uint(step.policy_denials)),
                ("policyWarnings", Value::Uint(step.policy_warnings)),
                (
                    "resources",
                    Value::List(
                        step.resources
                            .iter()
                            .map(|r| {
                                map([
                                    ("id", text(&r.id)),
                                    ("action", text(&r.action)),
                                    ("phase", text(r.phase.as_str())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "outputs",
                    Value::List(
                        step.outputs
                            .iter()
                            .map(|o| {
                                map([
                                    ("name", text(&o.name)),
                                    ("sensitive", Value::Bool(o.sensitive)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    map([
        ("schema", text(SCHEMA)),
        ("run", text(&report.run)),
        ("mode", text(&report.mode)),
        ("command", text(&report.command)),
        ("status", text(report.status.as_str())),
        ("started", text(&report.started)),
        ("finished", text(&report.finished)),
        ("durationMs", duration(report.duration_ms())),
        ("resumes", text(&report.resumes)),
        ("rollsBack", text(&report.rolls_back)),
        ("user", text(&report.user)),
        ("host", text(&report.host)),
        ("selector", text(&report.selector)),
        ("events", Value::Uint(report.events)),
        (
            "summary",
            map([
                ("steps", Value::Uint(summary.steps)),
                ("succeeded", Value::Uint(summary.succeeded)),
                ("failed", Value::Uint(summary.failed)),
                ("skipped", Value::Uint(summary.skipped)),
                ("notRun", Value::Uint(summary.not_run)),
                ("deferred", Value::Uint(summary.deferred)),
                ("rolledBack", Value::Uint(summary.rolled_back)),
                ("running", Value::Uint(summary.running)),
                ("planned", Value::Uint(summary.planned)),
                ("changed", Value::Uint(summary.changed)),
                ("drifted", Value::Uint(summary.drifted)),
            ]),
        ),
        ("steps", Value::List(steps)),
    ])
    .pretty()
}

/// Escapes text for XML and HTML, in content and in a quoted attribute.
/// Characters XML 1.0 cannot carry at all are dropped.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Why a step that did not simply succeed ended as it did, in a sentence.
fn why(step: &Step) -> String {
    match step.status {
        StepState::Failed if step.policy == "denied" && step.policy_denials > 0 => format!(
            "failed: {} ({} denied; `meridian plan` shows which)",
            explain(&step.reason),
            rules(step.policy_denials)
        ),
        StepState::Failed => format!("failed: {}", explain(&step.reason)),
        StepState::Skipped => format!("skipped: {}", explain(&step.reason)),
        StepState::NotRun => format!("not run: the earlier step {} failed", step.waiting_on),
        StepState::Deferred => format!(
            "deferred: waits on the output {} of {}",
            step.waiting_for, step.waiting_on
        ),
        StepState::RolledBack => "put back to what was applied before".to_owned(),
        StepState::Running => "did not finish".to_owned(),
        StepState::Succeeded => match (step.drift.as_str(), step.rollback.as_str()) {
            ("detected", _) => "drifted: it no longer matches what was applied".to_owned(),
            ("unknown", _) => format!("drift unknown: {}", explain(&step.reason)),
            (_, "failed") => format!("could not be put back: {}", explain(&step.reason)),
            (_, "unavailable") => format!("nothing to put back: {}", explain(&step.reason)),
            _ if step.policy_warnings > 0 => {
                format!("the policy warned: {}", rules(step.policy_warnings))
            }
            _ => String::new(),
        },
    }
}

fn rules(n: u64) -> String {
    format!("{n} rule{}", if n == 1 { "" } else { "s" })
}

fn seconds(ms: Option<u64>) -> String {
    let ms = ms.unwrap_or(0);
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn resource_lines(step: &Step) -> String {
    let mut out = String::new();
    for resource in &step.resources {
        let _ = writeln!(
            out,
            "{} {} {}",
            resource.phase.as_str(),
            resource.action,
            resource.id
        );
    }
    for output in &step.outputs {
        let _ = writeln!(
            out,
            "output {}{}",
            output.name,
            if output.sensitive { " (sensitive)" } else { "" }
        );
    }
    out
}

fn junit(report: &Report) -> String {
    let failures = report.steps.iter().filter(|s| s.is_problem()).count();
    let skipped = report
        .steps
        .iter()
        .filter(|s| {
            matches!(
                s.status,
                StepState::Skipped | StepState::NotRun | StepState::Deferred
            )
        })
        .count();
    // A run that died leaves steps with no end. They are errors, not
    // failures: nothing said they failed, and nothing said they worked.
    let errors = report
        .steps
        .iter()
        .filter(|s| s.status == StepState::Running)
        .count();
    let counts = format!(
        "tests=\"{}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{}\"",
        report.steps.len(),
        seconds(report.duration_ms())
    );
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<testsuites name=\"meridian {}\" {counts}>",
        escape(&report.mode)
    );
    let _ = writeln!(
        out,
        "  <testsuite name=\"{}\" {counts} timestamp=\"{}\" hostname=\"{}\">",
        escape(&report.run),
        escape(&report.started),
        escape(&report.host)
    );
    out.push_str("    <properties>\n");
    for (name, value) in [
        ("meridian.run", report.run.as_str()),
        ("meridian.mode", report.mode.as_str()),
        ("meridian.status", report.status.as_str()),
        ("meridian.command", report.command.as_str()),
        ("meridian.resumes", report.resumes.as_str()),
        ("meridian.rollsBack", report.rolls_back.as_str()),
    ] {
        if !value.is_empty() {
            let _ = writeln!(
                out,
                "      <property name=\"{name}\" value=\"{}\"/>",
                escape(value)
            );
        }
    }
    out.push_str("    </properties>\n");
    for step in &report.steps {
        let _ = write!(
            out,
            "    <testcase classname=\"meridian.{}\" name=\"{}\" time=\"{}\"",
            escape(if step.target.is_empty() {
                "unknown"
            } else {
                &step.target
            }),
            escape(&step.title()),
            seconds(step.duration_ms())
        );
        let why = escape(&why(step));
        let mut body = String::new();
        if step.is_problem() {
            let kind = if step.status == StepState::Failed {
                step.reason.as_str()
            } else if step.drift == "detected" {
                "drift"
            } else {
                "rollback-failed"
            };
            let _ = writeln!(
                body,
                "      <failure type=\"{}\" message=\"{why}\"/>",
                escape(kind)
            );
        } else if step.status == StepState::Running {
            let _ = writeln!(body, "      <error type=\"unfinished\" message=\"{why}\"/>");
        } else if matches!(
            step.status,
            StepState::Skipped | StepState::NotRun | StepState::Deferred
        ) {
            let _ = writeln!(body, "      <skipped message=\"{why}\"/>");
        }
        let lines = resource_lines(step);
        if !lines.is_empty() {
            let _ = writeln!(body, "      <system-out>{}</system-out>", escape(&lines));
        }
        if body.is_empty() {
            out.push_str("/>\n");
        } else {
            out.push_str(">\n");
            out.push_str(&body);
            out.push_str("    </testcase>\n");
        }
    }
    out.push_str("  </testsuite>\n</testsuites>\n");
    out
}

/// The run in one line: `apply succeeded: 3 steps, 4 resources changed`.
pub fn headline(report: &Report) -> String {
    let summary = report.summary();
    let plural = |n: u64, noun: &str| format!("{n} {noun}{}", if n == 1 { "" } else { "s" });
    let mut parts = vec![plural(summary.steps, "step")];
    for (n, what) in [
        (summary.failed, "failed"),
        (summary.not_run, "not run"),
        (summary.deferred, "deferred"),
        (summary.skipped, "skipped"),
        (summary.rolled_back, "rolled back"),
        (summary.drifted, "drifted"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    if summary.changed > 0 {
        parts.push(format!("{} changed", plural(summary.changed, "resource")));
    }
    if summary.planned > 0 {
        parts.push(format!("{} to change", plural(summary.planned, "resource")));
    }
    let status = match report.status {
        RunState::Running => "did not finish",
        RunState::Succeeded => "succeeded",
        RunState::Failed => "failed",
    };
    format!(
        "{} {status}: {}",
        if report.mode.is_empty() {
            "run"
        } else {
            &report.mode
        },
        parts.join(", ")
    )
}

/// A status as a person reads it, with a mark that survives a plain-text
/// viewer.
fn status_word(step: &Step) -> String {
    let mark = if step.is_problem() {
        "✗"
    } else {
        match step.status {
            StepState::Succeeded | StepState::RolledBack => "✓",
            StepState::Running => "…",
            _ => "–",
        }
    };
    let word = if step.drift == "detected" {
        "drifted"
    } else {
        step.status.as_str()
    };
    format!("{mark} {word}")
}

fn change_count(step: &Step) -> String {
    let planned = step
        .resources
        .iter()
        .filter(|r| r.phase == Phase::Planned)
        .count();
    let changed = step.resources.len() - planned;
    match (planned, changed) {
        (0, 0) => "none".to_owned(),
        (p, 0) => format!("{p} planned"),
        (0, c) => format!("{c} changed"),
        (p, c) => format!("{c} changed, {p} planned"),
    }
}

/// The facts about the run, as label and value, leaving out what is empty.
fn facts(report: &Report) -> Vec<(&'static str, String)> {
    let mut facts = vec![
        ("Run", report.run.clone()),
        ("Command", report.command.clone()),
        ("Started", report.started.clone()),
        (
            "Duration",
            report.duration_ms().map(human).unwrap_or_default(),
        ),
        ("Resumes", report.resumes.clone()),
        ("Rolls back", report.rolls_back.clone()),
        ("Selector", report.selector.clone()),
    ];
    facts.push((
        "Run by",
        match (report.user.as_str(), report.host.as_str()) {
            ("", host) => host.to_owned(),
            (user, "") => user.to_owned(),
            (user, host) => format!("{user}@{host}"),
        },
    ));
    facts.retain(|(_, value)| !value.is_empty());
    facts
}

/// Escapes text for a Markdown table cell or a line of prose. Markdown
/// renderers also read HTML, so the HTML escapes come first.
fn md(text: &str) -> String {
    let mut out = String::new();
    for c in escape(text).chars() {
        match c {
            '|' | '`' | '*' | '_' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn markdown(report: &Report) -> String {
    let mut out = String::new();
    let mark = match report.status {
        RunState::Succeeded => "✓",
        RunState::Failed => "✗",
        RunState::Running => "…",
    };
    let _ = writeln!(out, "## {mark} Meridian {}\n", md(&headline(report)));
    for (label, value) in facts(report) {
        let _ = writeln!(out, "- **{label}:** {}", md(&value));
    }
    out.push_str(
        "\n| Step | Target | Endpoint | Status | Changes | Duration |\n|---|---|---|---|---|---|\n",
    );
    for step in &report.steps {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            md(&step.title()),
            md(&step.target),
            md(&step.endpoint),
            status_word(step),
            change_count(step),
            step.duration_ms().map(human).unwrap_or_default()
        );
    }
    let notes: Vec<(&Step, String)> = report
        .steps
        .iter()
        .map(|step| (step, why(step)))
        .filter(|(_, why)| !why.is_empty())
        .collect();
    if !notes.is_empty() {
        out.push_str("\n### What needs attention\n\n");
        for (step, why) in notes {
            let _ = writeln!(out, "- **{}** {}", md(&step.title()), md(&why));
        }
    }
    for step in &report.steps {
        if step.resources.is_empty() && step.outputs.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "\n<details><summary>{}: {}</summary>\n",
            escape(&step.title()),
            change_count(step)
        );
        for resource in &step.resources {
            let _ = writeln!(
                out,
                "- {} {} <code>{}</code>",
                resource.phase.as_str(),
                md(&resource.action),
                escape(&resource.id)
            );
        }
        for output in &step.outputs {
            let _ = writeln!(
                out,
                "- output <code>{}</code>{}",
                escape(&output.name),
                if output.sensitive { " (sensitive)" } else { "" }
            );
        }
        out.push_str("\n</details>\n");
    }
    if report.status == RunState::Failed && report.mode == "apply" {
        out.push_str(
            "\nOnce the cause is fixed, `meridian apply --resume` with the same documents \
             continues from the step that failed.\n",
        );
    }
    out.push_str(
        "\n<sub>Values are never in a report: it names steps, resources and outputs only.</sub>\n",
    );
    out
}

/// The style sheet of the HTML report, for a page that sits beside one.
pub const STYLE: &str = "\
:root{--bg:#fff;--fg:#1b1f24;--muted:#5b6470;--line:#d8dee4;--ok:#1a7f37;--bad:#cf222e;--idle:#6e7781;--card:#f6f8fa}\
@media(prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#9198a1;--line:#30363d;--ok:#3fb950;--bad:#f85149;--idle:#8b949e;--card:#161b22}}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}\
main{max-width:980px;margin:0 auto;padding:24px 16px}\
h1{font-size:22px;margin:0 0 4px}h2{font-size:17px;margin:28px 0 8px}\
.ok{color:var(--ok)}.bad{color:var(--bad)}.idle{color:var(--idle)}\
dl{display:grid;grid-template-columns:max-content 1fr;gap:2px 16px;margin:12px 0;color:var(--muted)}dd{margin:0;color:var(--fg);overflow-wrap:anywhere}\
.tiles{display:flex;flex-wrap:wrap;gap:8px;margin:16px 0}\
.tile{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:8px 14px;min-width:84px}\
.tile b{display:block;font-size:20px}.tile span{color:var(--muted);font-size:13px}\
.scroll{overflow-x:auto}table{border-collapse:collapse;width:100%}\
th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);vertical-align:top}\
th{color:var(--muted);font-weight:600;font-size:13px}\
code{font:13px ui-monospace,monospace;overflow-wrap:anywhere}\
details{margin:6px 0}summary{cursor:pointer}ul{margin:6px 0;padding-left:22px}\
footer{margin-top:32px;color:var(--muted);font-size:13px}";

fn html(report: &Report) -> String {
    let summary = report.summary();
    let class = match report.status {
        RunState::Succeeded => "ok",
        RunState::Failed => "bad",
        RunState::Running => "idle",
    };
    let mut out = String::from("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n");
    let _ = writeln!(
        out,
        "<title>Meridian {} {}</title>",
        escape(&report.mode),
        escape(&report.run)
    );
    let _ = writeln!(out, "<style>{STYLE}</style>\n</head>\n<body>\n<main>");
    let _ = writeln!(
        out,
        "<h1 class=\"{class}\">Meridian {}</h1>",
        escape(&headline(report))
    );
    out.push_str("<dl>\n");
    for (label, value) in facts(report) {
        let _ = writeln!(out, "<dt>{label}</dt><dd>{}</dd>", escape(&value));
    }
    out.push_str("</dl>\n<div class=\"tiles\">\n");
    for (n, label) in [
        (summary.steps, "steps"),
        (summary.succeeded, "succeeded"),
        (summary.failed, "failed"),
        (summary.not_run, "not run"),
        (summary.skipped, "skipped"),
        (summary.deferred, "deferred"),
        (summary.rolled_back, "rolled back"),
        (summary.drifted, "drifted"),
        (summary.changed, "changed"),
        (summary.planned, "planned"),
    ] {
        if n > 0 || label == "steps" {
            let _ = writeln!(
                out,
                "<div class=\"tile\"><b>{n}</b><span>{label}</span></div>"
            );
        }
    }
    out.push_str("</div>\n<h2>Steps</h2>\n<div class=\"scroll\"><table>\n<thead><tr><th>Step</th><th>Target</th><th>Endpoint</th><th>Status</th><th>Changes</th><th>Duration</th><th>Artifact</th></tr></thead>\n<tbody>\n");
    for step in &report.steps {
        let class = if step.is_problem() {
            "bad"
        } else if matches!(step.status, StepState::Succeeded | StepState::RolledBack) {
            "ok"
        } else {
            "idle"
        };
        let why = why(step);
        let hash = step
            .artifact
            .strip_prefix("sha256:")
            .unwrap_or(&step.artifact);
        let _ = writeln!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"{class}\">{}{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>",
            escape(&step.title()),
            escape(&step.target),
            escape(&step.endpoint),
            status_word(step),
            if why.is_empty() {
                String::new()
            } else {
                format!("<br><small>{}</small>", escape(&why))
            },
            change_count(step),
            step.duration_ms().map(human).unwrap_or_default(),
            escape(&hash[..hash.len().min(12)])
        );
    }
    out.push_str("</tbody>\n</table></div>\n");
    if report
        .steps
        .iter()
        .any(|s| !s.resources.is_empty() || !s.outputs.is_empty())
    {
        out.push_str("<h2>Resources and outputs</h2>\n");
    }
    for step in &report.steps {
        if step.resources.is_empty() && step.outputs.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "<details{}><summary>{}: {}</summary>\n<ul>",
            if step.is_problem() { " open" } else { "" },
            escape(&step.title()),
            change_count(step)
        );
        for resource in &step.resources {
            let _ = writeln!(
                out,
                "<li>{} {} <code>{}</code></li>",
                resource.phase.as_str(),
                escape(&resource.action),
                escape(&resource.id)
            );
        }
        for output in &step.outputs {
            let _ = writeln!(
                out,
                "<li>output <code>{}</code>{}</li>",
                escape(&output.name),
                if output.sensitive { " (sensitive)" } else { "" }
            );
        }
        out.push_str("</ul>\n</details>\n");
    }
    let _ = writeln!(
        out,
        "<footer>Folded from {} events. Values are never in a report: it names steps, resources and outputs only.</footer>\n</main>\n</body>\n</html>",
        report.events
    );
    out
}
