//! `meridian diagnose`: what broke in a run, where, and what to run next,
//! read from the run's record, its events and the failures this machine
//! kept. It runs no tool, loads no plugin and takes no lock.

use std::collections::BTreeMap;

use meridian_sdk::Value;

use crate::event::{self, Event};
use crate::record::{RunRecord, StepRecord, StepStatus};

/// From this many machines failing one step the same way, they are shown as
/// one group.
pub const GROUP_AT: usize = 3;

/// What `diagnose` was narrowed to.
#[derive(Clone, Copy, Debug, Default)]
pub struct Filter<'a> {
    pub step: Option<&'a str>,
    pub endpoint: Option<&'a str>,
    /// A cause word, or a step's reason word.
    pub cause: Option<&'a str>,
}

/// One resource a tool failed on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceFailure {
    pub endpoint: String,
    pub resource: String,
    pub cause: String,
    pub tool_code: String,
    /// Empty when this machine did not keep the run's failures: the record
    /// holds names and causes only.
    pub summary: String,
    /// Where the resource is declared, as `site.yaml:42`.
    pub at: String,
}

/// The machines of one step that failed, one entry per machine or, from
/// [`GROUP_AT`] machines failing the same way, one for all of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The cause word, or the step's reason when the tool named no resource.
    pub cause: String,
    /// The machines; empty when the step has none.
    pub endpoints: Vec<String>,
    /// The failures, of the first machine when grouped.
    pub failures: Vec<ResourceFailure>,
    /// The commands to run next, in the order to try them.
    pub next: Vec<String>,
}

/// A step that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedStep {
    pub step: StepRecord,
    pub entries: Vec<Entry>,
}

/// A step that was not run because an earlier one failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub step: StepRecord,
    /// The step that failed first; empty when the events are gone.
    pub because_of: String,
}

/// What `diagnose` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnosis {
    pub run: RunRecord,
    /// The failed steps, in the order they ran.
    pub failed: Vec<FailedStep>,
    pub skipped: Vec<Skipped>,
    /// Whether the failures' words came from this machine's
    /// `failures.jsonl`; when not, only names and causes are known.
    pub kept_here: bool,
}

/// Whether a run has something to diagnose.
pub fn has_failed(run: &RunRecord) -> bool {
    run.status == crate::record::RunStatus::Failed
        || run.steps.iter().any(|s| s.status == StepStatus::Failed)
}

/// Diagnoses `run` from its events and the failures this machine kept
/// (`logs::read_failures`).
pub fn diagnose(
    run: &RunRecord,
    events: &[Event],
    kept: &[BTreeMap<String, Value>],
    filter: Filter<'_>,
) -> Diagnosis {
    let text = |map: &BTreeMap<String, Value>, key: &str| match map.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    // Each step's failures: from this machine's file, or else from the
    // events, which hold the resource and the cause only.
    let mut failures: BTreeMap<(String, String), Vec<ResourceFailure>> = BTreeMap::new();
    let kept_here = !kept.is_empty();
    if kept_here {
        for f in kept {
            failures
                .entry((text(f, "kind"), text(f, "step")))
                .or_default()
                .push(ResourceFailure {
                    endpoint: text(f, "endpoint"),
                    resource: text(f, "resource"),
                    cause: text(f, "cause"),
                    tool_code: text(f, "toolCode"),
                    summary: text(f, "summary"),
                    at: text(f, "at"),
                });
        }
    } else {
        for e in events.iter().filter(|e| e.kind == event::RESOURCE_FAILED) {
            let kind = match e.detail.get("kind") {
                Some(Value::String(kind)) => kind.clone(),
                _ => String::new(),
            };
            failures
                .entry((kind, e.step.clone()))
                .or_default()
                .push(ResourceFailure {
                    endpoint: e.endpoint.clone(),
                    resource: text(&e.detail, "resource"),
                    cause: text(&e.detail, "cause"),
                    ..ResourceFailure::default()
                });
        }
    }
    let because_of: BTreeMap<&str, String> = events
        .iter()
        .filter(|e| e.kind == event::STEP_SKIPPED)
        .map(|e| (e.step.as_str(), text(&e.detail, "becauseOf")))
        .collect();

    let wanted_step = |step: &StepRecord| filter.step.is_none_or(|name| step.id.name == name);
    let mut failed = Vec::new();
    for step in run
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Failed && wanted_step(s))
    {
        let mut found: Vec<ResourceFailure> = failures
            .get(&(step.id.kind.as_str().to_owned(), step.id.name.clone()))
            .or_else(|| failures.get(&(String::new(), step.id.name.clone())))
            .cloned()
            .unwrap_or_default();
        for f in &mut found {
            if f.endpoint.is_empty() {
                f.endpoint.clone_from(&step.endpoint);
            }
        }
        found.retain(|f| {
            filter.endpoint.is_none_or(|e| f.endpoint == e)
                && filter
                    .cause
                    .is_none_or(|c| f.cause == c || step.reason == c)
        });
        let mut entries = entries_of(step, found);
        if entries.is_empty() {
            let wanted = filter.endpoint.is_none_or(|e| step.endpoint == e)
                && filter.cause.is_none_or(|c| step.reason == c);
            if !wanted {
                continue;
            }
            entries.push(Entry {
                cause: step.reason.clone(),
                endpoints: if step.endpoint.is_empty() {
                    Vec::new()
                } else {
                    vec![step.endpoint.clone()]
                },
                failures: Vec::new(),
                next: Vec::new(),
            });
        }
        for entry in &mut entries {
            entry.next = next(run, step, entry);
        }
        failed.push(FailedStep {
            step: step.clone(),
            entries,
        });
    }
    let skipped = run
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::NotRun)
        .map(|s| Skipped {
            step: s.clone(),
            because_of: because_of
                .get(s.id.name.as_str())
                .cloned()
                .unwrap_or_default(),
        })
        .collect();
    Diagnosis {
        run: run.clone(),
        failed,
        skipped,
        kept_here,
    }
}

/// One entry per machine, in the order the machines first failed; machines
/// that failed with the same single cause, [`GROUP_AT`] or more of them,
/// become one entry.
fn entries_of(step: &StepRecord, found: Vec<ResourceFailure>) -> Vec<Entry> {
    let mut machines: Vec<(String, Vec<ResourceFailure>)> = Vec::new();
    for f in found {
        match machines.iter_mut().find(|(e, _)| *e == f.endpoint) {
            Some((_, list)) => list.push(f),
            None => machines.push((f.endpoint.clone(), vec![f])),
        }
    }
    let single_cause = |list: &[ResourceFailure]| {
        let first = &list[0].cause;
        list.iter()
            .all(|f| &f.cause == first)
            .then(|| first.clone())
    };
    let mut by_cause: BTreeMap<String, usize> = BTreeMap::new();
    for (_, list) in &machines {
        if let Some(cause) = single_cause(list) {
            *by_cause.entry(cause).or_default() += 1;
        }
    }
    let mut entries: Vec<Entry> = Vec::new();
    // The entry each grouped cause gathers its machines in.
    let mut groups: BTreeMap<String, usize> = BTreeMap::new();
    for (endpoint, list) in machines {
        let endpoints: Vec<String> = if endpoint.is_empty() {
            Vec::new()
        } else {
            vec![endpoint]
        };
        if let Some(cause) = single_cause(&list).filter(|c| by_cause[c] >= GROUP_AT) {
            if let Some(&at) = groups.get(&cause) {
                entries[at].endpoints.extend(endpoints);
                continue;
            }
            groups.insert(cause.clone(), entries.len());
            entries.push(Entry {
                cause,
                endpoints,
                failures: list,
                next: Vec::new(),
            });
            continue;
        }
        let cause = single_cause(&list).unwrap_or_else(|| {
            if step.reason.is_empty() {
                list[0].cause.clone()
            } else {
                step.reason.clone()
            }
        });
        entries.push(Entry {
            cause,
            endpoints,
            failures: list,
            next: Vec::new(),
        });
    }
    entries
}

/// The commands to run next for one entry.
fn next(run: &RunRecord, step: &StepRecord, entry: &Entry) -> Vec<String> {
    let mut next = Vec::new();
    let mut logs = format!("meridian logs {} --step {}", run.id, step.id.name);
    if !step.endpoint.is_empty() {
        logs.push_str(&format!(" --endpoint {}", step.endpoint));
    }
    let typed = typed_again(&run.command);
    match entry.cause.as_str() {
        "unreachable" => next.push(format!(
            "check that {} can be reached at the address and port the step uses; then {logs}",
            names(entry)
        )),
        "auth-failed" => next.push(format!(
            "check the credential the step uses for {}; then {logs}",
            names(entry)
        )),
        "compile-refused" | "no-plugin" | "not-supported" => {
            if let Some(files) = files_of(&typed) {
                next.push(format!("meridian validate {files}"));
            }
        }
        _ => {
            next.push(logs);
            let rerun = matches!(step.reason.as_str(), "tool-failed" | "timed-out");
            if rerun && !typed.is_empty() {
                let mut again = format!(
                    "meridian {typed} --tool-debug --tool-debug-step {}",
                    step.id.name
                );
                if run.mode == "apply" {
                    again.push_str(&format!(" --resume={}", run.id));
                }
                next.push(again);
            }
        }
    }
    next
}

/// The machines of an entry.
fn names(entry: &Entry) -> String {
    match entry.endpoints.as_slice() {
        [] => "the machine".to_owned(),
        [one] => one.clone(),
        many => format!("the {} machines", many.len()),
    }
}

/// What was typed, without the words that asked for a resume or for the
/// tools' debug output: the command to give them again.
fn typed_again(command: &str) -> String {
    let mut words = command.split_whitespace();
    let mut kept = Vec::new();
    while let Some(word) = words.next() {
        if word == "--tool-debug-step" {
            words.next();
            continue;
        }
        if word == "--resume"
            || word.starts_with("--resume=")
            || word == "--tool-debug"
            || word.starts_with("--tool-debug=")
        {
            continue;
        }
        kept.push(word);
    }
    kept.join(" ")
}

/// The document files of a command: the words after the mode, up to the
/// first option.
fn files_of(typed: &str) -> Option<String> {
    let files: Vec<&str> = typed
        .split_whitespace()
        .skip(1)
        .take_while(|w| !w.starts_with('-'))
        .collect();
    (!files.is_empty()).then(|| files.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RunStatus;
    use crate::{StageId, StageKind};

    fn step(name: &str, endpoint: &str, status: StepStatus, reason: &str) -> StepRecord {
        StepRecord {
            id: StageId {
                kind: StageKind::ResourceSet,
                name: name.to_owned(),
            },
            target: "ansible".to_owned(),
            endpoint: endpoint.to_owned(),
            status,
            artifact: String::new(),
            changes: 0,
            reason: reason.to_owned(),
            previous: String::new(),
        }
    }

    fn run(steps: Vec<StepRecord>) -> RunRecord {
        RunRecord {
            id: "2026-11-18T09-14-03Z-0cc7".to_owned(),
            mode: "apply".to_owned(),
            command: "apply site.yaml --tool-debug=1 --tool-debug-step web".to_owned(),
            user: String::new(),
            host: String::new(),
            selector: String::new(),
            started: String::new(),
            finished: String::new(),
            status: RunStatus::Failed,
            resumes: String::new(),
            steps,
        }
    }

    fn kept(endpoint: &str, resource: &str, cause: &str, summary: &str) -> BTreeMap<String, Value> {
        [
            ("kind", "ResourceSet"),
            ("step", "web"),
            ("endpoint", endpoint),
            ("resource", resource),
            ("cause", cause),
            ("toolCode", ""),
            ("summary", summary),
            ("at", "site.yaml:42"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), Value::from(v)))
        .collect()
    }

    fn skipped_event(step: &str, because_of: &str) -> Event {
        Event {
            run: String::new(),
            seq: 0,
            time: String::new(),
            kind: event::STEP_SKIPPED.to_owned(),
            workflow: String::new(),
            step: step.to_owned(),
            instance: step.to_owned(),
            endpoint: String::new(),
            target: String::new(),
            attempt: 1,
            detail: [
                ("reason".to_owned(), Value::from("earlier-step-failed")),
                ("becauseOf".to_owned(), Value::from(because_of)),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn two_machines_failing_differently_are_two_entries_with_their_next_steps() {
        let run = run(vec![
            step("web", "", StepStatus::Failed, "tool-failed"),
            step("app", "", StepStatus::NotRun, ""),
            step("dns", "", StepStatus::NotRun, ""),
        ]);
        let kept = [
            kept(
                "203.0.113.10",
                "nginx_conf",
                "resource-failed",
                "template: 'nginx_port' is undefined",
            ),
            kept(
                "203.0.113.11",
                "",
                "unreachable",
                "ssh: connect to host 203.0.113.11 port 22: Connection timed out",
            ),
        ];
        let events = [skipped_event("app", "web"), skipped_event("dns", "web")];
        let d = diagnose(&run, &events, &kept, Filter::default());
        assert!(d.kept_here);
        let entries = &d.failed[0].entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].cause, "resource-failed");
        assert_eq!(entries[0].endpoints, ["203.0.113.10"]);
        assert_eq!(entries[0].failures[0].resource, "nginx_conf");
        assert_eq!(
            entries[0].next,
            [
                "meridian logs 2026-11-18T09-14-03Z-0cc7 --step web",
                "meridian apply site.yaml --tool-debug --tool-debug-step web --resume=2026-11-18T09-14-03Z-0cc7",
            ]
        );
        assert_eq!(entries[1].cause, "unreachable");
        assert!(entries[1].next[0].starts_with("check that 203.0.113.11 can be reached"));
        let skipped: Vec<(&str, &str)> = d
            .skipped
            .iter()
            .map(|s| (s.step.id.name.as_str(), s.because_of.as_str()))
            .collect();
        assert_eq!(skipped, [("app", "web"), ("dns", "web")]);
    }

    #[test]
    fn many_machines_failing_the_same_way_are_one_group() {
        let run = run(vec![step("web", "", StepStatus::Failed, "tool-failed")]);
        let mut kept: Vec<_> = (1..=5)
            .map(|n| kept(&format!("10.0.0.{n}"), "", "unreachable", "timed out"))
            .collect();
        kept.push(super::tests::kept(
            "10.0.0.9",
            "conf",
            "resource-failed",
            "bad",
        ));
        let d = diagnose(&run, &[], &kept, Filter::default());
        let entries = &d.failed[0].entries;
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0].cause, "unreachable");
        assert_eq!(entries[0].endpoints.len(), 5);
        assert!(entries[0].next[0].starts_with("check that the 5 machines can be reached"));
        assert_eq!(entries[1].endpoints, ["10.0.0.9"]);
    }

    #[test]
    fn filters_narrow_by_step_endpoint_and_cause() {
        let run = run(vec![step("web", "", StepStatus::Failed, "tool-failed")]);
        let kept = [
            kept("10.0.0.1", "a", "resource-failed", "x"),
            kept("10.0.0.2", "", "unreachable", "y"),
        ];
        let only = |filter: Filter<'_>| -> Vec<String> {
            diagnose(&run, &[], &kept, filter)
                .failed
                .iter()
                .flat_map(|s| s.entries.iter().map(|e| e.cause.clone()))
                .collect()
        };
        assert_eq!(
            only(Filter {
                cause: Some("unreachable"),
                ..Filter::default()
            }),
            ["unreachable"]
        );
        assert_eq!(
            only(Filter {
                endpoint: Some("10.0.0.1"),
                ..Filter::default()
            }),
            ["resource-failed"]
        );
        assert!(only(Filter {
            step: Some("app"),
            ..Filter::default()
        })
        .is_empty());
    }

    #[test]
    fn without_kept_failures_the_events_and_the_reason_still_answer() {
        let run = run(vec![step(
            "web",
            "10.0.0.5",
            StepStatus::Failed,
            "compile-refused",
        )]);
        let d = diagnose(&run, &[], &[], Filter::default());
        assert!(!d.kept_here);
        let entry = &d.failed[0].entries[0];
        assert_eq!(entry.cause, "compile-refused");
        assert_eq!(entry.endpoints, ["10.0.0.5"]);
        assert_eq!(entry.next, ["meridian validate site.yaml"]);

        let run = super::tests::run(vec![step(
            "web",
            "10.0.0.5",
            StepStatus::Failed,
            "tool-failed",
        )]);
        let mut failed = skipped_event("web", "");
        failed.kind = event::RESOURCE_FAILED.to_owned();
        failed.endpoint = "10.0.0.5".to_owned();
        failed.detail = [
            ("resource".to_owned(), Value::from("banner")),
            ("cause".to_owned(), Value::from("resource-failed")),
        ]
        .into_iter()
        .collect();
        let d = diagnose(&run, &[failed], &[], Filter::default());
        let entry = &d.failed[0].entries[0];
        assert_eq!(entry.failures[0].resource, "banner");
        assert_eq!(entry.failures[0].summary, "");
        assert_eq!(
            entry.next[0],
            "meridian logs 2026-11-18T09-14-03Z-0cc7 --step web --endpoint 10.0.0.5"
        );
    }
}
