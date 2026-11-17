//! The server as a process: events are sent to it over HTTP, by hand and by
//! the sink Meridian ships, and it shows what they fold to.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use meridian_orchestrator::{event, Event};
use meridian_sdk::Value;

const WRITE: &str = "write-token-0123456789";
const READ: &str = "read-token-0123456789";
const RUN: &str = "2026-11-17T09-14-03Z-7f3a";

/// A running server, stopped when the test ends.
struct Server {
    child: Child,
    address: String,
    data: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(test: &str, read_token: bool, extra: &[&str]) -> Server {
    let data = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("server")
        .join(test);
    let _ = fs::remove_dir_all(&data);
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian-report-server"));
    command
        .args(["--listen", "127.0.0.1:0", "--data"])
        .arg(&data)
        .args(extra)
        .env("MERIDIAN_SERVER_TOKEN", WRITE)
        .env_remove("MERIDIAN_SERVER_READ_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if read_token {
        command.env("MERIDIAN_SERVER_READ_TOKEN", READ);
    }
    let mut child = command.spawn().expect("the server starts");
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let address = first
        .trim()
        .strip_prefix("listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line: {first:?}"))
        .to_owned();
    Server {
        child,
        address,
        data,
    }
}

/// One HTTP request, by hand: the status and the body.
fn request(
    server: &Server,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        server.address,
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status in {response:?}"));
    let body = response
        .split_once("\r\n\r\n")
        .map_or("", |(_, body)| body)
        .to_owned();
    (status, body)
}

fn get(server: &Server, path: &str) -> (u16, String) {
    request(server, "GET", path, None, "")
}

fn an_event(run: &str, seq: u64, kind: &str, step: &str, detail: &[(&str, &str)]) -> Event {
    let mut detail: BTreeMap<String, Value> = detail
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::from(*value)))
        .collect();
    if !step.is_empty() {
        detail.insert("kind".to_owned(), Value::from("ResourceSet"));
    }
    Event {
        run: run.to_owned(),
        seq,
        time: format!("2026-11-17T09:14:{seq:02}.000Z"),
        kind: kind.to_owned(),
        workflow: String::new(),
        step: step.to_owned(),
        instance: step.to_owned(),
        endpoint: String::new(),
        target: if step.is_empty() { "" } else { "ansible" }.to_owned(),
        attempt: u32::from(!step.is_empty()),
        detail,
    }
}

/// An apply of two steps, the second of which fails.
fn a_run(run: &str) -> Vec<Event> {
    vec![
        an_event(
            run,
            1,
            "run.started",
            "",
            &[("mode", "apply"), ("command", "apply site.yaml")],
        ),
        an_event(run, 2, "step.started", "base", &[]),
        an_event(
            run,
            3,
            "resource.changed",
            "base",
            &[("resource", "sshd_config"), ("action", "update")],
        ),
        an_event(run, 4, "step.succeeded", "base", &[]),
        an_event(run, 5, "step.started", "app", &[]),
        an_event(run, 6, "step.failed", "app", &[("reason", "tool-failed")]),
        an_event(run, 7, "run.failed", "", &[]),
    ]
}

fn events_path(run: &str) -> String {
    format!("/v1/projects/shop/runs/{run}/events")
}

#[test]
fn events_sent_in_batches_become_a_run_that_can_be_read_in_every_format() {
    let server = start("batches", false, &[]);
    let (status, home) = get(&server, "/");
    assert_eq!(status, 200);
    assert!(home.contains("No run has been sent here yet"), "{home}");

    let events = a_run(RUN);
    // The run arrives in two batches, as it happens.
    let first = event::to_jsonl(&events[..5]);
    let (status, body) = request(&server, "POST", &events_path(RUN), Some(WRITE), &first);
    assert_eq!(
        (status, body.as_str()),
        (200, "{\"accepted\":5,\"ignored\":0}\n")
    );

    // Part way, the run is still going, and its page asks to be reloaded.
    let (_, page) = get(&server, &format!("/projects/shop/runs/{RUN}"));
    assert!(
        page.contains("<meta http-equiv=\"refresh\" content=\"3\">"),
        "{page}"
    );
    assert!(
        page.contains("Meridian apply did not finish: 2 steps"),
        "{page}"
    );

    // A sender that retries sends some events twice. They are kept once.
    let rest = event::to_jsonl(&events[3..]);
    let (status, body) = request(&server, "POST", &events_path(RUN), Some(WRITE), &rest);
    assert_eq!(
        (status, body.as_str()),
        (200, "{\"accepted\":2,\"ignored\":2}\n")
    );

    // What is stored is exactly the run's events, in order.
    let (status, stored) = get(&server, &events_path(RUN));
    assert_eq!(status, 200);
    assert_eq!(stored, event::to_jsonl(&events));
    assert_eq!(
        fs::read_to_string(
            server
                .data
                .join("projects/shop/runs")
                .join(RUN)
                .join("events.jsonl")
        )
        .unwrap(),
        stored
    );

    // Every format is the report folded from those events.
    let report = meridian_report::Report::from_events(&events);
    for format in meridian_report::Format::ALL {
        let (status, body) = get(
            &server,
            &format!("/v1/projects/shop/runs/{RUN}/report/{}", format.as_str()),
        );
        assert_eq!(status, 200, "{format:?}");
        assert_eq!(body, meridian_report::render(&report, format), "{format:?}");
    }
    let (status, _) = get(&server, &format!("/v1/projects/shop/runs/{RUN}/report/pdf"));
    assert_eq!(status, 404);

    // The pages for people.
    let (_, page) = get(&server, &format!("/projects/shop/runs/{RUN}"));
    assert!(!page.contains("http-equiv=\"refresh\""), "{page}");
    assert!(
        page.contains("<h1 class=\"bad\">Meridian apply failed: 2 steps, 1 failed"),
        "{page}"
    );
    assert!(
        page.contains("<a href=\"/projects/shop\">shop</a>"),
        "{page}"
    );
    let (_, project) = get(&server, "/projects/shop");
    assert!(
        project.contains(&format!(
            "<a href=\"/projects/shop/runs/{RUN}\"><code>{RUN}</code></a>"
        )),
        "{project}"
    );
    assert!(
        project.contains("<b>1</b><span>apply runs, 1 failed</span>"),
        "{project}"
    );
    let (_, home) = get(&server, "/");
    assert!(
        home.contains("<a href=\"/projects/shop\">shop</a>"),
        "{home}"
    );

    let (status, list) = get(&server, "/v1/projects/shop/runs");
    assert_eq!(status, 200);
    assert_eq!(
        list,
        format!(
            "{{\"runs\":[{{\"run\":\"{RUN}\",\"mode\":\"apply\",\"status\":\"failed\",\
             \"started\":\"2026-11-17T09:14:01.000Z\",\"finished\":\"2026-11-17T09:14:07.000Z\",\
             \"steps\":2,\"failed\":1,\"changed\":1,\"drifted\":0}}]}}\n"
        )
    );

    for missing in [
        "/projects/nowhere",
        "/v1/projects/nowhere/runs",
        "/projects/shop/runs/2026-01-01T00-00-00Z-0000",
        "/v1/projects/shop/runs/2026-01-01T00-00-00Z-0000/events",
        "/nothing",
    ] {
        assert_eq!(get(&server, missing).0, 404, "{missing}");
    }
    assert_eq!(get(&server, "/healthz"), (200, "ok\n".to_owned()));
}

#[test]
fn adding_events_needs_the_write_token_and_reading_can_need_one() {
    let server = start("tokens", true, &[]);
    let body = event::to_jsonl(&a_run(RUN));
    for token in [None, Some("wrong-token-0123456789"), Some(READ), Some("")] {
        let (status, _) = request(&server, "POST", &events_path(RUN), token, &body);
        assert_eq!(status, 401, "{token:?}");
    }
    assert!(!server.data.join("projects").exists());
    let (status, _) = request(&server, "POST", &events_path(RUN), Some(WRITE), &body);
    assert_eq!(status, 200);

    for path in [
        "/".to_owned(),
        "/projects/shop".to_owned(),
        format!("/projects/shop/runs/{RUN}"),
        "/v1/projects/shop/runs".to_owned(),
        events_path(RUN),
        format!("/v1/projects/shop/runs/{RUN}/report/json"),
    ] {
        assert_eq!(get(&server, &path).0, 401, "{path}");
        assert_eq!(
            request(&server, "GET", &path, Some("nope"), "").0,
            401,
            "{path}"
        );
        assert_eq!(
            request(&server, "GET", &path, Some(READ), "").0,
            200,
            "{path}"
        );
        // Whoever may write may read.
        assert_eq!(
            request(&server, "GET", &path, Some(WRITE), "").0,
            200,
            "{path}"
        );
    }
    // A browser can be asked for the token as a password.
    let mut stream = TcpStream::connect(&server.address).unwrap();
    // "meridian:read-token-0123456789"
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nAuthorization: Basic bWVyaWRpYW46cmVhZC10b2tlbi0wMTIzNDU2Nzg5\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    // The health check is open: it says nothing.
    assert_eq!(get(&server, "/healthz").0, 200);
}

#[test]
fn what_is_not_an_event_of_the_run_is_refused_and_nothing_is_stored() {
    let server = start("refused", false, &[]);
    let post = |path: &str, body: &str| request(&server, "POST", path, Some(WRITE), body);
    let good = an_event(RUN, 1, "run.started", "", &[]).to_json();

    let (status, body) = post(&events_path(RUN), "not json\n");
    assert_eq!(status, 400);
    assert!(body.starts_with("line 1: "), "{body}");
    let (status, body) = post(&events_path(RUN), &format!("{good}\n{{\"oops\":1}}\n"));
    assert_eq!(status, 400);
    assert!(body.starts_with("line 2: "), "{body}");
    let other = an_event("2026-11-17T09-14-03Z-ffff", 1, "run.started", "", &[]).to_json();
    assert_eq!(
        post(&events_path(RUN), &other),
        (
            400,
            format!(
                "line 1: the event belongs to run \"2026-11-17T09-14-03Z-ffff\", not {RUN:?}\n"
            )
        )
    );
    assert_eq!(
        post(&events_path(RUN), "\n\n"),
        (400, "the request holds no events\n".to_owned())
    );
    // A name that would leave the data directory is not a name.
    assert_eq!(
        post("/v1/projects/..%2F..%2Fetc/runs/x/events", &good).0,
        400
    );
    assert_eq!(post("/v1/projects/-rf/runs/x/events", &good).0, 400);
    assert_eq!(
        post("/v1/projects/shop/runs/..%2Fx/events", &good),
        (400, "\"../x\" is not a run id\n".to_owned())
    );
    // More than a request may carry.
    let huge = "x".repeat(meridian_report_server::MAX_BODY + 1);
    assert_eq!(post(&events_path(RUN), &huge).0, 413);
    assert!(!server.data.join("projects").exists());

    // A field an event does not have is not kept.
    let padded = good.replacen('{', "{\"password\":\"hunter2\",", 1);
    assert_eq!(post(&events_path(RUN), &padded).0, 200);
    let (_, stored) = get(&server, &events_path(RUN));
    assert_eq!(stored, format!("{good}\n"));
}

#[test]
fn a_project_keeps_only_its_newest_runs_and_a_hostile_name_stays_text() {
    let server = start("keep", false, &["--keep-runs", "2"]);
    let runs = [
        "2026-11-17T09-00-00Z-0001",
        "2026-11-17T10-00-00Z-0002",
        "2026-11-17T11-00-00Z-0003",
    ];
    for run in runs {
        let hostile = an_event(run, 1, "step.started", "<script>alert(1)</script>", &[]);
        let (status, _) = request(
            &server,
            "POST",
            &events_path(run),
            Some(WRITE),
            &hostile.to_json(),
        );
        assert_eq!(status, 200);
    }
    let (_, project) = get(&server, "/projects/shop");
    assert!(!project.contains(runs[0]), "{project}");
    assert!(
        project.contains(runs[1]) && project.contains(runs[2]),
        "{project}"
    );
    assert_eq!(get(&server, &events_path(runs[0])).0, 404);

    let (_, page) = get(&server, &format!("/projects/shop/runs/{}", runs[2]));
    assert!(!page.contains("<script>"), "{page}");
    assert!(
        page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{page}"
    );
}

#[test]
fn the_shipped_sink_delivers_a_run_and_fails_when_the_server_refuses() {
    if Command::new("curl").arg("--version").output().is_err() {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let server = start("sink", false, &[]);
    let sink =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sinks/meridian-notify-server");
    let events = a_run(RUN);
    let send = |token: &str| {
        let mut child = Command::new(&sink)
            .env("MERIDIAN_NOTIFY_PROTOCOL", "1")
            .env("MERIDIAN_NOTIFY_URL", format!("http://{}/", server.address))
            .env("MERIDIAN_NOTIFY_PROJECT", "shop")
            .env("MERIDIAN_NOTIFY_TOKEN", token)
            .env("MERIDIAN_NOTIFY_TIMEOUT", "5")
            // The test's server is on this machine; no proxy is in the way.
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event::to_jsonl(&events).as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };

    let refused = send("wrong-token-0123456789");
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(get(&server, &events_path(RUN)).0, 404);
    // The token is not in what the sink says when it fails.
    assert!(!String::from_utf8_lossy(&refused.stderr).contains("wrong-token"));

    let sent = send(WRITE);
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert_eq!(get(&server, &events_path(RUN)).1, event::to_jsonl(&events));
}

#[test]
fn the_server_does_not_start_without_a_real_write_token() {
    let run = |token: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_meridian-report-server"));
        command
            .args(["--listen", "127.0.0.1:0"])
            .env_remove("MERIDIAN_SERVER_TOKEN")
            .env_remove("MERIDIAN_SERVER_READ_TOKEN");
        if let Some(token) = token {
            command.env("MERIDIAN_SERVER_TOKEN", token);
        }
        command.output().unwrap()
    };
    let none = run(None);
    assert_eq!(none.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&none.stderr),
        "error: a write token is required: set MERIDIAN_SERVER_TOKEN, or give --token-file FILE\n"
    );
    let short = run(Some("hunter2"));
    assert_eq!(short.status.code(), Some(1));
    let said = String::from_utf8_lossy(&short.stderr).into_owned();
    assert!(said.contains("shorter than 16 characters"), "{said}");
    assert!(!said.contains("hunter2"), "{said}");
}

/// The store reads only directories that resolve to a place inside its data
/// directory. A link planted inside it that points outside is listed as a
/// project but never followed: its runs are not read, and no event comes
/// back for it.
#[cfg(unix)]
#[test]
fn a_link_out_of_the_data_directory_is_never_followed() {
    use meridian_report_server::store::Store;

    let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("server")
        .join("link-out");
    let _ = fs::remove_dir_all(&base);
    let outside = base.join("outside").join("runs");
    let run = outside.join("2026-11-17T09-14-03Z-7f3a");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("events.jsonl"), "{}\n").unwrap();
    let data = base.join("data");
    let projects = data.join("projects");
    fs::create_dir_all(&projects).unwrap();
    std::os::unix::fs::symlink(base.join("outside"), projects.join("planted")).unwrap();

    let store = Store::new(&data, 10);
    assert_eq!(store.projects(), vec!["planted".to_owned()]);
    assert!(store.runs("planted").is_empty());
    assert_eq!(store.events("planted", "2026-11-17T09-14-03Z-7f3a"), None);
}
