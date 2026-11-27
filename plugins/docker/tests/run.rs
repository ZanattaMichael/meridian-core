//! The running half, against a stand-in for `docker`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, secrets, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, SecretRef, Value};
use meridian_target_docker::DockerRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled(host: &str) -> Artifact {
    let project = r#"{"name":"shop","host":"HOST","context":"","services":["app","db"],
        "outputs":[
          {"name":"url","parts":[{"text":"http://web01:"},{"service":"app","port":"80"}]},
          {"name":"state","parts":[{"service":"db","inspect":".State.Status"}]},
          {"name":"restarts","parts":[{"service":"db","inspect":".RestartCount"}]}]}"#
        .replace("HOST", host);
    artifact(&[("compose.yaml", "name: shop\n"), ("project.json", &project)])
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "shop".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A stand-in that appends each call to `calls` and answers by what the call
/// holds.
fn runner(dir: &Path, cases: &str) -> DockerRunner {
    let program = stand_in(
        dir,
        "docker",
        &format!(
            "echo \"$@\" >> \"{dir}/calls\"\ncase \"$*\" in\n{cases}\nesac",
            dir = dir.display()
        ),
    );
    DockerRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            l.replace(
                "compose --project-name shop --file compose.yaml ",
                "compose ",
            )
            .replace(" --filter label=com.docker.compose.project=shop", "")
            .replace(
                " --format {{.Label \"com.docker.compose.service\"}}|{{.Label \"com.docker.compose.config-hash\"}}|{{.State}}|{{.Names}}|{{.Label \"com.docker.compose.oneoff\"}}",
                " --format <containers>",
            )
        })
        .collect()
}

/// The same project, with settings for how it is brought up.
fn compiled_with(settings: &str) -> Artifact {
    let project = format!(r#"{{"name":"shop","host":"","context":"","outputs":[],{settings}}}"#);
    artifact(&[("compose.yaml", "name: shop\n"), ("project.json", &project)])
}

/// The file as Compose reads it: two services, a network and a volume.
const CONFIG: &str = r#"*" config --format json") echo '{"name":"shop","services":{"db":{"image":"postgres:16"},"app":{"image":"app:1"}},"networks":{"backend":{"name":"shop_backend"}},"volumes":{"data":{"name":"shop_data"}}}' ;;
*" config --hash "*) echo 'db h-db'; echo 'app h-app' ;;"#;

/// Docker has everything, as the file describes it.
const RUNNING: &str = r#"*"ps --all --filter"*) echo 'db|h-db|running|shop-db-1|False'; echo 'app|h-app|running|shop-app-1|False' ;;
*"network ls "*) echo shop_backend ;;
*"volume ls "*) echo shop_data ;;"#;

/// What every plan and apply asks first.
const SURVEY: [&str; 5] = [
    "compose config --format json",
    "compose config --hash *",
    "ps --all --format <containers>",
    "network ls --format {{.Name}}",
    "volume ls --format {{.Name}}",
];

fn shown(changes: &[meridian_sdk::Change]) -> Vec<String> {
    changes.iter().map(ToString::to_string).collect()
}

const UP: &str = r#"*" up "*) echo ' Container shop-db-1  Started' >&2 ;;"#;

const READS: &str = r#"*" port "*) echo '0.0.0.0:32768' ;;
*" ps --all --quiet "*) echo 4f1c9a ;;
*inspect*) echo '[{"Id":"4f1c9a","RestartCount":0,"State":{"Status":"running"}}]' ;;"#;

#[test]
fn a_plan_compares_the_file_with_docker_and_changes_nothing() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = runner(&dir, CONFIG)
        .plan(&context(&dir), &compiled(""), &log)
        .unwrap();
    assert_eq!(calls(&dir), SURVEY);
    assert_eq!(
        shown(&plan.changes),
        [
            "create backend: network shop_backend",
            "create data: volume shop_data",
            "create app: container, image app:1",
            "create db: container, image postgres:16",
        ]
    );
    assert!(!plan.unknown);
    assert_eq!(
        log.text()[0],
        "project shop: comparing the file with what Docker is running"
    );
    // Each change is also shown to the operator as the plan is made.
    assert_eq!(log.text()[1], "create backend: network shop_backend");
    assert!(dir.join("work/compose.yaml").exists());
}

#[test]
fn a_project_already_as_the_file_describes_is_in_sync() {
    let dir = scratch(TMP, "in-sync");
    let plan = runner(&dir, &format!("{CONFIG}\n{RUNNING}"))
        .plan(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
}

#[test]
fn a_plan_reports_a_changed_service_a_stopped_one_and_an_orphan() {
    let dir = scratch(TMP, "plan-drift");
    let cases = format!(
        "{CONFIG}\n*\"ps --all --filter\"*) echo 'db|h-old|running|shop-db-1|False'; echo 'app|h-app|exited|shop-app-1|False'; echo 'cache|h-c|running|shop-cache-1|False'; echo 'app|h-app|running|shop-app-run-1|True' ;;\n*\"network ls \"*) echo shop_backend ;;\n*\"volume ls \"*) echo shop_data ;;"
    );
    let plan = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap();
    assert_eq!(
        shown(&plan.changes),
        [
            "update app: started; container shop-app-1 is exited",
            // Recreating a container loses what was in it, so it is a
            // replace and not an update.
            "replace db: its configuration changed, so its container is recreated",
            "delete cache: no longer in the document, so its containers are removed",
        ]
    );
}

#[test]
fn an_apply_brings_the_project_up_and_reads_the_outputs() {
    let dir = scratch(TMP, "apply");
    let log = KeptLog::default();
    let mut docker = runner(&dir, &format!("{CONFIG}\n{UP}\n{READS}"));
    docker.extra_args = vec!["--pull".into(), "always".into()];
    let applied = docker
        .apply(&context(&dir), &compiled("ssh://web01"), &log)
        .unwrap();
    let all = calls(&dir);
    // The daemon is the document's host, reached over SSH, for every call.
    assert!(
        all.iter().all(|c| c.starts_with("--host ssh://web01 ")),
        "{all:#?}"
    );
    let all: Vec<String> = all
        .iter()
        .map(|c| c.replace("--host ssh://web01 ", ""))
        .collect();
    assert_eq!(all[..SURVEY.len()], SURVEY);
    assert_eq!(
        all[SURVEY.len()..],
        [
            "compose up --detach --remove-orphans --pull always",
            "compose port --protocol tcp app 80",
            "compose ps --all --quiet db",
            "inspect 4f1c9a",
            "compose ps --all --quiet db",
            "inspect 4f1c9a",
        ]
    );
    // What was applied is what the plan said, not what Compose printed.
    assert_eq!(applied.changes.len(), 4);
    assert!(log
        .text()
        .contains(&" Container shop-db-1  Started".to_owned()));
    assert_eq!(
        applied.outputs[0].value,
        Value::String("http://web01:32768".into())
    );
    assert_eq!(applied.outputs[1].value, Value::String("running".into()));
    // One reference alone keeps the type Docker gave it.
    assert_eq!(applied.outputs[2].value, Value::Int(0));
}

#[test]
fn the_projects_settings_shape_every_call() {
    let dir = scratch(TMP, "settings");
    let settings =
        r#""directory":"/srv/shop","profiles":["debug","metrics"],"wait":true,"wait_timeout":90"#;
    runner(&dir, &format!("{CONFIG}\n{RUNNING}"))
        .apply(
            &context(&dir),
            &compiled_with(settings),
            &KeptLog::default(),
        )
        .unwrap();
    let all = calls(&dir);
    let shared = "compose --project-directory /srv/shop --profile debug --profile metrics";
    assert_eq!(all[0], format!("{shared} config --format json"));
    assert_eq!(
        all.last().unwrap(),
        &format!("{shared} up --detach --remove-orphans --wait --wait-timeout 90")
    );
}

#[test]
fn a_project_waits_only_when_its_settings_say_so() {
    let dir = scratch(TMP, "wait");
    runner(&dir, &format!("{CONFIG}\n{RUNNING}"))
        .apply(
            &context(&dir),
            &compiled_with(r#""wait":true"#),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        calls(&dir).last().unwrap(),
        "compose up --detach --remove-orphans --wait"
    );

    let dir = scratch(TMP, "no-wait");
    runner(&dir, &format!("{CONFIG}\n{RUNNING}"))
        .apply(
            &context(&dir),
            &compiled_with(r#""wait":false"#),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        calls(&dir).last().unwrap(),
        "compose up --detach --remove-orphans"
    );
}

#[test]
fn a_service_built_from_source_is_rebuilt_and_makes_the_plan_uncertain() {
    let dir = scratch(TMP, "build");
    let cases = r#"*" config --format json") echo '{"services":{"api":{"build":{"context":"/srv/api"}}}}' ;;
*" config --hash "*) echo 'api h-api' ;;
*"ps --all --filter"*) echo 'api|h-api|running|shop-api-1|False' ;;"#;
    let docker = runner(&dir, cases);
    let plan = docker
        .plan(
            &context(&dir),
            &compiled_with(r#""wait":false"#),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(plan.changes.is_empty());
    assert!(
        plan.unknown,
        "a build's result cannot be known without building"
    );

    docker
        .apply(
            &context(&dir),
            &compiled_with(r#""wait":false"#),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        calls(&dir).last().unwrap(),
        "compose up --detach --remove-orphans --build"
    );
}

#[test]
fn an_output_with_nothing_behind_it_is_an_error() {
    let dir = scratch(TMP, "unpublished");
    let cases = format!("{CONFIG}\n{UP}\n*\" port \"*) echo '' ;;");
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "output \"url\": service \"app\" does not publish port 80"
    );
}

#[test]
fn a_file_compose_rejects_is_an_error_with_what_compose_said() {
    let dir = scratch(TMP, "rejected");
    let cases =
        "*\" config --format json\") echo 'services.web.ports must be a list' >&2; exit 15 ;;";
    let log = KeptLog::default();
    let err = runner(&dir, cases)
        .plan(&context(&dir), &compiled(""), &log)
        .unwrap_err();
    assert_eq!(err.msg, "docker compose rejected the compiled file");
    assert_eq!(err.exit_code, Some(15));
    assert_eq!(err.output, "services.web.ports must be a list\n");
    // Nothing further was asked once the file was refused.
    assert_eq!(calls(&dir).len(), 1);
}

#[test]
fn a_failed_up_keeps_what_compose_said() {
    let dir = scratch(TMP, "failed");
    let cases = format!(
        "{CONFIG}\n*\" up \"*) echo 'Error response from daemon: pull access denied for app' >&2; exit 1 ;;"
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg
            .starts_with("docker compose could not bring the project up"),
        "{}",
        err.msg
    );
    assert!(err.output.contains("pull access denied"), "{}", err.output);
    assert_eq!(err.failures[0].cause.as_str(), "dependency-missing");
}

#[test]
fn a_failed_up_names_the_service_whose_container_broke() {
    let dir = scratch(TMP, "failed-container");
    let cases = format!(
        "{CONFIG}\n*\" up \"*) echo 'dependency failed to start: container web-db-1 is unhealthy' >&2; exit 1 ;;\n*\"--format {{{{.ID}}}}\"*) echo abc123 ;;\n\"inspect abc123\") echo '[{{\"Config\":{{\"Labels\":{{\"com.docker.compose.service\":\"db\"}}}},\"State\":{{\"Status\":\"running\",\"ExitCode\":0,\"Error\":\"\",\"Health\":{{\"Status\":\"unhealthy\",\"Log\":[]}}}}}}]' ;;"
    );
    let err = runner(&dir, &cases)
        .apply(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.failures.len(), 1, "{:?}", calls(&dir));
    let f = &err.failures[0];
    assert_eq!(
        (
            f.resource.as_str(),
            f.tool_code.as_str(),
            f.summary.as_str()
        ),
        ("db", "unhealthy", "the health check failed")
    );
}

#[test]
fn docker_being_unreachable_is_an_error_not_an_empty_plan() {
    let dir = scratch(TMP, "daemon-down");
    let cases = format!(
        "{CONFIG}\n*\"ps --all --filter\"*) echo 'Cannot connect to the Docker daemon' >&2; exit 1 ;;"
    );
    let err = runner(&dir, &cases)
        .plan(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "docker could not list what the project has running"
    );
    assert!(err.output.contains("Cannot connect to the Docker daemon"));
}

#[test]
fn an_artifact_this_plugin_did_not_compile_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, CONFIG)
        .plan(
            &context(&dir),
            &artifact(&[("playbook.yml", "- hosts: all\n")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no compose.yaml and project.json; it was not compiled by this plugin"
    );
    assert!(
        calls(&dir).is_empty(),
        "nothing is run for a foreign artifact"
    );
}

#[test]
fn outputs_are_read_from_the_running_project() {
    let dir = scratch(TMP, "outputs");
    let docker = runner(&dir, READS);
    assert!(docker.outputs(&context(&dir)).is_err());
    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(
        dir.join("work/project.json"),
        &compiled("").files["project.json"],
    )
    .unwrap();
    let outputs = docker.outputs(&context(&dir)).unwrap();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].name, "url");
}

/// Unlike anything else the tests write, with what a dotenv value escapes.
const SECRET: &str = "hunter2 \"q\" $5 it's \\";

/// A stand-in that also keeps a copy of the environment file it is given,
/// with its mode, since the file is gone once the stage is over.
fn runner_keeping_env(dir: &Path, cases: &str) -> DockerRunner {
    let d = dir.display();
    let program = stand_in(
        dir,
        "docker",
        &format!(
            "echo \"$@\" >> \"{d}/calls\"\nprev=''\nfor a in \"$@\"; do\n  if [ \"$prev\" = --env-file ]; then cp \"$a\" \"{d}/env\"; stat -c %a \"$a\" > \"{d}/env.mode\" 2>/dev/null || stat -f %Lp \"$a\" > \"{d}/env.mode\"; fi\n  prev=$a\ndone\ncase \"$*\" in\n{cases}\nesac"
        ),
    );
    DockerRunner {
        program: program.to_str().unwrap().to_owned(),
        extra_args: Vec::new(),
    }
}

/// The calls with the environment file's path, which differs every run,
/// replaced by `<env>`.
fn calls_with_env(dir: &Path) -> Vec<String> {
    calls(dir)
        .into_iter()
        .map(|call| match call.find("--env-file ") {
            Some(at) => {
                let rest = &call[at + "--env-file ".len()..];
                let end = rest.find(' ').unwrap_or(rest.len());
                format!("{}--env-file <env>{}", &call[..at], &rest[end..])
            }
            None => call,
        })
        .collect()
}

#[test]
fn secrets_reach_compose_in_a_private_environment_file_for_the_stage_alone() {
    let dir = scratch(TMP, "secrets");
    let log = KeptLog::default();
    let docker = runner_keeping_env(&dir, &format!("{CONFIG}\n{RUNNING}\n{UP}\n{READS}"));
    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", SECRET)]);
    let applied = docker.apply(&context, &compiled(""), &log).unwrap();
    assert_eq!(applied.changes.len(), 0);

    let all = calls_with_env(&dir);
    // Every reading of the file and the up are given the file; reading the
    // outputs is not, as Compose finds the containers by their labels.
    assert_eq!(
        all,
        [
            "compose --env-file <env> config --format json",
            "compose --env-file <env> config --hash *",
            "ps --all --format <containers>",
            "network ls --format {{.Name}}",
            "volume ls --format {{.Name}}",
            "compose --env-file <env> up --detach --remove-orphans",
            "compose port --protocol tcp app 80",
            "compose ps --all --quiet db",
            "inspect 4f1c9a",
            "compose ps --all --quiet db",
            "inspect 4f1c9a",
        ]
    );
    // The file is Compose's dotenv: the variable, and the value in double
    // quotes with its own `$` doubled so Compose leaves it alone.
    assert_eq!(
        fs::read_to_string(dir.join("env")).unwrap(),
        "meridian_secret_db_password=\"hunter2 \\\"q\\\" $$5 it's \\\\\"\n"
    );
    // Windows has no mode; there the file's directory is private to the user.
    #[cfg(unix)]
    assert_eq!(
        fs::read_to_string(dir.join("env.mode")).unwrap().trim(),
        "600"
    );
    // It is gone, and the value is in nothing else.
    let path = calls(&dir)
        .iter()
        .find_map(|c| {
            c.find("--env-file ").map(|at| {
                c[at + "--env-file ".len()..]
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned()
            })
        })
        .unwrap();
    assert!(!Path::new(&path).exists(), "{path} is still there");
    assert!(!fs::read_to_string(dir.join("calls"))
        .unwrap()
        .contains("hunter2"));
    assert!(
        !log.text().join("\n").contains("hunter2"),
        "{:?}",
        log.text()
    );
    for entry in fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert!(
                !fs::read_to_string(&path).unwrap().contains("hunter2"),
                "{} holds the secret",
                path.display()
            );
        }
    }
}

#[test]
fn a_plan_with_secrets_gives_compose_the_file_too_and_removes_it_after() {
    let dir = scratch(TMP, "secrets-plan");
    let docker = runner_keeping_env(&dir, &format!("{CONFIG}\n{RUNNING}"));
    let mut context = context(&dir);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", SECRET)]);
    docker
        .plan(&context, &compiled(""), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls_with_env(&dir)[..2],
        [
            "compose --env-file <env> config --format json",
            "compose --env-file <env> config --hash *",
        ]
    );
    assert!(dir.join("env").exists());
}

#[test]
fn a_destroy_takes_the_project_down_with_its_volumes() {
    let dir = scratch(TMP, "destroy");
    let runner = runner(&dir, &format!("{CONFIG}\n{RUNNING}"));
    assert!(runner.destroys());
    let removed = runner
        .destroy(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap();
    let mut expected = SURVEY.to_vec();
    expected.push("compose down --volumes --remove-orphans");
    assert_eq!(calls(&dir), expected);
    assert_eq!(
        shown(&removed),
        [
            "delete db: container",
            "delete app: container",
            "delete backend: network shop_backend",
            "delete data: volume shop_data",
        ]
    );
}

#[test]
fn a_destroy_preview_changes_nothing_and_an_empty_project_is_not_taken_down() {
    let dir = scratch(TMP, "destroy-preview");
    let plan = runner(&dir, &format!("{CONFIG}\n{RUNNING}"))
        .plan_destroy(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap();
    assert_eq!(calls(&dir), SURVEY);
    assert_eq!(plan.changes.len(), 4);

    let dir = scratch(TMP, "destroy-nothing");
    let removed = runner(&dir, CONFIG)
        .destroy(&context(&dir), &compiled(""), &KeptLog::default())
        .unwrap();
    assert!(removed.is_empty());
    assert_eq!(calls(&dir), SURVEY);
}

#[test]
fn inspect_reads_a_services_container_and_compares_the_file() {
    let dir = scratch(TMP, "inspect");
    let inspect = r#"*"inspect shop-db-1"*) echo '[{"Config":{"Image":"postgres:16","Env":["POSTGRES_PASSWORD=x"]},"RestartCount":2,"State":{"Status":"running","Health":{"Status":"healthy"}}}]' ;;"#;
    let cases = format!("{CONFIG}\n{RUNNING}\n{inspect}");
    let runner = runner(&dir, &cases);
    let read = runner
        .inspect(&context(&dir), &compiled(""), "db", "")
        .unwrap();
    let mut expected: Vec<String> = SURVEY.iter().map(|s| s.to_string()).collect();
    expected.push("inspect shop-db-1".to_owned());
    assert_eq!(calls(&dir), expected);
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(true));
    assert_eq!(read.actual.get("image"), Some(&Value::from("postgres:16")));
    assert_eq!(read.actual.get("health"), Some(&Value::from("healthy")));
    assert!(!read.actual.contains_key("Env"));
    assert_eq!(
        read.command,
        "docker compose ps --all db; docker inspect shop-db-1"
    );

    let read = runner
        .inspect(&context(&dir), &compiled(""), "data", "")
        .unwrap();
    assert!(read.found);
    assert_eq!(read.in_desired_state, Some(true));
}

#[test]
fn inspect_of_a_service_with_no_container_is_not_found() {
    let dir = scratch(TMP, "inspect-none");
    let read = runner(&dir, CONFIG)
        .inspect(&context(&dir), &compiled(""), "app", "")
        .unwrap();
    assert!(!read.found);
    assert_eq!(read.in_desired_state, Some(false));
    let error = runner(&dir, CONFIG)
        .inspect(&context(&dir), &compiled(""), "nothing", "")
        .unwrap_err();
    assert_eq!(
        error.msg,
        "compose.yaml has no service, network or volume \"nothing\""
    );
}

/// The same project, with one service adopted by replacing it.
fn compiled_adopting(service: &str) -> Artifact {
    let project = format!(
        r#"{{"name":"shop","host":"","context":"","services":["app","db"],"adopt":["{service}"],"outputs":[]}}"#
    );
    artifact(&[("compose.yaml", "name: shop\n"), ("project.json", &project)])
}

#[test]
fn an_adopted_service_is_removed_before_the_project_comes_up() {
    let dir = scratch(TMP, "adopt");
    let log = KeptLog::default();
    let applied = runner(&dir, &format!("{CONFIG}\n{RUNNING}\n{UP}\n{READS}"))
        .apply(&context(&dir), &compiled_adopting("app"), &log)
        .unwrap();
    // The plan reports the adoption, not a plain recreate.
    assert_eq!(
        shown(&applied.changes),
        ["adopt app: its existing container is replaced, taking it over (image app:1)"]
    );
    let all = calls(&dir);
    assert_eq!(all[..SURVEY.len()], SURVEY);
    let rm = all
        .iter()
        .position(|c| c == "compose rm --stop --force app")
        .unwrap_or_else(|| panic!("no removal of the adopted container: {all:#?}"));
    let up = all
        .iter()
        .position(|c| c.starts_with("compose up"))
        .expect("an up");
    assert!(
        rm < up,
        "the existing container is removed before the project comes up: {all:#?}"
    );
    assert!(
        !all.iter().any(|c| c.contains(" rm ") && c.contains("db")),
        "only the adopted service is removed: {all:#?}"
    );
}
