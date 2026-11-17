//! The running half, against stand-ins for `chef-client` and `chef-run`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner};
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
