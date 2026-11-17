//! The running half, against stand-ins for `podman`, `systemctl`, Quadlet's
//! generator and `ssh`. The scripts themselves run under the real `sh`.

use std::fs;
use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{artifact, graph_from_text, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, Data, Emitter, RunContext, Runner, Value};
use meridian_target_podman::{Podman, PodmanRunner, Scope, META_PATH};

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

const DATA: &str = "    - id: data\n      type: volume\n";
const WEB: &str = "    - id: web\n      type: container\n      params:\n        Image: quay.io/x/web:1\n        Volume: ${data}:/srv\n";
const OUTPUTS: &str = "  outputs:\n    - name: url\n      value: http://localhost:8080/\n    - name: files\n      value: ${data.inspect.Mountpoint}/www\n";

/// A document with the given resources and outputs, compiled.
fn compiled(resources: &str, outputs: &str) -> Artifact {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: shop\nspec:\n  target: podman\n  resources:\n{resources}{outputs}"
    );
    Podman
        .emit(&graph_from_text("shop.yaml", &text, "podman"), &Data::new())
        .unwrap()
        .artifact
}

fn shop() -> Artifact {
    compiled(&format!("{DATA}{WEB}"), OUTPUTS)
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "shop".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// A host made of stand-ins: a unit directory, a `systemctl` that keeps each
/// service's state as a file, a generator that knows the unit kinds, and a
/// `podman` that answers what the tests ask.
fn host(dir: &Path) -> PodmanRunner {
    let d = dir.display();
    for sub in ["units", "active", "broken"] {
        fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let systemctl = stand_in(
        dir,
        "systemctl",
        &format!(
            r#"echo "$@" >> "{d}/systemctl.calls"
if [ "$1" = --user ]; then shift; fi
verb=$1; shift
case "$verb" in
  is-active)
    quiet=0
    if [ "$1" = --quiet ]; then quiet=1; shift; fi
    if [ -e "{d}/active/$1" ]; then
      if [ "$quiet" = 0 ]; then echo active; fi
      exit 0
    fi
    if [ "$quiet" = 0 ]; then echo inactive; fi
    exit 3 ;;
  start|restart)
    for s in "$@"; do
      if [ -e "{d}/broken/$s" ]; then echo "Job for $s failed." >&2; exit 1; fi
      touch "{d}/active/$s"
    done ;;
  stop)
    for s in "$@"; do rm -f "{d}/active/$s"; done ;;
esac"#
        ),
    );
    let generator = stand_in(
        dir,
        "quadlet",
        &format!(
            r#"echo "$@" >> "{d}/quadlet.calls"
status=0
for f in "$QUADLET_UNIT_DIRS"/*; do
  name=$(basename "$f"); stem=${{name%.*}}
  echo "quadlet-generator[7]: Loading source unit file $f" >&2
  if grep -q '^Bogus=' "$f"; then
    echo "quadlet-generator[7]: converting \"$name\": unsupported key 'Bogus' in group 'Container' in $f" >&2
    status=1
    continue
  fi
  if grep -q '^Image=short$' "$f"; then
    echo "quadlet-generator[7]: Warning: $name specifies an image that is not fully qualified" >&2
  fi
  case "$name" in
    *.container|*.kube) echo "---$stem.service---" ;;
    *.volume) echo "---$stem-volume.service---" ;;
    *.network) echo "---$stem-network.service---" ;;
    *.image) echo "---$stem-image.service---" ;;
    *.pod) if [ ! -e "{d}/old-podman" ]; then echo "---$stem-pod.service---"; fi ;;
  esac
  echo "[Unit]"
done
exit $status"#
        ),
    );
    let podman = stand_in(
        dir,
        "podman",
        &format!(
            r#"echo "$@" >> "{d}/podman.calls"
case "$1" in
  --version) echo "podman version 5.4.0" ;;
  volume)
    if [ -e "{d}/no-volume" ]; then echo "Error: no such volume $5" >&2; exit 125; fi
    echo "/var/lib/volumes/$5/_data" ;;
esac"#
        ),
    );
    let journalctl = stand_in(dir, "journalctl", r#"echo "journal: $*""#);
    let sudo = stand_in(
        dir,
        "sudo",
        &format!(r#"printf '%s\n' "$*" >> "{d}/sudo.calls"; shift; exec "$@""#),
    );
    let path = |p: PathBuf| p.to_str().unwrap().to_owned();
    PodmanRunner {
        podman: path(podman),
        systemctl: path(systemctl),
        journalctl: path(journalctl),
        sudo: path(sudo),
        generator: Some(path(generator)),
        unit_dir: Some(path(dir.join("units"))),
        ..PodmanRunner::default()
    }
}

fn lines(path: PathBuf) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// What `systemctl` was asked to change, without the questions it was asked.
fn systemctl(dir: &Path) -> Vec<String> {
    lines(dir.join("systemctl.calls"))
        .into_iter()
        .filter(|call| !call.contains("is-active"))
        .collect()
}

fn installed(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.join("units"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn shown(changes: &[meridian_sdk::Change]) -> Vec<String> {
    changes.iter().map(ToString::to_string).collect()
}

#[test]
fn a_plan_says_what_an_apply_would_do_and_touches_nothing() {
    let dir = scratch(TMP, "plan");
    let log = KeptLog::default();
    let plan = host(&dir).plan(&context(&dir), &shop(), &log).unwrap();
    assert_eq!(
        shown(&plan.changes),
        [
            "create data: install data.volume and start data-volume.service",
            "create web: install web.container and start web.service",
        ]
    );
    assert!(!plan.unknown);
    assert!(installed(&dir).is_empty());
    assert!(systemctl(&dir).is_empty());
    // The host's own Quadlet read the units, as the user's.
    assert_eq!(lines(dir.join("quadlet.calls")), ["-dryrun -user"]);
    assert!(log.text().is_empty(), "{:?}", log.text());
    // The artifact is on disk for whoever wants to read what was planned.
    assert!(dir.join("work/web.container").exists());
}

#[test]
fn an_apply_installs_the_units_starts_them_and_reports_outputs() {
    let dir = scratch(TMP, "apply");
    let log = KeptLog::default();
    let artifact = shop();
    let applied = host(&dir).apply(&context(&dir), &artifact, &log).unwrap();

    assert_eq!(shown(&applied.changes).len(), 2);
    assert_eq!(installed(&dir), ["data.volume", "web.container"]);
    for file in ["data.volume", "web.container"] {
        assert_eq!(
            fs::read_to_string(dir.join("units").join(file)).unwrap(),
            artifact.files[file],
            "{file} is installed byte for byte"
        );
    }
    // The runner's own file stays in the work directory.
    assert!(!dir.join("units").join(META_PATH).exists());
    assert_eq!(
        systemctl(&dir),
        [
            "--user daemon-reload",
            "--user start data-volume.service web.service"
        ]
    );
    assert_eq!(log.text(), ["Starting data-volume.service, web.service"]);

    let value = |name: &str| {
        let output = applied.outputs.iter().find(|o| o.name == name).unwrap();
        assert!(!output.sensitive);
        output.value.clone()
    };
    assert_eq!(value("url"), Value::String("http://localhost:8080/".into()));
    assert_eq!(
        value("files"),
        Value::String("/var/lib/volumes/systemd-data/_data/www".into())
    );
    assert!(lines(dir.join("podman.calls"))
        .contains(&"volume inspect --format {{.Mountpoint}} systemd-data".to_owned()));
}

#[test]
fn a_host_that_already_matches_is_left_alone() {
    let dir = scratch(TMP, "in-sync");
    let runner = host(&dir);
    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    fs::remove_file(dir.join("systemctl.calls")).unwrap();

    let plan = runner
        .plan(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);

    let applied = runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    assert!(applied.changes.is_empty());
    assert!(systemctl(&dir).is_empty(), "nothing is reloaded or started");
    assert_eq!(applied.outputs.len(), 2);
}

#[test]
fn a_changed_unit_is_rewritten_and_only_it_is_restarted() {
    let dir = scratch(TMP, "changed");
    let runner = host(&dir);
    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    fs::remove_file(dir.join("systemctl.calls")).unwrap();

    let changed = compiled(
        &format!("{DATA}{WEB}        PublishPort: \"8080:80\"\n"),
        OUTPUTS,
    );
    let plan = runner
        .plan(&context(&dir), &changed, &KeptLog::default())
        .unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["update web: web.container changed; restart web.service"]
    );

    let log = KeptLog::default();
    runner.apply(&context(&dir), &changed, &log).unwrap();
    assert_eq!(
        systemctl(&dir),
        [
            "--user daemon-reload",
            "--user restart web.service",
            "--user start data-volume.service web.service",
        ]
    );
    assert!(fs::read_to_string(dir.join("units/web.container"))
        .unwrap()
        .contains("PublishPort=8080:80\n"));
    assert_eq!(
        log.text(),
        [
            "Restarting web.service",
            "Starting data-volume.service, web.service"
        ]
    );
}

#[test]
fn a_unit_the_document_no_longer_has_is_stopped_and_removed() {
    let dir = scratch(TMP, "removed");
    let runner = host(&dir);
    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    // Somebody else's unit, in the same directory.
    fs::write(dir.join("units/theirs.container"), "[Container]\n").unwrap();
    fs::remove_file(dir.join("systemctl.calls")).unwrap();

    let only_data = compiled(DATA, "");
    let plan = runner
        .plan(&context(&dir), &only_data, &KeptLog::default())
        .unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["delete web: stop web.service and remove web.container"]
    );
    assert!(
        dir.join("units/web.container").exists(),
        "a plan removes nothing"
    );

    let applied = runner
        .apply(&context(&dir), &only_data, &KeptLog::default())
        .unwrap();
    assert_eq!(applied.changes.len(), 1);
    assert_eq!(installed(&dir), ["data.volume", "theirs.container"]);
    assert_eq!(
        systemctl(&dir),
        [
            "--user stop web.service",
            "--user daemon-reload",
            "--user start data-volume.service",
        ]
    );
    assert!(!dir.join("active/web.service").exists());

    // What was removed is forgotten, so it is not removed twice.
    let plan = runner
        .plan(&context(&dir), &only_data, &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync(), "{:?}", plan.changes);
}

#[test]
fn a_unit_that_is_installed_but_not_running_is_started() {
    let dir = scratch(TMP, "stopped");
    let runner = host(&dir);
    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    fs::remove_file(dir.join("active/web.service")).unwrap();
    fs::remove_file(dir.join("systemctl.calls")).unwrap();

    let plan = runner
        .plan(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["update web: web.service is inactive; start it"]
    );
    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        systemctl(&dir),
        [
            "--user daemon-reload",
            "--user start data-volume.service web.service"
        ]
    );
    assert!(dir.join("active/web.service").exists());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the stand-in for Quadlet names its scratch directory as a Windows shell does, which is not what the runner strips from the message; Quadlet itself only exists on Linux"
)]
fn a_unit_quadlet_refuses_stops_the_run_before_anything_changes() {
    let dir = scratch(TMP, "refused");
    let bad = compiled(&format!("{DATA}{WEB}        Bogus: 1\n"), "");
    let err = host(&dir)
        .apply(&context(&dir), &bad, &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "the Quadlet on this machine refused the units");
    assert_eq!(err.exit_code, Some(1));
    // What Quadlet said, without its process id or the scratch directory.
    assert_eq!(
        err.output,
        "converting \"web.container\": unsupported key 'Bogus' in group 'Container' in web.container"
    );
    assert!(installed(&dir).is_empty());
    assert!(systemctl(&dir).is_empty());
    assert!(!dir.join("work/installed.json").exists());
}

#[test]
fn advice_from_quadlet_is_relayed_and_stops_nothing() {
    let dir = scratch(TMP, "advice");
    let short = compiled(
        "    - id: web\n      type: container\n      params: {Image: short}\n",
        "",
    );
    let log = KeptLog::default();
    let plan = host(&dir).plan(&context(&dir), &short, &log).unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(
        log.text(),
        ["Warning: web.container specifies an image that is not fully qualified"]
    );
}

#[test]
fn a_kind_of_unit_the_hosts_podman_does_not_know_is_refused_by_name() {
    let dir = scratch(TMP, "old-podman");
    let runner = host(&dir);
    fs::write(dir.join("old-podman"), "").unwrap();
    let pod = compiled("    - id: shop\n      type: pod\n", "");
    let err = runner
        .plan(&context(&dir), &pod, &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "shop.pod would not become shop-pod.service on this machine: its Podman (podman \
         version 5.4.0) does not turn it into a service. Quadlet learnt .pod units in Podman 5.0."
    );
}

#[test]
fn a_host_without_quadlet_says_so() {
    let dir = scratch(TMP, "no-quadlet");
    let runner = PodmanRunner {
        generator: Some(dir.join("nowhere/quadlet").to_str().unwrap().to_owned()),
        ..host(&dir)
    };
    let err = runner
        .plan(&context(&dir), &shop(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "Podman's Quadlet was not found on this machine. Quadlet comes with Podman 4.4 and \
         later; if it is installed somewhere unusual, name it with MERIDIAN_PODMAN_QUADLET"
    );
}

#[test]
fn a_unit_that_does_not_start_fails_the_apply_with_its_journal() {
    let dir = scratch(TMP, "broken");
    let runner = host(&dir);
    fs::write(dir.join("broken/web.service"), "").unwrap();
    let log = KeptLog::default();
    let err = runner.apply(&context(&dir), &shop(), &log).unwrap_err();
    assert_eq!(err.msg, "a unit did not start");
    assert_eq!(err.exit_code, Some(75));
    assert!(
        err.output.contains("Job for web.service failed."),
        "{}",
        err.output
    );
    assert!(
        err.output.contains(
            "--- the journal of web.service ---\njournal: --user -u web.service -n 30 --no-pager"
        ),
        "{}",
        err.output
    );
    // Both files were written before the start failed, and the failure says so.
    assert_eq!(shown(&err.changes).len(), 2);
    assert_eq!(installed(&dir), ["data.volume", "web.container"]);

    // The units are on record, so a later apply can still clean them up.
    fs::remove_file(dir.join("broken/web.service")).unwrap();
    let plan = runner
        .plan(&context(&dir), &compiled(DATA, ""), &KeptLog::default())
        .unwrap();
    assert_eq!(
        shown(&plan.changes),
        ["delete web: stop web.service and remove web.container"]
    );
}

#[test]
fn another_host_is_reached_over_ssh_with_the_same_scripts() {
    let dir = scratch(TMP, "remote");
    let ssh = stand_in(
        &dir,
        "ssh",
        &format!(
            r#"echo "$@" >> "{d}/ssh.calls"
for last in "$@"; do :; done
exec sh -c "$last""#,
            d = dir.display()
        ),
    );
    let runner = PodmanRunner {
        ssh: ssh.to_str().unwrap().to_owned(),
        ssh_args: vec!["-l".into(), "deploy".into()],
        // A local shell that must not be used.
        shell: dir.join("no-such-shell").to_str().unwrap().to_owned(),
        ..host(&dir)
    };
    let remote = RunContext {
        host: "edge01.example.com".into(),
        ..context(&dir)
    };
    let applied = runner.apply(&remote, &shop(), &KeptLog::default()).unwrap();
    assert_eq!(applied.changes.len(), 2);
    assert_eq!(installed(&dir), ["data.volume", "web.container"]);
    let calls = lines(dir.join("ssh.calls"));
    assert_eq!(
        calls.len(),
        3,
        "a probe, an apply and the outputs: {calls:?}"
    );
    for call in calls {
        assert_eq!(
            call,
            "-o BatchMode=yes -l deploy edge01.example.com sh -c 'eval \"$(cat)\"'"
        );
    }
}

#[test]
fn a_host_ssh_cannot_reach_is_named() {
    let dir = scratch(TMP, "unreachable");
    let ssh = stand_in(
        &dir,
        "ssh",
        "echo 'ssh: connect to host edge01.example.com port 22: Connection refused' >&2; exit 255",
    );
    let runner = PodmanRunner {
        ssh: ssh.to_str().unwrap().to_owned(),
        ..host(&dir)
    };
    let remote = RunContext {
        host: "edge01.example.com".into(),
        ..context(&dir)
    };
    let err = runner
        .plan(&remote, &shop(), &KeptLog::default())
        .unwrap_err();
    assert_eq!(err.msg, "could not reach edge01.example.com over ssh");
    assert_eq!(err.exit_code, Some(255));
    assert!(err.output.contains("Connection refused"), "{}", err.output);

    let odd = RunContext {
        host: "-oProxyCommand=evil".into(),
        ..context(&dir)
    };
    let err = runner.plan(&odd, &shop(), &KeptLog::default()).unwrap_err();
    assert_eq!(
        err.msg,
        "\"-oProxyCommand=evil\" is not a host name ssh can be given"
    );
}

#[test]
fn the_system_scope_addresses_the_machines_systemd_and_asks_for_root_only_to_change() {
    let dir = scratch(TMP, "system");
    let runner = PodmanRunner {
        scope: Scope::System,
        ..host(&dir)
    };
    let root = fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .any(|line| line.starts_with("Uid:") && line.split_whitespace().nth(1) == Some("0"));

    runner
        .plan(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    assert_eq!(lines(dir.join("quadlet.calls")), ["-dryrun"]);
    assert!(
        lines(dir.join("sudo.calls")).is_empty(),
        "a plan needs no more rights than the operator has"
    );

    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    assert_eq!(
        systemctl(&dir),
        ["daemon-reload", "start data-volume.service web.service"]
    );
    assert_eq!(installed(&dir), ["data.volume", "web.container"]);
    let sudo = lines(dir.join("sudo.calls"));
    if root {
        assert!(sudo.is_empty(), "root needs no sudo: {sudo:?}");
    } else {
        assert!(sudo.iter().all(|call| call.starts_with("-n ")), "{sudo:?}");
        assert!(
            sudo.iter().any(|call| call.contains("daemon-reload")),
            "{sudo:?}"
        );
        assert!(sudo.iter().any(|call| call.contains(" tee ")), "{sudo:?}");
    }
}

#[test]
fn outputs_are_read_again_without_planning_anything() {
    let dir = scratch(TMP, "outputs");
    let runner = host(&dir);
    let err = runner.outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );

    runner
        .apply(&context(&dir), &shop(), &KeptLog::default())
        .unwrap();
    fs::remove_file(dir.join("quadlet.calls")).unwrap();
    let outputs = runner.outputs(&context(&dir)).unwrap();
    assert_eq!(outputs.len(), 2);
    assert!(!dir.join("quadlet.calls").exists(), "nothing is planned");

    fs::write(dir.join("no-volume"), "").unwrap();
    let err = runner.outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "podman could not report Mountpoint of the volume systemd-data, which an output reads"
    );
    assert!(err.output.contains("no such volume"), "{}", err.output);
}

#[test]
fn outputs_known_at_compile_time_ask_the_host_nothing() {
    let dir = scratch(TMP, "static-outputs");
    let runner = host(&dir);
    let artifact = compiled(
        &format!("{DATA}{WEB}"),
        "  outputs:\n    - name: service\n      value: ${web.service}\n",
    );
    let applied = runner
        .apply(&context(&dir), &artifact, &KeptLog::default())
        .unwrap();
    assert_eq!(
        applied.outputs[0].value,
        Value::String("web.service".into())
    );
    assert_eq!(lines(dir.join("podman.calls")), ["--version"]);
}

#[test]
fn an_artifact_from_another_plugin_or_a_tampered_one_is_refused() {
    let dir = scratch(TMP, "foreign");
    let runner = host(&dir);
    let err = runner
        .plan(
            &context(&dir),
            &artifact(&[("playbook.yml", "")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.msg,
        "the artifact has no meridian-podman.json; it was not compiled by this plugin"
    );

    let tampered = artifact(&[(
        META_PATH,
        r#"{"units":[{"file":"x.container; rm -rf /","id":"x","kind":"container","service":"x.service"}]}"#,
    )]);
    let err = runner
        .plan(&context(&dir), &tampered, &KeptLog::default())
        .unwrap_err();
    assert_eq!(
        err.msg,
        "meridian-podman.json could not be read: it names a unit that is not a unit file"
    );
}

#[test]
fn a_setting_that_cannot_be_understood_fails_every_run() {
    let dir = scratch(TMP, "misconfigured");
    let runner = PodmanRunner {
        misconfigured: Some(
            "MERIDIAN_PODMAN_SCOPE is \"root\"; it must be \"user\" or \"system\"".into(),
        ),
        ..host(&dir)
    };
    let err = runner
        .plan(&context(&dir), &shop(), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with("MERIDIAN_PODMAN_SCOPE is \"root\""),
        "{}",
        err.msg
    );
}
