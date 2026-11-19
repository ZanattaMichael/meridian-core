//! The running half, against stand-ins for `puppet` and `bolt`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{
    agent_stand_ins, artifact, credential, scratch, secrets, stand_in, KeptLog,
};
use meridian_sdk::{AgentPrograms, Artifact, RunContext, Runner, SecretRef};
use meridian_target_puppet::{PuppetRunner, Transport};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn compiled() -> Artifact {
    artifact(&[
        ("manifests/init.pp", "class meridian_web {\n}\n"),
        ("site.pp", "node 'web01' {\n  include meridian_web\n}\n"),
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

fn runner(dir: &Path, puppet: &str, bolt: &str) -> PuppetRunner {
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
    PuppetRunner {
        puppet: record("puppet", puppet),
        bolt: record("bolt", bolt),
        transport: None,
        extra_args: Vec::new(),
        agent: AgentPrograms::default(),
    }
}

const NOOP: &str = "cat <<'OUT'
Notice: Compiled catalog for web01 in environment production in 0.05 seconds
Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: current_value 'absent', should be 'present' (noop)
Notice: /Stage[main]/Meridian_web/File[app_conf]/content: current_value '{sha256}aa', should be '{sha256}bb' (noop)
Notice: /Stage[main]/Meridian_web/Service[app_svc]: Would have triggered 'refresh' from 1 event
Notice: Class[Meridian_web]: Would have triggered 'refresh' from 2 events
Notice: Applied catalog in 0.03 seconds
OUT";

#[test]
fn on_the_node_itself_a_plan_is_puppet_apply_noop() {
    let dir = scratch(TMP, "local-plan");
    let log = KeptLog::default();
    let plan = runner(&dir, NOOP, "exit 9")
        .plan(&context(&dir, "localhost"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap().trim(),
        "apply --detailed-exitcodes --color=false --noop apply.pp"
    );
    assert_eq!(
        fs::read_to_string(dir.join("work/apply.pp")).unwrap(),
        "class meridian_web {\n}\n\ninclude meridian_web\n"
    );
    let shown: Vec<String> = plan.changes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shown,
        [
            "create app_pkg: ensure: current_value 'absent', should be 'present' (noop)",
            "update app_conf: content: current_value '{sha256}aa', should be '{sha256}bb' (noop)",
            "update app_svc: Would have triggered 'refresh' from 1 event",
        ]
    );
    assert!(!plan.unknown);
    assert_eq!(log.text().len(), 6);
}

#[test]
fn exit_status_two_means_changed_and_four_means_failed() {
    let dir = scratch(TMP, "local-apply");
    let body = "echo \"Notice: /Stage[main]/Meridian_web/Package[app_pkg]/ensure: created\"";
    let applied = runner(&dir, &format!("{body}\nexit 2"), "exit 9")
        .apply(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    assert_eq!(
        applied.changes[0].to_string(),
        "create app_pkg: ensure: created"
    );
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap().trim(),
        "apply --detailed-exitcodes --color=false apply.pp"
    );

    let err = runner(
        &dir,
        &format!("{body}\necho 'Error: Could not start Service[app_svc]' >&2\nexit 6"),
        "exit 9",
    )
    .apply(
        &context(&dir, "localhost"),
        &compiled(),
        &KeptLog::default(),
    )
    .unwrap_err();
    assert_eq!(err.msg, "puppet apply reported a failed resource");
    assert_eq!(err.exit_code, Some(6));
    assert_eq!(err.changes.len(), 1);
    assert!(err.output.contains("Could not start Service[app_svc]"));
}

const BOLT_REPORT: &str = r#"cat <<'OUT'
{"items":[{"target":"web01","action":"apply","object":null,"status":"success","value":{"report":{"status":"changed","resource_statuses":{"Package[app_pkg]":{"changed":true,"events":[{"property":"ensure","message":"created","status":"success"}]},"Service[app_svc]":{"changed":false,"events":[]}}}}}],"target_count":1,"elapsed_time":3}
OUT"#;

#[test]
fn another_host_is_reached_with_bolt() {
    let dir = scratch(TMP, "bolt-apply");
    let log = KeptLog::default();
    let applied = runner(&dir, "exit 9", BOLT_REPORT)
        .apply(&context(&dir, "web01"), &compiled(), &log)
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("bolt.args")).unwrap().trim(),
        "apply apply.pp --targets web01 --format json"
    );
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(
        applied.changes[0].to_string(),
        "create app_pkg: ensure: created"
    );
    assert_eq!(log.text(), ["create app_pkg: ensure: created"]);
}

#[test]
fn a_bolt_report_that_cannot_be_read_is_not_taken_for_in_sync() {
    let dir = scratch(TMP, "bolt-unreadable");
    let plan = runner(&dir, "exit 9", "echo 'Finished on web01'")
        .plan(&context(&dir, "web01"), &compiled(), &KeptLog::default())
        .unwrap();
    assert!(plan.unknown);
    assert!(!plan.in_sync());
    assert_eq!(
        fs::read_to_string(dir.join("bolt.args")).unwrap().trim(),
        "apply apply.pp --targets web01 --format json --noop"
    );
}

#[test]
fn the_transport_can_be_forced_and_a_bolt_failure_keeps_its_output() {
    let dir = scratch(TMP, "bolt-failed");
    let mut forced = runner(
        &dir,
        "exit 9",
        "echo '{\"items\":[]}'\necho 'Failed to connect to web01' >&2\nexit 2",
    );
    forced.transport = Some(Transport::Bolt);
    let err = forced
        .apply(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "bolt apply failed");
    assert!(err.output.contains("Failed to connect to web01"));
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let err = runner(&dir, "exit 9", "exit 9")
        .plan(
            &context(&dir, "localhost"),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(err
        .msg
        .starts_with("the artifact has no manifests/init.pp and site.pp"));
}

#[test]
fn a_target_bolt_would_read_as_an_option_is_refused_before_it_runs() {
    // Issue 134: the core refuses such a host first; the runner is the last
    // thing between it and the command line.
    let dir = scratch(TMP, "hostile-host");
    let err = runner(&dir, "exit 9", BOLT_REPORT)
        .apply(
            &context(&dir, "-oProxyCommand=touch /tmp/pwned"),
            &compiled(),
            &KeptLog::default(),
        )
        .expect_err("refused");
    assert!(
        err.to_string().contains("bolt cannot be given this target"),
        "{err}"
    );
    assert!(!dir.join("bolt.args").exists());
}

// ---- secrets ------------------------------------------------------------

/// A stand-in body that keeps what a tool given `--hiera_config` (or Bolt's
/// `--hiera-config`) could read: the configuration, the data file beside it
/// and its mode, and the environment, then prints `report`.
fn keeping_hiera(dir: &Path, report: &str) -> String {
    format!(
        "env > \"{dir}/env\"
while [ $# -gt 0 ]; do
  case \"$1\" in
    --hiera_config|--hiera-config)
      echo \"$2\" > \"{dir}/config-path\"
      cp \"$2\" \"{dir}/hiera.yaml\"
      cp \"$(dirname \"$2\")/common.yaml\" \"{dir}/common.yaml\"
      stat -c %a \"$(dirname \"$2\")/common.yaml\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$(dirname \"$2\")/common.yaml\" > \"{dir}/mode\"
      shift;;
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
fn secrets_reach_puppet_apply_as_hiera_data_in_a_private_directory_and_nowhere_else() {
    let dir = scratch(TMP, "secrets-local");
    let log = KeptLog::default();
    runner(&dir, &keeping_hiera(&dir, NOOP), "exit 9")
        .apply(&with_secret(&dir, "localhost"), &compiled(), &log)
        .unwrap();

    // The data file holds the value under its key, as a JSON string, which
    // YAML reads the same.
    assert_eq!(
        fs::read_to_string(dir.join("common.yaml")).unwrap(),
        "---\n\"meridian_secret_db_password\": \"hunter2\\\"\\\\\"\n"
    );
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    // The configuration beside it names that directory and nothing else.
    let config = fs::read_to_string(dir.join("hiera.yaml")).unwrap();
    let config_path = fs::read_to_string(dir.join("config-path")).unwrap();
    let private_dir = Path::new(config_path.trim()).parent().unwrap();
    assert!(config.contains("version: 5\n"), "{config}");
    assert!(
        config.contains(&format!(
            "datadir: {}\n",
            meridian_sdk::Value::String(private_dir.to_string_lossy().into_owned()).canonical()
        )),
        "{config}"
    );
    assert!(config.contains("path: common.yaml\n"), "{config}");
    assert!(!config.contains("hunter2"), "{config}");
    // The command line named the configuration and nothing more, and the
    // environment held nothing.
    let args = fs::read_to_string(dir.join("puppet.args")).unwrap();
    assert!(args.contains("--hiera_config "), "{args}");
    assert!(!args.contains("hunter2"), "{args}");
    assert!(!fs::read_to_string(dir.join("env"))
        .unwrap()
        .contains("hunter2"));
    assert!(!log.text().iter().any(|l| l.contains("hunter2")));
    // Neither the work directory nor the private one holds it now.
    assert!(
        !private_dir.exists(),
        "{} was left behind",
        private_dir.display()
    );
    assert!(!fs::read_to_string(dir.join("work/apply.pp"))
        .unwrap()
        .contains("hunter2"));
}

#[test]
fn over_bolt_the_hiera_configuration_is_passed_as_bolt_spells_it() {
    let dir = scratch(TMP, "secrets-bolt");
    runner(&dir, "exit 9", &keeping_hiera(&dir, BOLT_REPORT))
        .apply(
            &with_secret(&dir, "web01"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    let args = fs::read_to_string(dir.join("bolt.args")).unwrap();
    assert!(args.contains("--hiera-config "), "{args}");
    assert!(!args.contains("--hiera_config"), "{args}");
    assert!(!args.contains("hunter2"), "{args}");
    assert_eq!(
        fs::read_to_string(dir.join("common.yaml")).unwrap(),
        "---\n\"meridian_secret_db_password\": \"hunter2\\\"\\\\\"\n"
    );
    let config_path = fs::read_to_string(dir.join("config-path")).unwrap();
    assert!(!Path::new(config_path.trim()).exists());
}

#[test]
fn a_run_without_secrets_names_no_hiera_configuration() {
    let dir = scratch(TMP, "no-secrets");
    runner(&dir, NOOP, "exit 9")
        .plan(
            &context(&dir, "localhost"),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap();
    let args = fs::read_to_string(dir.join("puppet.args")).unwrap();
    assert!(!args.contains("hiera"), "{args}");
}

// ---- credentials ----------------------------------------------------------

const KEY: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-9fXw\n-----END OPENSSH PRIVATE KEY-----";

/// A `bolt` stand-in body that keeps its environment and a copy of the
/// inventory it is given, with the inventory's mode, then prints the report.
fn keeping_inventory(dir: &Path) -> String {
    format!(
        "env > \"{dir}/env\"
while [ $# -gt 0 ]; do
  case \"$1\" in
    --inventoryfile)
      echo \"$2\" > \"{dir}/inventory-path\"
      cp \"$2\" \"{dir}/inventory.yaml\"
      stat -c %a \"$2\" > \"{dir}/mode\" 2>/dev/null || stat -f %Lp \"$2\" > \"{dir}/mode\"
      shift;;
  esac
  shift
done
{BOLT_REPORT}",
        dir = dir.display()
    )
}

fn with_credential(dir: &Path, host: &str, kind: &str, fields: &[(&str, &str)]) -> RunContext {
    let mut context = context(dir, host);
    context.credential = Some(credential(kind, "deploy", fields));
    context
}

#[test]
fn a_credentials_key_reaches_bolt_through_an_agent_and_its_user_in_a_private_inventory() {
    let dir = scratch(TMP, "credential-agent");
    let mut puppet = runner(&dir, "exit 9", &keeping_inventory(&dir));
    puppet.agent = agent_stand_ins(&dir);
    let log = KeptLog::default();
    let applied = puppet
        .apply(
            &with_credential(
                &dir,
                "web01",
                "ssh",
                &[("privateKey", KEY), ("passphrase", "open sesame")],
            ),
            &compiled(),
            &log,
        )
        .unwrap();
    assert_eq!(applied.changes.len(), 1);

    let args = fs::read_to_string(dir.join("bolt.args")).unwrap();
    assert!(
        args.starts_with("apply apply.pp --targets web01 --format json --inventoryfile "),
        "{args}"
    );
    assert!(!args.contains("deploy"), "{args}");
    assert_eq!(
        fs::read_to_string(dir.join("inventory.yaml")).unwrap(),
        "---\n# Written by Meridian for this run, and removed with it.\n\
         targets:\n  - uri: \"web01\"\n    config:\n      transport: ssh\n      ssh:\n        user: \"deploy\"\n"
    );
    // Bolt was told the agent's socket and nothing else of the credential.
    let agent_args = fs::read_to_string(dir.join("ssh-agent.args")).unwrap();
    let socket = agent_args
        .trim()
        .strip_prefix("-D -a ")
        .expect("ssh-agent -D -a <socket>");
    let env = fs::read_to_string(dir.join("env")).unwrap();
    assert!(env.contains(&format!("SSH_AUTH_SOCK={socket}")), "{env}");
    assert!(
        !env.contains("not-a-real-key") && !env.contains("sesame"),
        "{env}"
    );
    assert_eq!(
        fs::read_to_string(dir.join("added-key")).unwrap(),
        format!("{KEY}\n")
    );
    assert_eq!(
        fs::read_to_string(dir.join("added-passphrase")).unwrap(),
        "open sesame\n"
    );
    // The inventory and the agent's directory are gone, and nothing printed
    // holds the key.
    let inventory_path = fs::read_to_string(dir.join("inventory-path")).unwrap();
    assert!(!Path::new(inventory_path.trim()).exists());
    assert!(!Path::new(socket).parent().unwrap().exists());
    assert!(!log
        .text()
        .iter()
        .any(|l| l.contains("not-a-real-key") || l.contains("sesame")));
}

#[test]
fn a_credentials_password_is_in_the_private_inventory_and_nowhere_else() {
    let dir = scratch(TMP, "credential-password");
    let mut puppet = runner(&dir, "exit 9", &keeping_inventory(&dir));
    puppet.agent = agent_stand_ins(&dir);
    let log = KeptLog::default();
    puppet
        .plan(
            &with_credential(
                &dir,
                "web01",
                "usernamePassword",
                &[("password", "hunter2\"\\")],
            ),
            &compiled(),
            &log,
        )
        .unwrap();
    assert!(fs::read_to_string(dir.join("inventory.yaml"))
        .unwrap()
        .ends_with(
            "      ssh:\n        user: \"deploy\"\n        password: \"hunter2\\\"\\\\\"\n"
        ));
    #[cfg(unix)]
    assert_eq!(fs::read_to_string(dir.join("mode")).unwrap().trim(), "600");
    assert!(
        !dir.join("ssh-agent.args").exists(),
        "an agent was started for no key"
    );
    let shown = format!(
        "{}\n{}\n{}",
        fs::read_to_string(dir.join("bolt.args")).unwrap(),
        fs::read_to_string(dir.join("env")).unwrap(),
        log.text().join("\n")
    );
    assert!(!shown.contains("hunter2"), "{shown}");
    for entry in fs::read_dir(dir.join("work")).unwrap() {
        let path = entry.unwrap().path();
        if let Ok(text) = fs::read_to_string(&path) {
            assert!(
                !text.contains("hunter2"),
                "{} holds the password",
                path.display()
            );
        }
    }
}

#[test]
fn a_credential_for_this_machine_is_refused() {
    let dir = scratch(TMP, "credential-local");
    let err = runner(&dir, NOOP, "exit 9")
        .plan(
            &with_credential(&dir, "localhost", "ssh", &[("privateKey", KEY)]),
            &compiled(),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(
        err.msg.contains(
            "the host is this machine, which puppet apply configures as the user running Meridian"
        ),
        "{}",
        err.msg
    );
    assert!(!dir.join("puppet.args").exists(), "puppet was started");
}

#[test]
fn a_debug_level_reaches_puppet_apply_and_bolt_each_as_its_own_switch() {
    let dir = scratch(TMP, "debug");
    let debug = |host: &str, level: u32| RunContext {
        debug_level: level,
        ..context(&dir, host)
    };

    runner(&dir, NOOP, "exit 9")
        .plan(&debug("localhost", 2), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("puppet.args")).unwrap().trim(),
        "apply --detailed-exitcodes --color=false --debug --noop apply.pp"
    );

    // Bolt logs to standard error; the report on standard output is read
    // as before.
    let bolt = format!("echo 'DEBUG  [Bolt::Executor] Running apply' >&2\n{BOLT_REPORT}");
    let log = KeptLog::default();
    let applied = runner(&dir, "exit 9", &bolt)
        .apply(&debug("web01", 3), &compiled(), &log)
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("bolt.args")).unwrap().trim(),
        "apply apply.pp --targets web01 --format json --log-level trace"
    );
    assert_eq!(applied.changes.len(), 1);
    assert!(log.text().iter().any(|l| l.contains("Bolt::Executor")));

    runner(&dir, "exit 9", BOLT_REPORT)
        .apply(&debug("web01", 0), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("bolt.args")).unwrap().trim(),
        "apply apply.pp --targets web01 --format json"
    );
}

#[test]
fn bolt_applys_own_report_is_a_list_of_results_and_is_read_too() {
    // What `bolt apply --format json` prints: the results, unwrapped.
    let dir = scratch(TMP, "bolt-list");
    let list = r#"cat <<'OUT'
[{"target":"web01","action":"apply","object":null,"status":"success","value":{"report":{"status":"changed","resource_statuses":{"Package[app_pkg]":{"changed":true,"events":[{"property":"ensure","message":"created","status":"success"}]}}}}}]
OUT"#;
    let applied = runner(&dir, "exit 9", list)
        .apply(&context(&dir, "web01"), &compiled(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        applied
            .changes
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>(),
        ["create app_pkg: ensure: created"]
    );
}
