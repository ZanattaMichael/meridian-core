//! What a run says as it happens.
//!
//! Every change of state is one event, and events are the only way state is
//! recorded, so the terminal, the record and anything else that listens
//! always agree. The envelope is the one the language design fixes
//! (`docs/plans/run-safety.md`, section 2): until a `Workflow` kind exists a
//! step is a document, `instance` equals `step`, and `attempt` is 1.
//!
//! An event never holds a value a document or a tool produced. It names
//! things: a step, a resource id, an output. A tool's error text is shown on
//! the terminal and is not an event's to keep, because a tool can quote a
//! secret in one.

use std::collections::BTreeMap;

use meridian_sdk::{decode_map, Value};

pub const RUN_STARTED: &str = "run.started";
pub const RUN_SUCCEEDED: &str = "run.succeeded";
pub const RUN_FAILED: &str = "run.failed";
pub const STEP_STARTED: &str = "step.started";
pub const STEP_SUCCEEDED: &str = "step.succeeded";
pub const STEP_FAILED: &str = "step.failed";
pub const STEP_SKIPPED: &str = "step.skipped";
pub const STEP_DEFERRED: &str = "step.deferred";
pub const RESOURCE_PLANNED: &str = "resource.planned";
pub const RESOURCE_CHANGED: &str = "resource.changed";
pub const RESOURCE_FAILED: &str = "resource.failed";
pub const OUTPUT_PRODUCED: &str = "output.produced";
pub const DRIFT_DETECTED: &str = "drift.detected";
pub const DRIFT_NONE: &str = "drift.none";
pub const DRIFT_UNKNOWN: &str = "drift.unknown";
pub const ROLLBACK_STARTED: &str = "rollback.started";
pub const ROLLBACK_SUCCEEDED: &str = "rollback.succeeded";
pub const ROLLBACK_FAILED: &str = "rollback.failed";
pub const ROLLBACK_UNAVAILABLE: &str = "rollback.unavailable";
pub const POLICY_PASSED: &str = "policy.passed";
pub const POLICY_DENIED: &str = "policy.denied";
pub const ANALYSIS_PASSED: &str = "analysis.passed";
pub const ANALYSIS_FAILED: &str = "analysis.failed";
pub const FINDING_REPORTED: &str = "finding.reported";
pub const LOCK_ACQUIRED: &str = "lock.acquired";
pub const LOCK_RELEASED: &str = "lock.released";

/// One change of state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    /// The run it belongs to.
    pub run: String,
    /// Its place in the run, counted from 1 and strictly increasing.
    pub seq: u64,
    /// When, as RFC 3339 in UTC.
    pub time: String,
    /// What happened, such as `step.failed`.
    pub kind: String,
    /// The workflow; empty until the `Workflow` kind exists.
    pub workflow: String,
    /// The step, which today is the document's name; empty for an event
    /// about the run as a whole.
    pub step: String,
    /// The instance of the step; today the same as `step`.
    pub instance: String,
    /// The machine the step configures, when there is one.
    pub endpoint: String,
    /// The target the step's document selected.
    pub target: String,
    /// Which try this is, counted from 1; 0 for an event about the run.
    pub attempt: u32,
    /// What else there is to say. Its shape is fixed per kind.
    pub detail: BTreeMap<String, Value>,
}

impl Event {
    /// The event as one line of JSON, with `kind` spelled `type`.
    pub fn to_json(&self) -> String {
        let text = |s: &str| Value::from(s);
        let map: BTreeMap<String, Value> = [
            ("run", text(&self.run)),
            ("seq", Value::Uint(self.seq)),
            ("time", text(&self.time)),
            ("type", text(&self.kind)),
            ("workflow", text(&self.workflow)),
            ("step", text(&self.step)),
            ("instance", text(&self.instance)),
            ("endpoint", text(&self.endpoint)),
            ("target", text(&self.target)),
            ("attempt", Value::Uint(u64::from(self.attempt))),
            ("detail", Value::Map(self.detail.clone())),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
        meridian_sdk::canonical_map(&map)
    }

    /// Reads a line [`Event::to_json`] wrote.
    pub fn from_json(line: &str) -> Result<Event, String> {
        let map = decode_map(line.as_bytes())?;
        let text = |name: &str| match map.get(name) {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let number = |name: &str| match map.get(name) {
            Some(Value::Uint(n)) => *n,
            Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
            _ => 0,
        };
        let kind = text("type");
        if kind.is_empty() {
            return Err("the line has no event type".to_owned());
        }
        Ok(Event {
            run: text("run"),
            seq: number("seq"),
            time: text("time"),
            kind,
            workflow: text("workflow"),
            step: text("step"),
            instance: text("instance"),
            endpoint: text("endpoint"),
            target: text("target"),
            attempt: u32::try_from(number("attempt")).unwrap_or(0),
            detail: match map.get("detail") {
                Some(Value::Map(detail)) => detail.clone(),
                _ => BTreeMap::new(),
            },
        })
    }

    /// Whether the event's kind is one `pattern` asks for: the kind itself,
    /// a family such as `step.*`, or `*` for everything.
    pub fn matches(&self, pattern: &str) -> bool {
        match pattern.strip_suffix('*') {
            Some("") => true,
            Some(family) => family.ends_with('.') && self.kind.starts_with(family),
            None => self.kind == pattern,
        }
    }
}

/// Writes events as the lines of an `events.jsonl`.
pub fn to_jsonl(events: &[Event]) -> String {
    let mut out = String::new();
    for event in events {
        out.push_str(&event.to_json());
        out.push('\n');
    }
    out
}

/// Reads an `events.jsonl`. A line that cannot be read is left out: the
/// stream is a record, and one bad line should not hide the rest.
pub fn from_jsonl(text: &str) -> Vec<Event> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| Event::from_json(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Event {
        Event {
            run: "2026-11-17T09-14-03Z-7f3a".to_owned(),
            seq: 7,
            time: "2026-11-17T09:15:11.204Z".to_owned(),
            kind: STEP_FAILED.to_owned(),
            workflow: String::new(),
            step: "web".to_owned(),
            instance: "web".to_owned(),
            endpoint: "203.0.113.10".to_owned(),
            target: "ansible".to_owned(),
            attempt: 1,
            detail: [
                ("reason".to_owned(), Value::from("tool-failed")),
                ("changes".to_owned(), Value::Int(2)),
            ]
            .into(),
        }
    }

    #[test]
    fn an_event_is_one_line_with_a_fixed_envelope_and_reads_back() {
        let line = event().to_json();
        assert_eq!(
            line,
            "{\"attempt\":1,\"detail\":{\"changes\":2,\"reason\":\"tool-failed\"},\
             \"endpoint\":\"203.0.113.10\",\"instance\":\"web\",\
             \"run\":\"2026-11-17T09-14-03Z-7f3a\",\"seq\":7,\"step\":\"web\",\
             \"target\":\"ansible\",\"time\":\"2026-11-17T09:15:11.204Z\",\
             \"type\":\"step.failed\",\"workflow\":\"\"}"
        );
        assert_eq!(Event::from_json(&line).unwrap(), event());
    }

    #[test]
    fn a_stream_reads_back_and_skips_a_line_it_cannot_read() {
        let mut second = event();
        second.seq = 8;
        let text = format!(
            "{}not json\n\n{}",
            to_jsonl(&[event()]),
            to_jsonl(&[second.clone()])
        );
        assert_eq!(from_jsonl(&text), [event(), second]);
        assert!(Event::from_json("{}").is_err());
    }

    #[test]
    fn a_pattern_is_a_kind_a_family_or_everything() {
        let event = event();
        assert!(event.matches("step.failed"));
        assert!(event.matches("step.*"));
        assert!(event.matches("*"));
        assert!(!event.matches("step.started"));
        assert!(!event.matches("run.*"));
        assert!(!event.matches("st*"));
    }
}
