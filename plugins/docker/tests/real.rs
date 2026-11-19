//! End to end with the real Docker: a document is compiled and brought up by
//! the plugin binary, changed, brought up again and torn down.
//!
//! It runs wherever `docker compose` and a Docker daemon are available, which
//! includes the project's CI, and is skipped with a note where they are not.
//! Set `MERIDIAN_TEST_REAL_DOCKER=1` to make a missing Docker a failure instead,
//! or `MERIDIAN_TEST_REAL_DOCKER=0` to skip it even where Docker is present.

// Left out on Windows: this file drives the real tool, and the workflow that
// installs the tool and runs it is a Linux job.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use meridian_plugin_host::{Client, StartOptions};
use meridian_plugin_testkit::{
    graph_from_text, plugin_binary, scratch, secret_at, secrets, KeptLog,
};
use meridian_sdk::{
    Artifact, ChangeAction, Data, Emitter, LogLine, Plan, RunContext, Runner, Value,
};
use meridian_target_docker::{Docker, DockerRunner};

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
    let asked = std::env::var("MERIDIAN_TEST_REAL_DOCKER").unwrap_or_default();
    if asked == "0" {
        eprintln!("skipped: MERIDIAN_TEST_REAL_DOCKER=0");
        return false;
    }
    let available = quiet("docker", &["compose", "version"]) && quiet("docker", &["info"]);
    if !available {
        assert!(
            asked != "1",
            "MERIDIAN_TEST_REAL_DOCKER=1, and docker compose or the Docker daemon is not available"
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

#[ignore = "needs a real Docker; see docs/contributing/real-tool-tests.md"]
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

    // A destroy previews what goes, then takes the project down, its
    // network and volume with it.
    let stop = meridian_plugin_host::Stop::default();
    let mut lines: Vec<LogLine> = Vec::new();
    let preview = session
        .client
        .plan_destroy_with(&session.context, &second, &mut |l| lines.push(l), &stop)
        .unwrap_or_else(|e| panic!("the destroy preview runs: {e}\n{lines:#?}"));
    let mut expected = vec!["delete default", "delete site", "delete web"];
    let mut previewed = shown(&preview.changes);
    previewed.sort();
    assert_eq!(previewed, expected);
    assert_eq!(
        containers(&project).len(),
        1,
        "a preview must change nothing"
    );
    let removed = session
        .client
        .destroy_with(&session.context, &second, &mut |l| lines.push(l), &stop)
        .unwrap_or_else(|e| panic!("the destroy runs: {e}\n{lines:#?}"));
    let mut removed = shown(&removed);
    removed.sort();
    expected.sort();
    assert_eq!(removed, expected);
    assert!(containers(&project).is_empty(), "the project is down");
}

#[ignore = "needs a real Docker; see docs/contributing/real-tool-tests.md"]
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

/// Unlike anything Compose or the plugin prints, with everything a dotenv
/// value has to escape, so that finding it is finding a leak and reading it
/// back whole proves the escaping.
const SECRET: &str = "zzq-Vt9 \"not\" a\\real $tok'en 4kLm";

/// What a container prints for a command, trimmed.
fn in_container(container: &str, command: &[&str]) -> String {
    let output = Command::new("docker")
        .args(["exec", container])
        .args(command)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "docker exec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[ignore = "needs a real Docker; see docs/contributing/real-tool-tests.md"]
#[test]
fn a_secret_reaches_a_service_by_interpolation_and_as_a_compose_secret_and_no_file_holds_it() {
    if !docker_is_available() {
        return;
    }
    let project = "meridian-real-secrets";
    let _tear_down = TearDown {
        project: project.to_owned(),
    };
    drop(TearDown {
        project: project.to_owned(),
    });
    let dir = scratch(TMP, "real-secrets");
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: real\nspec:\n  target: docker\n  resources:\n    - id: deployment\n      type: project\n      params:\n        name: {project}\n        wait: false\n    - id: probe\n      type: service\n      params:\n        image: busybox:stable\n        command: [sleep, \"3600\"]\n        stop_grace_period: 1s\n        environment:\n          TOKEN: placeholder\n        secrets: [token_file]\n    - id: token_file\n      type: secret\n      params:\n        value: placeholder\n"
    );
    let mut graph = graph_from_text("real.yaml", &text, "docker");
    let slot = secret_at(&mut graph, "probe", &["environment", "TOKEN"], "token");
    let tag = secret_at(&mut graph, "token_file", &["value"], "token");
    let _ = tag;
    graph.secrets.truncate(1);
    // Both places are the one secret `token`.
    let value = meridian_sdk::Value::Secret(slot);
    graph
        .resources
        .iter_mut()
        .find(|r| r.id == "token_file")
        .unwrap()
        .params
        .insert("value".to_owned(), value);
    let artifact = Docker.emit(&graph, &Data::new()).unwrap().artifact;
    let compose = &artifact.files["compose.yaml"];
    assert!(
        compose.contains("TOKEN: ${meridian_secret_token}"),
        "{compose}"
    );
    assert!(
        compose.contains("environment: meridian_secret_token"),
        "{compose}"
    );
    assert!(!compose.contains(SECRET));

    let mut context = RunContext {
        document: "real".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    };
    context.secrets = secrets(&[(slot, "token", SECRET)]);
    let log = KeptLog::default();
    let runner = DockerRunner::from_env();
    let plan = runner
        .plan(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert!(!plan.changes.is_empty());
    runner
        .apply(&context, &artifact, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));

    // The container was given the value both ways, exactly.
    let container = format!("{project}-probe-1");
    // Fixed messages, so that a failure never prints the value.
    assert!(
        in_container(&container, &["sh", "-c", "printf '%s' \"$TOKEN\""]) == SECRET,
        "the service's environment does not hold the value Compose was handed"
    );
    assert!(
        in_container(&container, &["cat", "/run/secrets/token_file"]) == SECRET,
        "the service's secret file does not hold the value Compose was handed"
    );
    // Nothing printed holds it, and no file in the work directory does.
    assert!(
        !log.text().iter().any(|l| l.contains(SECRET)),
        "the secret reached the log:\n{}",
        log.text().join("\n")
    );
    for entry in std::fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert!(
                !std::fs::read_to_string(&path).unwrap().contains(SECRET),
                "{} holds the secret in the open",
                path.display()
            );
        }
    }
    // Applied again with the same value, the project is in step.
    let plan = runner
        .plan(&context, &artifact, &KeptLog::default())
        .unwrap();
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
}

/// The schema source against the published Compose Specification, which
/// needs the network; the installed Compose only names the version. The
/// hand-kept attribute lists the compile checks against are held to the
/// schema here, so a new Compose attribute is noticed.
#[ignore = "needs the network; see docs/contributing/real-tool-tests.md"]
#[test]
fn the_compose_schema_is_exported_and_agrees_with_the_checked_attributes() {
    use meridian_plugin_testkit::graph_from_text;
    use meridian_sdk::{SchemaRequest, SchemaSource};
    use meridian_target_docker::{known_keys, ComposeSchemas, Section};
    if std::env::var("MERIDIAN_TEST_REAL_DOCKER").ok().as_deref() != Some("1") {
        eprintln!("skipped: set MERIDIAN_TEST_REAL_DOCKER=1 to fetch the real Compose schema");
        return;
    }
    let dir = scratch(TMP, "real-schemas");
    let text = "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: site\nspec:\n  target: docker\n  resources:\n    - id: web\n      type: service\n      params:\n        image: nginx:1.27\n";
    let request = SchemaRequest::new(
        dir.join("schemas"),
        vec![graph_from_text("site.yaml", text, "docker")],
    );
    let log = KeptLog::default();
    let export = ComposeSchemas::from_env()
        .export(&request, &log)
        .unwrap_or_else(|e| panic!("{e}\n{}", log.text().join("\n")));
    assert_eq!(export.tool, "compose");
    assert!(
        export.pins.contains_key("compose-spec"),
        "{:?}",
        export.pins
    );
    if quiet("docker", &["compose", "version"]) {
        assert!(export.pins.contains_key("compose"), "{:?}", export.pins);
    }
    let service = export
        .schemas
        .iter()
        .find(|s| s.r#type == "service")
        .unwrap_or_else(|| panic!("the service is described: {:?}", export.notes));
    assert!(service.open, "x- extension fields");
    assert!(service.find_param("image").is_some());
    assert!(service.find_param("depends_on").is_some());
    assert!(
        service.source.starts_with("compose-spec sha256:"),
        "{}",
        service.source
    );
    for section in Section::ALL {
        let schema = export
            .schemas
            .iter()
            .find(|s| s.r#type == section.type_name())
            .unwrap_or_else(|| panic!("{} is described", section.type_name()));
        // Every attribute the compile knows is in the schema. The reverse
        // is not asserted: an attribute the schema gained is reported, so
        // the list can be brought up to date on purpose.
        let unknown: Vec<&str> = known_keys(section)
            .iter()
            .copied()
            .filter(|key| schema.find_param(key).is_none())
            .collect();
        assert!(
            unknown.is_empty(),
            "{}: the compile checks for {unknown:?}, which the Compose schema no longer has",
            section.type_name()
        );
        let new: Vec<&str> = schema
            .params
            .iter()
            .map(|p| p.name.as_str())
            .filter(|name| !known_keys(section).contains(name))
            .collect();
        if !new.is_empty() {
            eprintln!(
                "{}: the Compose schema has attributes the compile does not check: {new:?}",
                section.type_name()
            );
        }
    }
}

#[ignore = "needs a real Docker; see docs/contributing/real-tool-tests.md"]
#[test]
fn inspect_reads_a_services_container_across_the_plugin_boundary() {
    if !docker_is_available() {
        return;
    }
    let dir = scratch(TMP, "real-inspect");
    let project = format!("meridian-inspect-{}", std::process::id());
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
    let first = session.compile(&document(&project, "hello", false));
    let read = session
        .client
        .inspect(&session.context, &first, "web", "")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!read.found, "nothing is running yet");

    session.apply(&first);
    let read = session
        .client
        .inspect(&session.context, &first, "web", "")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(true));
    assert_eq!(read.actual.get("status"), Some(&Value::from("running")));

    let changed = session.compile(&document(&project, "bye", false));
    let read = session
        .client
        .inspect(&session.context, &changed, "web", "")
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read.in_desired_state, Some(false));
    assert_eq!(
        containers(&project),
        [format!("{project}-web-1")],
        "inspect changes nothing"
    );
}
