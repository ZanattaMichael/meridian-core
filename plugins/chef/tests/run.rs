//! The running half, against stand-ins for `chef-client` and `chef-run`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, secrets, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, SecretRef};
use meridian_target_chef::{ChefRunner, Transport};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        (
            "cookbooks/meridian_web/metadata.rb",
            "name 'meridian_web'\n",
        ),
        ("cookbooks/meridian_web/recipes/default.rb", "# empty\n"),
    ])
}

fn context(dir: &Path, host: &str) -> RunContext {
    RunContext {
        document: "web".into(),
        host: host.into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

fn runner(dir: &Path, client: &str, run: &str) -> ChefRunner {
    let record = |name: &str, body: &str| {
        stand_in(
            dir,
            name,
            &format!("echo \"$@\" > \"{}/{name}.args\"\n{body}", dir.display()),
        )
        .to_str()
        .unwrap()
        .to_owned()
    };
    ChefRunner {
        client: record("chef-client", client),
        run: record("chef-run", run),
        transport: None,
        extra_args: Vec::new(),
    }
}

fn args(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(format!("{name}.args")))
        .unwrap()
        .trim()
        .to_owned()
}

const REPORT: &str = "cat <<'OUT'
Recipe: meridian_web::default
  * apt_package[nginx_pkg] action install
    - Would install version 1.24.0 of package nginx
  * service[nginx_svc] action start (up to date)
OUT";

#[test]
fn on_the_node_itself_a_plan_is_a_why_run() {
    let dir = scratch(TMP, "local-plan");
    let plan = runner(&dir, REPORT, "exit 9")
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        args(&dir, "chef-client"),
        "--local-mode --no-color --force-formatter --override-runlist recipe[meridian_web] --why-run"
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(shown, ["update nginx_pkg: install"]);
    assert!(!plan.unknown);
    assert!(dir
        .join("work/cookbooks/meridian_web/recipes/default.rb")
        .exists());
}

#[test]
fn on_the_node_itself_an_apply_is_a_local_mode_run() {
    let dir = scratch(TMP, "local-apply");
    let mut chef = runner(&dir, REPORT, "exit 9");
    chef.extra_args = vec!["--chef-license".into(), "accept".into()];
    let applied = chef
        .apply(&context(&dir, ""), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        args(&dir, "chef-client"),
        "--local-mode --no-color --force-formatter --override-runlist recipe[meridian_web] --chef-license accept"
    );
    assert_eq!(applied.changes.len(), 1);
    assert!(applied.outputs.is_empty());
}

#[test]
fn another_machine_is_converged_with_chef_run() {
    let dir = scratch(TMP, "remote-apply");
    let log = KeptLog::default();
    let applied = runner(&dir, "exit 9", "echo converged")
        .apply(&context(&dir, "web01.example.com"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        args(&dir, "chef-run"),
        "web01.example.com cookbooks/meridian_web"
    );
    assert!(applied.changes.is_empty());
    assert!(log
        .text()
        .iter()
        .any(|l| l == "chef-run does not report which resources it changed"));
}

#[test]
fn a_plan_for_another_machine_is_unknown_and_runs_nothing() {
    let dir = scratch(TMP, "remote-plan");
    let plan = runner(&dir, "exit 9", "exit 9")
        .plan(
            &context(&dir, "web01.example.com"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(plan.unknown);
    assert!(!plan.in_sync());
    assert!(!dir.join("chef-run.args").exists());
}

#[test]
fn the_transport_can_be_told() {
    let dir = scratch(TMP, "told");
    let mut chef = runner(&dir, REPORT, "exit 9");
    chef.transport = Some(Transport::Local);
    let plan = chef
        .plan(
            &context(&dir, "web01.example.com"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(plan.changes.len(), 1);
}

#[test]
fn a_failed_run_keeps_what_chef_said_and_what_it_had_changed() {
    let dir = scratch(TMP, "failed");
    let body = format!(
        "{REPORT}\necho 'FATAL: Chef::Exceptions::Service: service[nginx_svc]' >&2\nexit 1"
    );
    let err = runner(&dir, &body, "exit 9")
        .apply(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "chef-client could not converge the cookbook");
    assert!(
        err.output.contains("Chef::Exceptions::Service"),
        "{}",
        err.output
    );
    assert_eq!(err.changes.len(), 1);
}

#[test]
fn a_host_chef_run_would_read_as_an_option_is_refused_before_it_runs() {
    // Issue 134: the core refuses such a host first; the runner is the last
    // thing between it and the command line.
    let dir = scratch(TMP, "hostile-host");
    let err = runner(&dir, "exit 9", "echo converged")
        .apply(
            &context(&dir, "-oProxyCommand=touch /tmp/pwned"),
            &compiled(),
            &KeptLog::default(),
        )
        .expect_err("refused");
    assert!(
        err.to_string()
            .contains("chef-run cannot be given this host"),
        "{err}"
    );
    assert!(!dir.join("chef-run.args").exists());
}

// ---- secrets ------------------------------------------------------------

/// A stand-in body that keeps what a client given `--json-attributes` and
/// `--config` could read: both files, the attributes file's mode, and the
/// environment, then prints `report`.
fn keeping_attributes(dir: &Path, report: &str) -> String {
    format!(
        "env > \"{dir}/env\"
while [ $# -gt 0 ]; do
  case \"$1\" in
    --json-attributes)
      echo \"$2\" > \"{dir}/attributes-path\"
      cp \"$2\" \"{dir}/attributes.json\"
      stat -c %a \"$2\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$2\" > \"{dir}/mode\"
      shift;;
    --config) cp \"$2\" \"{dir}/client.rb\"; shift;;
  esac
  shift
done
{report}",
        dir = dir.display()
    )
}

fn with_secret(dir: &Path, host: &str) -> RunContext {
    let mut context = context(dir, host);
    context.secrets = secrets(&[(SecretRef::new(1), "db_password", "hunter2\"\\")]);
    context
}

#[test]
fn secrets_reach_chef_client_as_node_attributes_in_a_private_file_and_nowhere_else() {
    let dir = scratch(TMP, "secrets-local");
    let log = KeptLog::default();
    runner(&dir, &keeping_attributes(&dir, REPORT), "exit 9")
        .apply(&with_secret(&dir, "localhost"), &compiled(), &log)
        .unwrap();

    assert_eq!(
        fs::read_to_string(dir.join("attributes.json")).unwrap(),
        "{\"meridian_secret_db_password\":\"hunter2\\\"\\\\\"}\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    // The configuration beside it names the repository, and keeps the node
    // object the client writes back (attributes and all) in the private
    // directory rather than the work directory.
    let config = fs::read_to_string(dir.join("client.rb")).unwrap();
    let attributes_path = fs::read_to_string(dir.join("attributes-path")).unwrap();
    let private_dir = Path::new(attributes_path.trim()).parent().unwrap();
    let work = meridian_sdk::tool::absolute_path(&dir.join("work"));
    // Both paths are Ruby single-quoted strings, so a backslash in one (a
    // Windows path) is written doubled.
    let ruby = |path: &Path| path.to_string_lossy().replace('\\', "\\\\");
    assert_eq!(
        config,
        format!(
            "chef_repo_path '{}'\nnode_path '{}'\n",
            ruby(&work),
            ruby(private_dir)
        )
    );
    let args = args(&dir, "chef-client");
    assert!(args.contains("--config "), "{args}");
    assert!(args.contains("--json-attributes "), "{args}");
    assert!(args.starts_with("--local-mode --no-color --force-formatter --override-runlist recipe[meridian_web] --config "), "{args}");
    assert!(!args.contains("hunter2"), "{args}");
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("hunter2"));
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
    assert!(
        !private_dir.exists(),
        "{} was left behind",
        private_dir.display()
    );
}

#[test]
fn a_why_run_with_secrets_takes_the_same_files_and_a_run_without_takes_none() {
    let dir = scratch(TMP, "secrets-plan");
    runner(&dir, &keeping_attributes(&dir, REPORT), "exit 9")
        .plan(&with_secret(&dir, ""), &compiled(), &KeptLog::default())
        .unwrap();
    let with = args(&dir, "chef-client");
    assert!(
        with.contains("--json-attributes ") && with.ends_with("--why-run"),
        "{with}"
    );

    let dir = scratch(TMP, "no-secrets");
    runner(&dir, REPORT, "exit 9")
        .plan(&context(&dir, ""), &compiled(), &KeptLog::default())
        .unwrap();
    let plain = args(&dir, "chef-client");
    assert!(
        !plain.contains("--json-attributes") && !plain.contains("--config"),
        "{plain}"
    );
}

#[test]
fn chef_run_is_refused_a_secret_because_it_would_put_it_on_the_command_line() {
    let dir = scratch(TMP, "secrets-remote");
    let log = KeptLog::default();
    let err = runner(&dir, "exit 9", "echo converged")
        .apply(&with_secret(&dir, "web01.example.com"), &compiled(), &log)
        .unwrap_err();
    assert!(
        err.msg.starts_with("chef-run has no private way to hand a secret to web01.example.com: it takes attributes on its command line only"),
        "{}",
        err.msg
    );
    assert!(err.msg.contains("local transport"), "{}", err.msg);
    assert!(!err.msg.contains("hunter2"));
    assert!(!dir.join("chef-run.args").exists(), "chef-run was started");
    // A plan for that host says it is unknown, as it does without secrets,
    // and runs nothing either.
    let plan = runner(&dir, "exit 9", "echo converged")
        .plan(
            &with_secret(&dir, "web01.example.com"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert!(plan.unknown);
}
