//! `meridian` inside a pipeline engine, run as a process against the fake
//! target: a refusal becomes an annotation at the line of the document, a
//! failed step becomes one at its document, the report becomes the job's
//! summary, and no value of the run is in either. The engine is stood in for
//! by its environment variable; the real one is `.github/workflows/
//! pipeline-shell.yml`, which runs the GitHub Actions snippet of
//! `docs/operations/running-in-a-pipeline.md` for real.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::repo_root;

/// Unlike anything a document or a target writes, so that finding it is
/// finding a leak.
const PASSWORD: &str = "zzq-Vt9-not-a-real-password-4kLm";

/// Two stages on the fake target, the second of which fails when `MOTD`
/// holds the fake tool's magic word, with a secret in a param so that the
/// tool is handed a value the engine must never be shown.
const WORKFLOW: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
  outputs:
    - name: address
      value: "${vm.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: fake
  targetHost:
    from: infrastructure
    ref: network
    output: address
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  resources:
    - id: conf
      type: file
      params:
        content: "password=${{ secret.db_password }}\n"
    - id: motd
      type: file
      params:
        content: "MOTD\n"
"#;

fn scratch(test: &str, motd: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("pipeline")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("site.yaml"), WORKFLOW.replace("MOTD", motd)).unwrap();
    dir
}

/// Runs `meridian` as a job of the engine whose variable is given: its
/// standard output is a pipe, as it is in a job.
fn meridian(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_ANNOTATIONS")
        .env("SITE_DB_PASSWORD", PASSWORD)
        .env("FAKE_SECRET_SINK", dir.join("sink.json"));
    // Not inside the engine this test runs in, unless the test says so.
    for variable in [
        "GITHUB_ACTIONS",
        "GITHUB_STEP_SUMMARY",
        "TF_BUILD",
        "AGENT_TEMPDIRECTORY",
        "GITLAB_CI",
        "BUILDKITE",
        "JENKINS_URL",
    ] {
        command.env_remove(variable);
    }
    // Nor on a cloud machine, or one with cloud credentials in its
    // environment, unless the test says so.
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if [
            "AWS_",
            "AZURE_",
            "GOOGLE_",
            "CLOUDSDK_",
            "GCE_",
            "IDENTITY_",
            "MSI_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        {
            command.env_remove(name.as_ref());
        }
    }
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn annotations(output: &Output) -> Vec<String> {
    stderr(output)
        .lines()
        .filter(|line| line.starts_with("::") || line.starts_with("##vso["))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_refused_document_is_an_annotation_at_its_file_and_line_on_github() {
    let dir = scratch("refused", "welcome");
    let typo = WORKFLOW.replace("    - id: motd\n", "    - id: motd\n      colour: blue\n");
    fs::write(dir.join("site.yaml"), typo.replace("MOTD", "welcome")).unwrap();
    let summary = dir.join("summary.md");
    let out = meridian(
        &dir,
        &["validate", "site.yaml"],
        &[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_STEP_SUMMARY", summary.to_str().unwrap()),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    // The error for a person is as it always was, and the annotation is on
    // standard error beside it, pointing at the document's own line.
    assert!(
        stderr(&out)
            .contains("error: site.yaml[doc 1]:36:7: spec.resources[1]: unknown field \"colour\""),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        annotations(&out),
        ["::error file=site.yaml,line=36,col=7,title=Meridian%3A validate::spec.resources[1]: unknown field \"colour\""]
    );
    assert_eq!(stdout(&out), "");
    // The summary is the problem, as the job page shows it.
    let written = fs::read_to_string(&summary).unwrap();
    assert!(
        written.starts_with(
            "## ✗ Meridian validate failed: 1 problem\n\n- **validate** `site.yaml:36:7`:"
        ),
        "{written}"
    );

    // Azure writes the same problem in its own syntax, and names the
    // summary file to the agent.
    let temp = dir.join("agent-temp");
    fs::create_dir_all(&temp).unwrap();
    let out = meridian(
        &dir,
        &["validate", "site.yaml"],
        &[
            ("TF_BUILD", "True"),
            ("AGENT_TEMPDIRECTORY", temp.to_str().unwrap()),
        ],
    );
    assert_eq!(
        annotations(&out),
        [
            "##vso[task.logissue type=error;sourcepath=site.yaml;linenumber=36;columnnumber=7]validate: spec.resources[1%5D: unknown field \"colour\"".to_owned(),
            format!("##vso[task.uploadsummary]{}", temp.join("meridian-summary.md").display()),
        ]
    );
    assert!(temp.join("meridian-summary.md").is_file());
}

#[test]
fn outside_an_engine_or_switched_off_nothing_is_annotated() {
    let dir = scratch("off", "welcome");
    let typo = WORKFLOW.replace("    - id: motd\n", "    - id: motd\n      colour: blue\n");
    fs::write(dir.join("site.yaml"), typo.replace("MOTD", "welcome")).unwrap();
    let plain = meridian(&dir, &["validate", "site.yaml"], &[]);
    assert_eq!(out_code(&plain), 1);
    assert_eq!(annotations(&plain), Vec::<String>::new());

    for (args, env) in [
        (
            vec!["validate", "site.yaml", "--annotations", "off"],
            vec![("GITHUB_ACTIONS", "true")],
        ),
        (
            vec!["validate", "site.yaml"],
            vec![("GITHUB_ACTIONS", "true"), ("MERIDIAN_ANNOTATIONS", "off")],
        ),
        // Set to nothing is not set.
        (vec!["validate", "site.yaml"], vec![("GITHUB_ACTIONS", "")]),
        // An engine with no annotation syntax of its own gets the log.
        (vec!["validate", "site.yaml"], vec![("GITLAB_CI", "true")]),
        (
            vec!["validate", "site.yaml"],
            vec![("JENKINS_URL", "https://ci.example.com/")],
        ),
    ] {
        let out = meridian(&dir, &args, &env);
        assert_eq!(out_code(&out), 1, "{args:?} {env:?}");
        assert_eq!(annotations(&out), Vec::<String>::new(), "{args:?} {env:?}");
        assert!(stderr(&out).contains("unknown field"), "{args:?} {env:?}");
    }
}

fn out_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

#[test]
fn a_failed_step_is_annotated_at_its_document_and_the_summary_is_the_report() {
    let dir = scratch("failed", "welcome FAIL");
    let summary = dir.join("summary.md");
    let github = [
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_STEP_SUMMARY", summary.to_str().unwrap()),
    ];
    let out = meridian(&dir, &["apply", "site.yaml"], &github);
    assert_eq!(out_code(&out), 1, "{}", stderr(&out));
    let lines = annotations(&out);
    assert_eq!(lines.len(), 1, "{lines:?}");
    // The ResourceSet starts at the second document of the file.
    assert!(
        lines[0].starts_with("::error file=site.yaml,line=16,title=Meridian%3A ResourceSet web failed::target fake on 10.0.0.5: tool-failed"),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].contains("`meridian apply --resume`"),
        "{}",
        lines[0]
    );
    let written = fs::read_to_string(&summary).unwrap();
    assert!(
        written.starts_with("## ✗ Meridian apply failed: 2 steps, 1 failed"),
        "{written}"
    );
    assert!(
        written.contains("| ResourceSet web | fake | 10.0.0.5 | ✗ failed |"),
        "{written}"
    );
    assert!(
        written.contains("The record is kept in the state directory .meridian: `meridian report "),
        "{written}"
    );

    // The engine was shown nothing of the secret: not in an annotation, not
    // in the summary, not in the log.
    for (what, text) in [
        ("annotations", lines.join("\n")),
        ("the summary", written.clone()),
        ("stdout", stdout(&out)),
        ("stderr", stderr(&out)),
    ] {
        assert!(!text.contains(PASSWORD), "{what} holds the secret:\n{text}");
    }

    // Fixed, the resume succeeds, and the summary says so, after the first.
    fs::write(dir.join("site.yaml"), WORKFLOW.replace("MOTD", "welcome")).unwrap();
    let out = meridian(&dir, &["apply", "site.yaml", "--resume"], &github);
    assert_eq!(out_code(&out), 0, "{}", stderr(&out));
    assert_eq!(annotations(&out), Vec::<String>::new());
    let written = fs::read_to_string(&summary).unwrap();
    assert!(
        written.contains("## ✓ Meridian apply succeeded:"),
        "{written}"
    );
    assert!(!written.contains(PASSWORD));
    // The tool was handed the secret, by its private channel, all the same.
    let handed = fs::read_to_string(dir.join("sink.json")).unwrap();
    assert!(handed.contains(PASSWORD), "{handed}");
    assert!(!stderr(&out).contains(PASSWORD));

    // A plan with --output json keeps its one object on stdout; the
    // annotations live on stderr and do not get in its way.
    let out = meridian(&dir, &["plan", "site.yaml", "--output", "json"], &github);
    assert_eq!(out_code(&out), 0, "{}", stderr(&out));
    let object: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(object["command"], "plan");
    assert!(fs::read_to_string(&summary)
        .unwrap()
        .contains("## ✓ Meridian plan succeeded:"));
}

#[test]
fn a_refused_run_is_one_annotation_with_no_place() {
    let dir = scratch("selector", "welcome");
    let out = meridian(
        &dir,
        &["plan", "site.yaml", "-l", "tier=none"],
        &[("GITHUB_ACTIONS", "true")],
    );
    assert_eq!(out_code(&out), 1);
    assert_eq!(
        annotations(&out),
        ["::error title=Meridian%3A plan::no document's labels match the selector tier=none"]
    );
}

/// A document with an ambient cloud credential, and nothing else to check.
const AMBIENT: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: cloud
spec:
  target: fake
  credentials:
    - name: deployer
      type: aws
      from: ambient
  resources:
    - id: vm
      type: file
      params:
        content: 10.0.0.5
"#;

#[test]
fn secret_check_looks_for_an_ambient_credential_where_the_tool_would() {
    let dir = scratch("ambient", "welcome");
    fs::write(dir.join("cloud.yaml"), AMBIENT).unwrap();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let no_cloud = [
        ("HOME", home.to_str().unwrap()),
        ("AWS_EC2_METADATA_DISABLED", "true"),
    ];

    // Present: the pipeline exported keys, as its credentials action does.
    let mut present = no_cloud.to_vec();
    present.push(("AWS_ACCESS_KEY_ID", "AKIAEXAMPLE"));
    present.push(("AWS_SECRET_ACCESS_KEY", "not-a-real-key"));
    let out = meridian(&dir, &["secret", "check", "cloud.yaml"], &present);
    assert_eq!(out_code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "cloud (cloud.yaml)\n  deployer  aws    ambient, ok: the environment holds AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY\n"
    );
    assert!(!stdout(&out).contains("not-a-real-key"));

    // Absent: nothing in the environment, no files, and the instance is not
    // asked.
    let out = meridian(&dir, &["secret", "check", "cloud.yaml"], &no_cloud);
    assert_eq!(out_code(&out), 1);
    assert!(
        stdout(&out).contains("  deployer  aws    ambient, MISSING: "),
        "{}",
        stdout(&out)
    );
    assert_eq!(
        stderr(&out),
        "error: 1 ambient credential would not be found by plan or apply\n"
    );

    // Unreachable: the identity endpoint does not answer.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let out = meridian(
        &dir,
        &["secret", "check", "cloud.yaml", "--output", "json"],
        &[
            ("HOME", home.to_str().unwrap()),
            ("AWS_EC2_METADATA_SERVICE_ENDPOINT", &closed),
        ],
    );
    assert_eq!(out_code(&out), 1, "{}", stderr(&out));
    let object: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(object["ok"], false);
    assert_eq!(object["credentialsDeclared"], 1);
    assert_eq!(object["credentialsUnanswered"], 1);
    let credential = &object["documents"][0]["credentials"][0];
    assert_eq!(credential["name"], "deployer");
    assert_eq!(credential["type"], "aws");
    assert_eq!(credential["state"], "missing");
    assert_eq!(credential["answered"], false);
    assert!(
        credential["detail"]
            .as_str()
            .unwrap()
            .contains("did not answer"),
        "{credential}"
    );
    // Inside the engine, the refusal is annotated like any other.
    let out = meridian(
        &dir,
        &["secret", "check", "cloud.yaml"],
        &[
            ("HOME", home.to_str().unwrap()),
            ("AWS_EC2_METADATA_DISABLED", "true"),
            ("GITHUB_ACTIONS", "true"),
        ],
    );
    assert_eq!(
        annotations(&out),
        ["::error title=Meridian%3A secret check::1 ambient credential would not be found by plan or apply"]
    );
}

/// The page's GitHub Actions snippet is what the smoke-test workflow runs:
/// every `meridian` command line of the snippet is a line of the workflow,
/// so the page cannot drift from what is tested for real.
#[test]
fn the_github_actions_snippet_on_the_page_is_what_the_workflow_runs() {
    let root = repo_root();
    let page = fs::read_to_string(root.join("docs/operations/running-in-a-pipeline.md")).unwrap();
    let workflow = fs::read_to_string(root.join(".github/workflows/pipeline-shell.yml")).unwrap();
    let snippet = page
        .split("<!-- github-actions-snippet -->")
        .nth(1)
        .expect("the page marks its GitHub Actions snippet")
        .split("```")
        .nth(1)
        .expect("the snippet is a fenced block");
    let commands: Vec<&str> = snippet
        .lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("run: ").unwrap_or(line))
        .filter(|line| line.starts_with("meridian "))
        .collect();
    assert!(commands.len() >= 4, "{commands:?}");
    // The workflow may add a redirection of standard error after a command,
    // to read the annotations back; the command itself is the page's.
    for command in commands {
        assert!(
            workflow.lines().map(str::trim).any(|line| {
                let line = line.strip_prefix("run: ").unwrap_or(line);
                line == command
                    || line
                        .strip_prefix(command)
                        .is_some_and(|rest| rest.starts_with(" 2>"))
            }),
            "the page's command `{command}` is not run by pipeline-shell.yml"
        );
    }
}
