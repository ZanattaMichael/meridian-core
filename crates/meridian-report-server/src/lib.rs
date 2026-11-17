//! The Meridian report server.
//!
//! It is an optional place to send the events of runs, through a sink, and
//! to look at them afterwards: a list of runs per project, each run's
//! report, and every report format over HTTP. It reads one thing, events,
//! and keeps one thing, events ([`store`]). It cannot start a run, holds no
//! state about infrastructure and is never sent a value, because an event
//! does not hold one. Meridian does not need it in order to run.
//!
//! | Request | Does |
//! |---|---|
//! | `POST /v1/projects/{project}/runs/{run}/events` | adds events, one JSON object per line. Needs the write token |
//! | `GET /v1/projects/{project}/runs` | the project's runs, as JSON |
//! | `GET /v1/projects/{project}/runs/{run}/events` | a run's events, one per line |
//! | `GET /v1/projects/{project}/runs/{run}/report/{format}` | a run's report: `json`, `junit`, `markdown` or `html` |
//! | `GET /`, `/projects/{project}`, `/projects/{project}/runs/{run}` | the pages for people |
//! | `GET /healthz` | `ok` |
//!
//! The server speaks plain HTTP and is meant to sit behind whatever already
//! terminates TLS for you. `docs/reference/report-server.md` is the guide.

pub mod store;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{
    HeaderName, AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
    WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use meridian_orchestrator::event;
use meridian_report::render::{escape, headline, STYLE};
use meridian_report::{render, Format, Report, RunState};

use store::{Reject, Store};

/// The most one request may carry.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// How many runs a list shows, newest first.
pub const LIST_LIMIT: usize = 200;

/// What the server needs to answer a request.
pub struct App {
    pub store: Store,
    /// What a sender must present to add events.
    pub write_token: String,
    /// What a reader must present, when reading is not open.
    pub read_token: Option<String>,
}

/// Compares two secrets without stopping at the first difference, so how
/// long the comparison takes says nothing about how much matched.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut differs = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        differs |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    differs == 0
}

/// The secret a request presents: a bearer token, or the password of HTTP
/// basic authentication, which is what a browser can ask a person for.
fn presented(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    if let Some(token) = value.strip_prefix("Bearer ") {
        return Some(token.trim().to_owned());
    }
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    decoded
        .split_once(':')
        .map(|(_, password)| password.to_owned())
}

fn text(status: StatusCode, body: impl Into<String>) -> Response {
    (
        status,
        [
            (CONTENT_TYPE, "text/plain; charset=utf-8"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body.into(),
    )
        .into_response()
}

fn typed(content_type: &'static str, body: String) -> Response {
    (
        [
            (CONTENT_TYPE, content_type),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (CACHE_CONTROL, "no-store"),
            // A page here is text and a style sheet. Nothing else may load
            // or run, whatever a name in an event tries.
            (
                CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
            ),
            (HeaderName::from_static("referrer-policy"), "no-referrer"),
        ],
        body,
    )
        .into_response()
}

fn not_found() -> Response {
    text(StatusCode::NOT_FOUND, "not found\n")
}

impl App {
    fn may_write(&self, headers: &HeaderMap) -> Result<(), Box<Response>> {
        match presented(headers) {
            Some(token) if same(&token, &self.write_token) => Ok(()),
            _ => Err(Box::new(text(
                StatusCode::UNAUTHORIZED,
                "adding events needs the write token: Authorization: Bearer <token>\n",
            ))),
        }
    }

    fn may_read(&self, headers: &HeaderMap) -> Result<(), Box<Response>> {
        let Some(read_token) = &self.read_token else {
            return Ok(());
        };
        match presented(headers) {
            // Whoever may write may read.
            Some(token) if same(&token, read_token) || same(&token, &self.write_token) => Ok(()),
            _ => {
                let mut response = text(StatusCode::UNAUTHORIZED, "reading needs the read token\n");
                response.headers_mut().insert(
                    WWW_AUTHENTICATE,
                    "Basic realm=\"Meridian reports\", charset=\"UTF-8\""
                        .parse()
                        .expect("a static header value"),
                );
                Err(Box::new(response))
            }
        }
    }

    fn report(&self, project: &str, run: &str) -> Option<Report> {
        self.store
            .events(project, run)
            .map(|events| Report::from_events(&events))
    }
}

/// The server's routes.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/", get(home))
        .route("/projects/{project}", get(project_page))
        .route("/projects/{project}/runs/{run}", get(run_page))
        .route("/v1/projects/{project}/runs", get(list_runs))
        .route(
            "/v1/projects/{project}/runs/{run}/events",
            get(read_events).post(add_events),
        )
        .route(
            "/v1/projects/{project}/runs/{run}/report/{format}",
            get(read_report),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

async fn add_events(
    State(app): State<Arc<App>>,
    Path((project, run)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(refused) = app.may_write(&headers) {
        return *refused;
    }
    let Ok(body) = std::str::from_utf8(&body) else {
        return text(StatusCode::BAD_REQUEST, "the body is not UTF-8\n");
    };
    match app.store.append(&project, &run, body) {
        Ok(done) => typed(
            "application/json",
            format!(
                "{{\"accepted\":{},\"ignored\":{}}}\n",
                done.accepted, done.ignored
            ),
        ),
        Err(Reject::Invalid(why)) => text(StatusCode::BAD_REQUEST, format!("{why}\n")),
        Err(Reject::Full) => text(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the run already holds as many events as a run may\n",
        ),
        Err(Reject::Io(why)) => {
            // The reason names a path on the server. It goes to the
            // server's own log and not to whoever asked.
            eprintln!("error: storing events of {project}/{run}: {why}");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the events could not be stored\n",
            )
        }
    }
}

async fn read_events(
    State(app): State<Arc<App>>,
    Path((project, run)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = app.may_read(&headers) {
        return *refused;
    }
    match app.store.events(&project, &run) {
        Some(events) => typed("application/x-ndjson", event::to_jsonl(&events)),
        None => not_found(),
    }
}

async fn read_report(
    State(app): State<Arc<App>>,
    Path((project, run, format)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = app.may_read(&headers) {
        return *refused;
    }
    let Ok(format) = Format::parse(&format) else {
        return not_found();
    };
    match app.report(&project, &run) {
        Some(report) => typed(
            match format {
                Format::Json => "application/json",
                Format::Junit => "application/xml",
                Format::Markdown => "text/markdown; charset=utf-8",
                Format::Html => "text/html; charset=utf-8",
            },
            render(&report, format),
        ),
        None => not_found(),
    }
}

/// The newest runs of a project, newest first, each folded to its report.
fn recent(app: &App, project: &str) -> Vec<Report> {
    app.store
        .runs(project)
        .iter()
        .rev()
        .take(LIST_LIMIT)
        .filter_map(|run| app.report(project, run))
        .collect()
}

fn json_string(text: &str) -> String {
    meridian_sdk::Value::from(text).canonical()
}

async fn list_runs(
    State(app): State<Arc<App>>,
    Path(project): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = app.may_read(&headers) {
        return *refused;
    }
    if !app.store.projects().contains(&project) {
        return not_found();
    }
    let mut out = String::from("{\"runs\":[");
    for (n, report) in recent(&app, &project).iter().enumerate() {
        let summary = report.summary();
        let _ = write!(
            out,
            "{}{{\"run\":{},\"mode\":{},\"status\":{},\"started\":{},\"finished\":{},\
             \"steps\":{},\"failed\":{},\"changed\":{},\"drifted\":{}}}",
            if n == 0 { "" } else { "," },
            json_string(&report.run),
            json_string(&report.mode),
            json_string(report.status.as_str()),
            json_string(&report.started),
            json_string(&report.finished),
            summary.steps,
            summary.failed,
            summary.changed,
            summary.drifted
        );
    }
    out.push_str("]}\n");
    typed("application/json", out)
}

fn page(title: &str, body: &str) -> Response {
    typed(
        "text/html; charset=utf-8",
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
             <title>{}</title>\n<style>{STYLE}a{{color:inherit}}</style>\n</head>\n<body>\n\
             <main>\n{body}</main>\n</body>\n</html>\n",
            escape(title)
        ),
    )
}

fn status_class(status: RunState) -> &'static str {
    match status {
        RunState::Succeeded => "ok",
        RunState::Failed => "bad",
        RunState::Running => "idle",
    }
}

async fn home(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Err(refused) = app.may_read(&headers) {
        return *refused;
    }
    let mut body = String::from("<h1>Meridian runs</h1>\n");
    let projects = app.store.projects();
    if projects.is_empty() {
        body.push_str(
            "<p>No run has been sent here yet. Add <code>--sink server</code> to a run; \
             the report server guide says how.</p>\n",
        );
    } else {
        body.push_str(
            "<div class=\"scroll\"><table>\n<thead><tr><th>Project</th><th>Runs</th>\
             <th>Latest run</th></tr></thead>\n<tbody>\n",
        );
        for project in projects {
            let runs = app.store.runs(&project);
            let latest = runs.last().and_then(|run| app.report(&project, run));
            let _ = writeln!(
                body,
                "<tr><td><a href=\"/projects/{0}\">{0}</a></td><td>{1}</td><td{2}</td></tr>",
                escape(&project),
                runs.len(),
                match &latest {
                    Some(report) => format!(
                        " class=\"{}\">{}",
                        status_class(report.status),
                        escape(&headline(report))
                    ),
                    None => ">".to_owned(),
                }
            );
        }
        body.push_str("</tbody>\n</table></div>\n");
    }
    page("Meridian runs", &body)
}

async fn project_page(
    State(app): State<Arc<App>>,
    Path(project): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = app.may_read(&headers) {
        return *refused;
    }
    if !app.store.projects().contains(&project) {
        return not_found();
    }
    let total = app.store.runs(&project).len();
    let mut body = format!(
        "<p><a href=\"/\">All projects</a></p>\n<h1>{}</h1>\n",
        escape(&project)
    );
    // How runs have gone lately, by mode: the counts a dashboard opens with.
    let reports = recent(&app, &project);
    let mut by_mode: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for report in &reports {
        let counts = by_mode.entry(report.mode.as_str()).or_default();
        counts.0 += 1;
        counts.1 += u64::from(report.status == RunState::Failed);
    }
    body.push_str("<div class=\"tiles\">\n");
    for (mode, (runs, failed)) in by_mode {
        let _ = writeln!(
            body,
            "<div class=\"tile\"><b>{runs}</b><span>{} runs, {failed} failed</span></div>",
            escape(mode)
        );
    }
    body.push_str(
        "</div>\n<div class=\"scroll\"><table>\n<thead><tr><th>Run</th><th>Result</th>\
         <th>Started</th><th>Duration</th></tr></thead>\n<tbody>\n",
    );
    for report in &reports {
        let _ = writeln!(
            body,
            "<tr><td><a href=\"/projects/{}/runs/{1}\"><code>{1}</code></a></td>\
             <td class=\"{2}\">{3}</td><td>{4}</td><td>{5}</td></tr>",
            escape(&project),
            escape(&report.run),
            status_class(report.status),
            escape(&headline(report)),
            escape(&report.started),
            report
                .duration_ms()
                .map(|ms| format!("{}.{} s", ms / 1000, (ms % 1000) / 100))
                .unwrap_or_default()
        );
    }
    body.push_str("</tbody>\n</table></div>\n");
    if total > reports.len() {
        let _ = writeln!(
            body,
            "<footer>Showing the newest {} of {total} runs.</footer>",
            reports.len()
        );
    }
    page(&format!("Meridian runs: {project}"), &body)
}

async fn run_page(
    State(app): State<Arc<App>>,
    Path((project, run)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = app.may_read(&headers) {
        return *refused;
    }
    let Some(report) = app.report(&project, &run) else {
        return not_found();
    };
    // The run's page is the HTML report itself, with a way back and links
    // to the other formats. While the run is going the page asks to be
    // loaded again, which is how it follows the run without a script.
    let base = format!("/v1/projects/{}/runs/{}", escape(&project), escape(&run));
    let nav = format!(
        "<main>\n<p><a href=\"/projects/{0}\">{0}</a> · <a href=\"{base}/report/json\">JSON</a> · \
         <a href=\"{base}/report/junit\">JUnit</a> · <a href=\"{base}/report/markdown\">Markdown</a> · \
         <a href=\"{base}/events\">events</a></p>",
        escape(&project)
    );
    let mut html = render(&report, Format::Html)
        .replacen("<main>", &nav, 1)
        .replacen("</style>", "a{color:inherit}</style>", 1);
    if report.status == RunState::Running {
        html = html.replacen(
            "<meta charset=\"utf-8\">",
            "<meta charset=\"utf-8\">\n<meta http-equiv=\"refresh\" content=\"3\">",
            1,
        );
    }
    typed("text/html; charset=utf-8", html)
}
