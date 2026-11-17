//! End to end with the real Docker: a document is compiled and brought up by
//! the plugin binary, changed, brought up again and torn down.
//!
//! It runs wherever `docker compose` and a Docker daemon are available, which
//! includes the project's CI, and is skipped with a note where they are not.
//! Set `MERIDIAN_REAL_DOCKER=1` to make a missing Docker a failure instead,
//! or `MERIDIAN_REAL_DOCKER=0` to skip it even where Docker is present.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use meridian_plugin_host::{Client, StartOptions};
use meridian_plugin_testkit::{graph_from_text, plugin_binary, scratch};
use meridian_sdk::{Artifact, ChangeAction, Data, LogLine, Plan, RunContext, Value};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether to run, by what the machine has and what the operator asked for.
fn docker_is_available() -> bool {
    let asked = std::env::var("MERIDIAN_REAL_DOCKER").unwrap_or_default();
    if asked == "0" {
        eprintln!("skipped: MERIDIAN_REAL_DOCKER=0");
        return false;
    }
    let available = quiet("docker", &["compose", "version"]) && quiet("docker", &["info"]);
    if !available {
        assert!(
            asked != "1",
            "MERIDIAN_REAL_DOCKER=1, and docker compose or the Docker daemon is not available"
        );
        eprintln!("skipped: docker compose or the Docker daemon is not available");
    }
    available
}

/// Removes everything the project made when the test ends, however it ends.
struct TearDown {
    project: String,
}

impl Drop for TearDown {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args([
                "compose",
                "--project-name",
                &self.project,
                "down",
                "--volumes",
                "--remove-orphans",
                "--timeout",
                "1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The names of the project's containers, running or not.
fn containers(project: &str) -> Vec<String> {
    let output = Command::new("docker")
        .args(["ps", "--all", "--format", "{{.Names}}", "--filter"])
        .arg(format!("label=com.docker.compose.project={project}"))
        .output()
        .expect("docker runs");
    let mut names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

/// The document, with a greeting the web server serves and, optionally, a
/// second service that waits for it.
fn document(project: &str, greeting: &str, with_sidecar: bool) -> String {
    let sidecar = if with_sidecar {
        "    - id: sidecar\n      type: service\n      params:\n        image: busybox:stable\n        command: [sleep, \"3600\"]\n        stop_grace_period: 1s\n      dependsOn:\n        - resource: web\n"
    } else {
        ""
    };
    format!(
        r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: real
spec:
  target: docker
  resources:
    - id: deployment
      type: project
      params:
        name: {project}
        wait_timeout: 120
    - id: site
      type: volume
    - id: web
      type: service
      params:
        image: busybox:stable
        command:
          - sh
          - -c
          - echo "$GREETING" > /www/index.html && exec httpd -f -p 80 -h /www
        environment:
          GREETING: {greeting}
        ports: ["127.0.0.1::80"]
        volumes: ["${{site}}:/www"]
        stop_grace_period: 1s
        healthcheck:
          test: [CMD-SHELL, "wget -q -O /dev/null http://127.0.0.1/ || exit 1"]
          interval: 1s
          retries: 30
{sidecar}  outputs:
    - name: url
      value: http://127.0.0.1:${{web.port.80}}/
    - name: port
      value: ${{web.port.80}}
    - name: container
      value: ${{web.Id}}
    - name: state
      value: ${{web.State.Status}}
"#
    )
}

struct Session {
    client: Client,
    context: RunContext,
}

impl Session {
    fn compile(&self, text: &str) -> Artifact {
        let graph = graph_from_text("real.yaml", text, "docker");
        let emitted = self
            .client
            .emit(&graph, &Data::new())
            .unwrap_or_else(|e| panic!("the document compiles: {e}"));
        assert!(emitted.warnings.is_empty(), "{:?}", emitted.warnings);
        emitted.artifact
    }

    fn plan(&self, artifact: &Artifact) -> Plan {
        let mut lines: Vec<LogLine> = Vec::new();
        self.client
            .plan(&self.context, artifact, &mut |line| lines.push(line))
            .unwrap_or_else(|e| panic!("the plan runs: {e}\n{lines:#?}"))
    }

    fn apply(&self, artifact: &Artifact) -> (Vec<String>, Vec<(String, String)>) {
        let mut lines: Vec<LogLine> = Vec::new();
        let applied = self
            .client
            .apply(&self.context, artifact, &mut |line| lines.push(line))
            .unwrap_or_else(|e| panic!("the apply runs: {e}\n{lines:#?}"));
        assert!(
            lines.iter().any(|l| l.text.contains("Container")),
            "Compose's own progress is relayed: {lines:#?}"
        );
        let outputs = applied
            .outputs
            .iter()
            .map(|o| match &o.value {
                Value::String(s) => (o.name.clone(), s.clone()),
                other => panic!("output {} is {other:?}", o.name),
            })
            .collect();
        (shown(&applied.changes), outputs)
    }
}

fn shown(changes: &[meridian_sdk::Change]) -> Vec<String> {
    changes
        .iter()
        .map(|c| format!("{} {}", c.action, c.resource))
        .collect()
}

fn value<'a>(outputs: &'a [(String, String)], name: &str) -> &'a str {
    outputs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("no output {name} in {outputs:?}"))
}

/// A plain HTTP GET, enough to read what the web server serves.
fn get(url: &str) -> String {
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix('/'))
        .unwrap_or_else(|| panic!("{url} is not the URL the document describes"));
    let mut last = String::new();
    for _ in 0..20 {
        match TcpStream::connect(address) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                let mut response = String::new();
                let _ = stream.read_to_string(&mut response);
                if let Some((_, body)) = response.split_once("\r\n\r\n") {
                    return body.to_owned();
                }
                last = response;
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("{url} did not answer: {last}");
}

#[test]
fn a_project_is_brought_up_changed_and_kept_in_step_with_its_document() {
    if !docker_is_available() {
        return;
    }
    let dir = scratch(TMP, "real");
    let project = format!("meridian-it-{}", std::process::id());
    let _tear_down = TearDown {
        project: project.clone(),
    };
    let binary = plugin_binary("meridian-target-docker");
    let session = Session {
        client: Client::start(binary.to_str().unwrap(), &StartOptions::default())
            .expect("the plugin starts"),
        context: RunContext {
            document: "real".into(),
            work_dir: Path::new(&dir).join("work"),
            ..RunContext::default()
        },
    };

    // A plan of something new is all creates, and creates nothing.
    let first = session.compile(&document(&project, "hello", true));
    let plan = session.plan(&first);
    assert_eq!(
        shown(&plan.changes),
        [
            "create default",
            "create site",
            "create sidecar",
            "create web"
        ]
    );
    assert!(!plan.unknown);
    assert!(
        containers(&project).is_empty(),
        "a plan must change nothing"
    );

    // An apply does what the plan said, waits for the healthcheck, and
    // answers the outputs from the running project.
    let (changes, outputs) = session.apply(&first);
    assert_eq!(changes, shown(&plan.changes));
    assert_eq!(
        containers(&project),
        [format!("{project}-sidecar-1"), format!("{project}-web-1")]
    );
    assert_eq!(value(&outputs, "state"), "running");
    assert_eq!(value(&outputs, "container").len(), 64);
    let port: u16 = value(&outputs, "port").parse().expect("a port number");
    assert_eq!(value(&outputs, "url"), format!("http://127.0.0.1:{port}/"));
    assert_eq!(get(value(&outputs, "url")).trim(), "hello");

    // What was just applied is in step with its document.
    let plan = session.plan(&first);
    assert!(plan.in_sync(), "{:?}", plan.changes);
    let (changes, _) = session.apply(&first);
    assert!(changes.is_empty(), "{changes:?}");

    // The outputs can be read again later without applying anything.
    let read: Vec<(String, String)> = session
        .client
        .outputs(&session.context)
        .expect("the outputs are read")
        .into_iter()
        .map(|o| match o.value {
            Value::String(s) => (o.name, s),
            other => panic!("output {} is {other:?}", o.name),
        })
        .collect();
    assert_eq!(read, outputs);

    // A changed service is replaced and a removed one deleted, and the plan
    // says so before the apply does it.
    let second = session.compile(&document(&project, "goodbye", false));
    let plan = session.plan(&second);
    assert_eq!(shown(&plan.changes), ["replace web", "delete sidecar"]);
    assert_eq!(plan.changes[0].action, ChangeAction::Replace);
    assert_eq!(containers(&project).len(), 2, "a plan must change nothing");

    let (changes, outputs) = session.apply(&second);
    assert_eq!(changes, ["replace web", "delete sidecar"]);
    assert_eq!(containers(&project), [format!("{project}-web-1")]);
    assert_eq!(get(value(&outputs, "url")).trim(), "goodbye");
    assert!(session.plan(&second).in_sync());

    // A container stopped behind Compose's back is drift the plan sees.
    assert!(quiet(
        "docker",
        &["stop", "--time", "1", &format!("{project}-web-1")]
    ));
    let plan = session.plan(&second);
    assert_eq!(shown(&plan.changes), ["update web"]);
    assert!(
        plan.changes[0].summary.starts_with("started; container"),
        "{}",
        plan.changes[0].summary
    );
    let (changes, outputs) = session.apply(&second);
    assert_eq!(changes, ["update web"]);
    assert_eq!(get(value(&outputs, "url")).trim(), "goodbye");
}

#[test]
fn a_file_only_compose_can_fault_is_refused_by_compose_itself() {
    if !docker_is_available() {
        return;
    }
    let dir = scratch(TMP, "real-rejected");
    let project = format!("meridian-it-bad-{}", std::process::id());
    let _tear_down = TearDown {
        project: project.clone(),
    };
    let binary = plugin_binary("meridian-target-docker");
    let client = Client::start(binary.to_str().unwrap(), &StartOptions::default())
        .expect("the plugin starts");
    let context = RunContext {
        document: "real".into(),
        work_dir: Path::new(&dir).join("work"),
        ..RunContext::default()
    };
    // The attribute is one of Compose's, so the document compiles; its value
    // is not one Compose accepts, which only Compose knows.
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: real\nspec:\n  target: docker\n  resources:\n    - id: deployment\n      type: project\n      params: {{name: {project}}}\n    - id: web\n      type: service\n      params:\n        image: busybox:stable\n        ports: not-a-list\n"
    );
    let graph = graph_from_text("real.yaml", &text, "docker");
    let artifact = client.emit(&graph, &Data::new()).unwrap().artifact;
    let mut lines: Vec<LogLine> = Vec::new();
    let err = client
        .plan(&context, &artifact, &mut |line| lines.push(line))
        .expect_err("Compose rejects the file");
    let message = err.to_string();
    assert!(
        message.starts_with("docker compose rejected the compiled file"),
        "{message}"
    );
    assert!(message.contains("ports"), "{message}");
    assert!(containers(&project).is_empty());
}
