//! A report is a fold over events: these build streams by hand and check
//! what comes out, in every format.

use std::collections::BTreeMap;

use meridian_orchestrator::Event;
use meridian_report::sink::{settings_by_sink, specs, SinkSpec};
use meridian_report::{render, Format, Report, RunState, StepState};
use meridian_sdk::Value;

/// A stream under construction: it numbers and times the events.
struct Stream(Vec<Event>);

impl Stream {
    fn new() -> Stream {
        Stream(Vec::new())
    }

    fn run(mut self, kind: &str, detail: &[(&str, Value)]) -> Stream {
        self.push(kind, "", "", detail);
        self
    }

    fn step(mut self, kind: &str, step: &str, detail: &[(&str, Value)]) -> Stream {
        self.push(kind, step, "ansible", detail);
        self
    }

    fn push(&mut self, kind: &str, step: &str, target: &str, detail: &[(&str, Value)]) {
        let seq = self.0.len() as u64 + 1;
        let mut detail: BTreeMap<String, Value> = detail
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        if !step.is_empty() {
            detail.insert("kind".to_owned(), Value::from("ResourceSet"));
        }
        self.0.push(Event {
            run: "2026-11-17T09-14-03Z-7f3a".to_owned(),
            seq,
            // One second apart, so durations are easy to read.
            time: format!("2026-11-17T09:14:{:02}.000Z", seq),
            kind: kind.to_owned(),
            workflow: String::new(),
            step: step.to_owned(),
            instance: step.to_owned(),
            endpoint: if step.is_empty() { "" } else { "web01" }.to_owned(),
            target: target.to_owned(),
            attempt: u32::from(!step.is_empty()),
            detail,
        });
    }
}

fn text(s: &str) -> Value {
    Value::from(s)
}

fn failed_apply() -> Vec<Event> {
    Stream::new()
        .run("lock.acquired", &[])
        .run(
            "run.started",
            &[
                ("mode", text("apply")),
                ("command", text("apply site.yaml")),
            ],
        )
        .step("step.started", "base", &[])
        .step(
            "resource.changed",
            "base",
            &[
                ("resource", text("sshd_config")),
                ("action", text("update")),
            ],
        )
        .step(
            "output.produced",
            "base",
            &[("name", text("token")), ("sensitive", Value::Bool(true))],
        )
        .step(
            "step.succeeded",
            "base",
            &[("artifact", text("sha256:abc"))],
        )
        .step("step.started", "app", &[])
        .step("step.failed", "app", &[("reason", text("tool-failed"))])
        .step(
            "step.skipped",
            "smoke",
            &[
                ("reason", text("earlier-step-failed")),
                ("becauseOf", text("app")),
            ],
        )
        .run("run.failed", &[])
        .run("lock.released", &[])
        .0
}

#[test]
fn a_report_is_folded_from_events_one_at_a_time_or_all_at_once() {
    let events = failed_apply();
    let report = Report::from_events(&events);
    assert_eq!(report.run, "2026-11-17T09-14-03Z-7f3a");
    assert_eq!(report.mode, "apply");
    assert_eq!(report.status, RunState::Failed);
    assert_eq!(report.duration_ms(), Some(8000));
    assert_eq!(report.events, 11);
    let states: Vec<(&str, StepState)> = report
        .steps
        .iter()
        .map(|s| (s.name.as_str(), s.status))
        .collect();
    assert_eq!(
        states,
        [
            ("base", StepState::Succeeded),
            ("app", StepState::Failed),
            ("smoke", StepState::NotRun)
        ]
    );
    assert_eq!(report.steps[0].duration_ms(), Some(3000));
    assert_eq!(report.steps[0].artifact, "sha256:abc");
    assert_eq!(report.steps[0].endpoint, "web01");
    assert_eq!(report.steps[1].reason, "tool-failed");
    assert_eq!(report.steps[2].waiting_on, "app");
    let summary = report.summary();
    assert_eq!(
        (
            summary.steps,
            summary.succeeded,
            summary.failed,
            summary.not_run,
            summary.changed
        ),
        (3, 1, 1, 1, 1)
    );

    // Folding as the events arrive gives the same report, and a report made
    // part way says the run is still going.
    let mut live = Report::default();
    for (n, event) in events.iter().enumerate() {
        live.apply(event);
        if n == 6 {
            assert_eq!(live.status, RunState::Running);
            assert_eq!(live.steps[1].status, StepState::Running);
        }
    }
    assert_eq!(live, report);
}

#[test]
fn an_event_of_an_unknown_kind_is_counted_and_changes_nothing_else() {
    let mut events = failed_apply();
    let known = Report::from_events(&events);
    let mut newer = Stream(std::mem::take(&mut events))
        .run("gate.waiting", &[("gate", text("approve"))])
        .0;
    // An unknown kind about a step it has seen leaves the step as it was.
    let mut odd = newer[3].clone();
    odd.kind = "resource.teleported".to_owned();
    newer.push(odd);
    let report = Report::from_events(&newer);
    assert_eq!(report.events, known.events + 2);
    assert_eq!(report.steps, known.steps);
    assert_eq!(report.status, known.status);
}

#[test]
fn a_drift_check_and_a_rollback_are_reported_for_what_they_are() {
    let drift = Stream::new()
        .run("run.started", &[("mode", text("drift"))])
        .step("step.started", "base", &[])
        .step(
            "resource.planned",
            "base",
            &[
                ("resource", text("sshd_config")),
                ("action", text("update")),
            ],
        )
        .step("drift.detected", "base", &[])
        .step("step.succeeded", "base", &[])
        .step("step.started", "app", &[])
        .step("drift.unknown", "app", &[("reason", text("never-applied"))])
        .step("step.succeeded", "app", &[])
        .run("run.succeeded", &[])
        .0;
    let report = Report::from_events(&drift);
    assert_eq!(report.summary().drifted, 1);
    let junit = render(&report, Format::Junit);
    assert!(junit.contains("tests=\"2\" failures=\"1\""), "{junit}");
    assert!(
        junit.contains(
            "<failure type=\"drift\" message=\"drifted: it no longer matches what was applied\"/>"
        ),
        "{junit}"
    );
    let markdown = render(&report, Format::Markdown);
    assert!(
        markdown
            .starts_with("## ✓ Meridian drift succeeded: 2 steps, 1 drifted, 1 resource to change"),
        "{markdown}"
    );
    assert!(markdown.contains("| ✗ drifted | 1 planned |"), "{markdown}");
    assert!(
        markdown.contains(
            "- **ResourceSet app** drift unknown: never applied, so there is nothing to compare"
        ),
        "{markdown}"
    );

    let rollback = Stream::new()
        .run(
            "run.started",
            &[("mode", text("rollback")), ("rollsBack", text("r1"))],
        )
        .step("rollback.started", "app", &[])
        .step("rollback.succeeded", "app", &[])
        .step("rollback.started", "base", &[])
        .step(
            "rollback.failed",
            "base",
            &[("reason", text("tool-failed"))],
        )
        .run("run.failed", &[])
        .0;
    let report = Report::from_events(&rollback);
    assert_eq!(report.rolls_back, "r1");
    assert_eq!(report.steps[0].status, StepState::RolledBack);
    assert_eq!(report.steps[1].rollback, "failed");
    assert!(report.steps[1].is_problem());
    assert_eq!(report.summary().rolled_back, 1);
}

#[test]
fn a_run_that_died_reports_as_unfinished_and_not_as_passed() {
    let events: Vec<Event> = failed_apply().into_iter().take(7).collect();
    let report = Report::from_events(&events);
    assert_eq!(report.status, RunState::Running);
    let junit = render(&report, Format::Junit);
    assert!(junit.contains("failures=\"0\" errors=\"1\""), "{junit}");
    assert!(
        junit.contains("<error type=\"unfinished\" message=\"did not finish\"/>"),
        "{junit}"
    );
    assert!(render(&report, Format::Markdown).starts_with("## … Meridian apply did not finish: "),);
    assert!(render(&report, Format::Json).contains("\"durationMs\": null"));
}

#[test]
fn names_a_document_chose_cannot_break_out_of_a_report() {
    let hostile = "</td><script>alert(1)</script> | `x` \u{1}";
    let events = Stream::new()
        .run(
            "run.started",
            &[("mode", text("apply")), ("command", text(hostile))],
        )
        .step("step.started", hostile, &[])
        .step(
            "resource.changed",
            hostile,
            &[("resource", text(hostile)), ("action", text("create"))],
        )
        .step("step.failed", hostile, &[("reason", text(hostile))])
        .run("run.failed", &[])
        .0;
    let report = Report::from_events(&events);
    for format in [Format::Junit, Format::Markdown, Format::Html] {
        let out = render(&report, format);
        assert!(!out.contains("<script>"), "{format:?}:\n{out}");
        assert!(!out.contains('\u{1}'), "{format:?}:\n{out}");
    }
    let markdown = render(&report, Format::Markdown);
    // A pipe in a name does not add a column to the table.
    let row = markdown
        .lines()
        .find(|line| line.starts_with("| ResourceSet"))
        .unwrap();
    assert_eq!(row.matches(" | ").count(), 5, "{row}");
    // JSON carries the name as it is, escaped as JSON.
    assert!(render(&report, Format::Json).contains("alert(1)"));
}

#[test]
fn every_format_has_a_name_and_reads_back() {
    for format in Format::ALL {
        assert_eq!(Format::parse(format.as_str()), Ok(format));
    }
    assert_eq!(Format::parse("md"), Ok(Format::Markdown));
    assert_eq!(Format::parse("xml"), Ok(Format::Junit));
    assert!(Format::parse("pdf").is_err());
}

fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn a_sink_keeps_meridians_settings_and_passes_on_its_own() {
    let spec = SinkSpec::find(
        "command",
        &[],
        settings(&[
            ("program", "/bin/cat"),
            ("on", "run.*, step.failed"),
            ("required", "true"),
            ("url", "https://example.com"),
        ]),
    )
    .unwrap();
    assert_eq!(spec.program.to_str(), Some("/bin/cat"));
    assert_eq!(spec.on, ["run.*", "step.failed"]);
    assert!(spec.required);
    // Only the sink's own setting is left to hand to its program.
    assert_eq!(spec.settings, settings(&[("url", "https://example.com")]));
    let event = |kind: &str| Event {
        kind: kind.to_owned(),
        ..Event::default()
    };
    assert!(spec.wants(&event("run.failed")));
    assert!(spec.wants(&event("step.failed")));
    assert!(!spec.wants(&event("step.started")));

    // By default a sink is sent everything and is not required.
    let plain = SinkSpec::find("command", &[], settings(&[("program", "/bin/cat")])).unwrap();
    assert_eq!(plain.on, ["*"]);
    assert!(!plain.required);

    // A setting's value can be a credential: debug output names it only.
    let secret = SinkSpec::find(
        "command",
        &[],
        settings(&[("program", "/bin/cat"), ("secret", "hunter2")]),
    )
    .unwrap();
    assert!(!format!("{secret:?}").contains("hunter2"));

    assert_eq!(
        meridian_report::sink::setting_variable("api-url"),
        "MERIDIAN_NOTIFY_API_URL"
    );
}

#[test]
fn sink_settings_are_checked_and_a_bad_one_never_echoes_its_value() {
    let names = ["slack".to_owned()];
    assert_eq!(
        settings_by_sink(&names, &["slack.channel=#deploys=x".to_owned()]).unwrap()["slack"],
        settings(&[("channel", "#deploys=x")])
    );
    let bad = settings_by_sink(&names, &["token=hunter2".to_owned()]).unwrap_err();
    assert!(bad.contains("is not SINK.KEY=VALUE"), "{bad}");
    assert!(!bad.contains("hunter2"), "{bad}");
    let other = settings_by_sink(&names, &["teams.token=hunter2".to_owned()]).unwrap_err();
    assert!(other.contains("add --sink teams"), "{other}");
    assert!(!other.contains("hunter2"), "{other}");

    for (name, pairs, error) in [
        ("../etc", vec![], "is not a sink name"),
        (
            "command",
            vec![("program", "x"), ("required", "yes")],
            "required is true or false",
        ),
        (
            "command",
            vec![("program", "x"), ("on", " , ")],
            "names no event type",
        ),
        (
            "pager",
            vec![],
            "there is no program named meridian-notify-pager in any plugin directory",
        ),
    ] {
        let err = SinkSpec::find(name, &[], settings(&pairs)).unwrap_err();
        assert!(err.contains(error), "{err}");
    }
    let twice = specs(&["a".to_owned(), "a".to_owned()], &[], &[]).unwrap_err();
    assert_eq!(twice, "the sink \"a\" was asked for twice");
}

#[test]
fn a_policy_denial_is_a_failed_step_with_counts_and_never_a_message() {
    let events = Stream::new()
        .run("run.started", &[("mode", text("apply"))])
        .step("step.started", "base", &[])
        .step(
            "policy.passed",
            "base",
            &[("denials", Value::Int(0)), ("warnings", Value::Int(2))],
        )
        .step("step.succeeded", "base", &[])
        .step("step.started", "app", &[])
        .step(
            "policy.denied",
            "app",
            &[("denials", Value::Int(1)), ("warnings", Value::Int(0))],
        )
        .step("step.failed", "app", &[("reason", text("policy-denied"))])
        .run("run.failed", &[])
        .0;
    let report = Report::from_events(&events);
    assert_eq!(report.steps[0].policy, "passed");
    assert_eq!(report.steps[0].policy_warnings, 2);
    assert_eq!(report.steps[1].policy, "denied");
    let markdown = render(&report, Format::Markdown);
    assert!(
        markdown.contains("- **ResourceSet base** the policy warned: 2 rules"),
        "{markdown}"
    );
    assert!(
        markdown.contains(
            "- **ResourceSet app** failed: the run&#39;s policy does not allow the change, or \
             could not be asked (1 rule denied; \\`meridian plan\\` shows which)"
        ),
        "{markdown}"
    );
    let junit = render(&report, Format::Junit);
    assert!(junit.contains("<failure type=\"policy-denied\""), "{junit}");
    let json = render(&report, Format::Json);
    assert!(json.contains("\"policy\": \"denied\""), "{json}");
    assert!(json.contains("\"policyDenials\": 1"), "{json}");
}

#[test]
fn a_document_left_out_by_its_condition_is_skipped_and_says_why() {
    let events = Stream::new()
        .run("run.started", &[("mode", text("apply"))])
        .step("step.started", "canary", &[])
        .step("step.skipped", "canary", &[("reason", text("when-false"))])
        .run("run.succeeded", &[])
        .0;
    let report = Report::from_events(&events);
    assert_eq!(report.steps[0].status, StepState::Skipped);
    assert_eq!(report.summary().skipped, 1);
    let junit = render(&report, Format::Junit);
    assert!(
        junit.contains("<skipped message=\"skipped: left out: its `when` condition is false\"/>"),
        "{junit}"
    );
}

/// A value that names a secret holds a slot of the run's secret table, never
/// the secret. Should one ever land in an event, the line sent to sinks and
/// every report show the placeholder and nothing else about it.
#[test]
fn a_secret_reference_in_an_event_prints_only_the_placeholder() {
    let slot = 4_815_162_342;
    let secret = Value::Secret(meridian_sdk::SecretRef::new(slot));
    let events = Stream::new()
        .run(
            "run.started",
            &[("mode", text("apply")), ("token", secret.clone())],
        )
        .step(
            "step.started",
            "web",
            &[("phase", text("apply")), ("token", secret.clone())],
        )
        .step(
            "step.failed",
            "web",
            &[
                ("phase", text("apply")),
                ("reason", secret.clone()),
                ("token", secret.clone()),
            ],
        )
        .run(
            "run.finished",
            &[("status", text("failed")), ("token", secret)],
        )
        .0;

    for event in &events {
        let line = event.to_json();
        assert!(line.contains(meridian_sdk::SENSITIVE), "{line}");
        assert!(!line.contains(&slot.to_string()), "{line}");
    }
    let report = Report::from_events(&events);
    for format in [Format::Json, Format::Junit, Format::Markdown, Format::Html] {
        let out = render(&report, format);
        assert!(
            !out.contains(&slot.to_string()),
            "{}: {out}",
            format.as_str()
        );
    }
}
