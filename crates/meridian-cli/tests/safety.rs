//! The commands that make a run safe to repeat and to share: the lock, the
//! record, backends and resume, run as a process against a real plugin.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    - id: key
      type: file
      params:
        content: hunter2-very-secret
  outputs:
    - name: address
      value: "${vm.content}"
    - name: secret_key
      value: "${key.content}"
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
  resources:
    - id: motd
      type: file
      params:
        content: "MOTD"
"#;

fn scratch(test: &str, motd: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("safety")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write(&dir, motd);
    dir
}

fn write(dir: &Path, motd: &str) {
    fs::write(dir.join("site.yaml"), WORKFLOW.replace("MOTD", motd)).unwrap();
}

/// The plugin directories: the fake target, and the reference backend
/// shipped in `examples/backends`.
fn plugin_dirs() -> String {
    let backends = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/backends");
    format!(
        "{}:{}",
        meridian_target_fake::plugin_dir().display(),
        backends.display()
    )
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", plugin_dirs())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The id of the only run under `.meridian/runs`, or of the newest.
fn last_run(dir: &Path) -> String {
    let mut ids: Vec<String> = fs::read_dir(dir.join(".meridian/runs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    ids.sort();
    ids.pop().unwrap()
}

#[test]
fn a_failed_apply_is_shown_by_status_and_finished_by_resume() {
    let dir = scratch("resume", "FAIL welcome");

    let nothing = meridian(&dir, &["status"]);
    assert!(nothing.status.success(), "{}", stderr(&nothing));
    assert_eq!(
        stdout(&nothing),
        "no run has been recorded in the state directory .meridian\nnot locked\n"
    );

    let failed = meridian(&dir, &["apply", "site.yaml"]);
    assert_eq!(failed.status.code(), Some(1));
    let first = last_run(&dir);
    assert!(
        stderr(&failed).contains(&format!(
            "run {first} failed. Once the cause is fixed, `meridian apply --resume site.yaml` \
             continues from the stage that failed"
        )),
        "{}",
        stderr(&failed)
    );

    let status = meridian(&dir, &["status"]);
    let text = stdout(&status);
    assert!(
        text.starts_with(&format!("run {first}: apply, failed\n")),
        "{text}"
    );
    assert!(
        text.contains("  command:  meridian apply site.yaml\n"),
        "{text}"
    );
    assert!(
        text.contains("  network (Infrastructure, target fake): succeeded, 4 changes\n"),
        "{text}"
    );
    assert!(
        text.contains("  web (ResourceSet, target fake on 10.0.0.5): failed (tool-failed)\n"),
        "{text}"
    );
    assert!(text.ends_with("not locked\n"), "{text}");
    // What the tool said is on the terminal of the run, not in the record.
    assert!(!text.contains("disk on fire"), "{text}");

    // Fix the document that failed, and continue, by pasting the hint: the
    // file after `--resume` is a file, not the run to resume.
    write(&dir, "welcome");
    let resumed = meridian(&dir, &["apply", "--resume", "site.yaml"]);
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert_eq!(
        stdout(&resumed),
        format!(
            "network (Infrastructure, target fake)\n\
             \x20 already applied by run {first}, and unchanged since; left alone\n\
             web (ResourceSet, target fake on 10.0.0.5)\n\
             \x20 + create files/motd: 7 bytes\n\
             \x20 applied 1 change\n\
             \x20 output file_count = 1\n\
             \x20 output token = (sensitive)\n"
        )
    );
    // The first stage's tool was not run again.
    assert!(
        !stderr(&resumed).contains("network |"),
        "{}",
        stderr(&resumed)
    );
    let second = last_run(&dir);

    let runs = meridian(&dir, &["runs"]);
    assert_eq!(
        stdout(&runs),
        format!(
            "{first}  apply    failed     2 steps, 1 failed\n\
             {second}  apply    succeeded  2 steps, resumes {first}\n"
        )
    );

    // There is nothing left to continue.
    let again = meridian(&dir, &["apply", "site.yaml", "--resume"]);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(
        stderr(&again),
        format!(
            "error: run {second} succeeded, so there is nothing to resume; run `meridian \
             apply` to apply again\n"
        )
    );
    assert_eq!(stdout(&again), "");

    // A run can also be named.
    let named = meridian(&dir, &["apply", "site.yaml", "--resume=nonsense"]);
    assert_eq!(
        stderr(&named),
        "error: there is no run nonsense in this state\n"
    );
}

#[test]
fn events_are_kept_filtered_and_streamed() {
    let dir = scratch("events", "welcome");
    let apply = meridian(&dir, &["apply", "site.yaml", "--events", "json"]);
    assert!(apply.status.success(), "{}", stderr(&apply));

    // Standard output is the stream and nothing else. The report for people
    // and the tools' own output are on standard error.
    let streamed = stdout(&apply);
    for line in streamed.lines() {
        assert!(
            line.starts_with("{\"attempt\":") && line.contains("\"type\":\""),
            "not an event: {line}"
        );
    }
    assert!(streamed.lines().count() > 8, "{streamed}");
    let report = stderr(&apply);
    assert!(report.contains("  applied 4 changes\n"), "{report}");
    assert!(report.contains("  network | create files/vm\n"), "{report}");
    assert!(!streamed.contains("hunter2") && !report.contains("hunter2"));

    // What was streamed is what was kept.
    let kept = meridian(&dir, &["events"]);
    assert!(kept.status.success(), "{}", stderr(&kept));
    assert_eq!(stdout(&kept), streamed);
    let run = last_run(&dir);
    assert_eq!(stdout(&meridian(&dir, &["events", &run])), streamed);

    let steps = stdout(&meridian(
        &dir,
        &["events", "--type", "step.*", "--type", "run.succeeded"],
    ));
    let types: Vec<&str> = steps
        .lines()
        .map(|line| {
            let at = line.find("\"type\":\"").unwrap() + 8;
            &line[at..at + line[at..].find('"').unwrap()]
        })
        .collect();
    assert_eq!(
        types,
        [
            "step.started",
            "step.succeeded",
            "step.started",
            "step.succeeded",
            "run.succeeded"
        ]
    );

    let missing = meridian(&dir, &["events", "2020-01-01T00-00-00Z-ffff"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        stderr(&missing),
        "error: there is no run 2020-01-01T00-00-00Z-ffff in the state directory .meridian\n"
    );
}

const LOCK: &str = r#"{"command":"apply site.yaml","host":"build-07","id":"2026-11-17T09-14-03Z-7f3a","pid":4242,"since":"2026-11-17T09:14:03.000Z","user":"mike"}"#;

#[test]
fn a_locked_state_refuses_a_run_until_the_lock_is_removed_by_name() {
    let dir = scratch("locked", "welcome");
    fs::create_dir_all(dir.join(".meridian")).unwrap();
    fs::write(dir.join(".meridian/lock"), LOCK).unwrap();
    let holder = "lock 2026-11-17T09-14-03Z-7f3a, taken by `meridian apply site.yaml` \
                  (mike@build-07, process 4242) at 2026-11-17T09:14:03.000Z";

    for command in ["plan", "apply"] {
        let refused = meridian(&dir, &[command, "site.yaml"]);
        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(stdout(&refused), "");
        assert_eq!(
            stderr(&refused),
            format!(
                "error: the state is locked by another run: {holder}\n\
                 Wait for it to finish, or wait here with --lock-timeout. If that run is gone \
                 and will never release it, remove the lock with `meridian unlock \
                 2026-11-17T09-14-03Z-7f3a`\n"
            )
        );
    }
    assert!(!dir.join(".meridian/infrastructure").exists());

    // Waiting is an option, and gives up when the time is over.
    let waited = meridian(&dir, &["plan", "site.yaml", "--lock-timeout", "300ms"]);
    assert_eq!(waited.status.code(), Some(1));

    // Compiling contacts nothing, so it is not in the other run's way.
    let compiled = meridian(&dir, &["compile", "site.yaml"]);
    assert!(compiled.status.success(), "{}", stderr(&compiled));

    let status = meridian(&dir, &["status"]);
    assert!(
        stdout(&status).ends_with(&format!("locked: {holder}\n")),
        "{}",
        stdout(&status)
    );

    // The lock is only removed when it is named.
    let wrong = meridian(&dir, &["unlock", "some-other-lock"]);
    assert_eq!(wrong.status.code(), Some(1));
    assert_eq!(
        stderr(&wrong),
        format!("error: the state is locked, but not by \"some-other-lock\": {holder}\n")
    );
    let unlocked = meridian(&dir, &["unlock", "2026-11-17T09-14-03Z-7f3a"]);
    assert!(unlocked.status.success(), "{}", stderr(&unlocked));
    assert_eq!(stdout(&unlocked), format!("removed {holder}\n"));
    let twice = meridian(&dir, &["unlock", "2026-11-17T09-14-03Z-7f3a"]);
    assert_eq!(stderr(&twice), "error: the state is not locked\n");

    let apply = meridian(&dir, &["apply", "site.yaml"]);
    assert!(apply.status.success(), "{}", stderr(&apply));
    assert!(!dir.join(".meridian/lock").exists());
}

#[test]
fn a_run_killed_mid_way_shows_as_interrupted_and_can_be_resumed() {
    let dir = scratch("interrupted", "FAIL welcome");
    assert_eq!(
        meridian(&dir, &["apply", "site.yaml"]).status.code(),
        Some(1)
    );
    // What a killed run leaves: a record that never heard how it ended.
    let run = last_run(&dir);
    let record = dir.join(".meridian/runs").join(&run).join("run.json");
    let text = fs::read_to_string(&record).unwrap();
    assert!(text.contains("\"status\":\"failed\""), "{text}");
    fs::write(
        &record,
        text.replacen("\"status\":\"failed\"", "\"status\":\"running\"", 1),
    )
    .unwrap();

    let status = stdout(&meridian(&dir, &["status"]));
    assert!(
        status.starts_with(&format!(
            "run {run}: apply, interrupted: it never said how it ended, and it does not hold \
             the lock\n"
        )),
        "{status}"
    );

    write(&dir, "welcome");
    let resumed = meridian(&dir, &["apply", "site.yaml", &format!("--resume={run}")]);
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert!(
        stdout(&resumed).contains(&format!("already applied by run {run}")),
        "{}",
        stdout(&resumed)
    );
}

#[test]
fn the_record_can_live_in_a_backend_program() {
    let dir = scratch("backend", "welcome");
    let shared = dir.join("shared");
    let config = format!("path={}", shared.display());
    let with_backend = |args: &[&str]| {
        let mut all = args.to_vec();
        all.extend(["--backend", "dir", "--backend-config", &config]);
        meridian(&dir, &all)
    };

    let apply = with_backend(&["apply", "site.yaml"]);
    assert!(apply.status.success(), "{}", stderr(&apply));
    assert_eq!(stderr(&apply).matches("warning").count(), 0);

    // Meridian's record is in the shared place. The tools' working
    // directories stay on this machine.
    assert!(shared.join("state.json").is_file());
    assert!(shared.join("runs").is_dir());
    assert!(!shared.join("lock").exists());
    assert!(!dir.join(".meridian/state.json").exists());
    assert!(!dir.join(".meridian/runs").exists());
    assert!(dir
        .join(".meridian/infrastructure/network/fake/state.json")
        .is_file());

    let status = stdout(&with_backend(&["status"]));
    assert!(status.contains(": apply, succeeded\n"), "{status}");
    assert_eq!(stdout(&with_backend(&["runs"])).lines().count(), 1);
    // Without the backend there is nothing to see: the record is not here.
    assert!(stdout(&meridian(&dir, &["status"])).starts_with("no run has been recorded"));

    // A lock taken through the backend is seen by every machine using it.
    fs::write(shared.join("lock"), LOCK).unwrap();
    let refused = with_backend(&["plan", "site.yaml"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("the state is locked by another run"),
        "{}",
        stderr(&refused)
    );
    assert!(with_backend(&["unlock", "2026-11-17T09-14-03Z-7f3a"])
        .status
        .success());
    assert!(with_backend(&["plan", "site.yaml"]).status.success());
}

#[test]
fn a_backend_that_is_missing_or_misconfigured_is_refused_before_anything_runs() {
    let dir = scratch("backend-errors", "welcome");

    let missing = meridian(&dir, &["apply", "site.yaml", "--backend", "s3"]);
    assert_eq!(missing.status.code(), Some(1));
    let said = stderr(&missing);
    assert!(
        said.starts_with(
            "error: no state backend \"s3\": there is no program named meridian-backend-s3 in "
        ),
        "{said}"
    );

    let bad = meridian(
        &dir,
        &[
            "apply",
            "site.yaml",
            "--backend",
            "dir",
            "--backend-config",
            "path",
        ],
    );
    assert_eq!(
        stderr(&bad),
        "error: --backend-config takes key=value, and was given \"path\"\n"
    );

    let local = meridian(&dir, &["apply", "site.yaml", "--backend-config", "path=x"]);
    assert_eq!(
        stderr(&local),
        "error: the local backend takes no settings, and was given \"path\"; it keeps the \
         record in --state-dir\n"
    );

    // The backend is there, and says it cannot work as configured.
    let unset = meridian(&dir, &["apply", "site.yaml", "--backend", "dir"]);
    assert_eq!(unset.status.code(), Some(1));
    assert_eq!(
        stderr(&unset),
        "error: taking the state lock: state backend \"dir\": create lock: exit status 2: the \
         dir backend needs --backend-config path=<directory>\n"
    );

    for refused in [missing, bad, local, unset] {
        assert_eq!(stdout(&refused), "");
    }
    assert!(!dir.join(".meridian").exists());

    let usage = meridian(&dir, &["plan", "site.yaml", "--lock-timeout", "soon"]);
    assert_eq!(usage.status.code(), Some(2));
    assert!(
        stderr(&usage).contains("\"soon\" is not a duration; write one as 30s, 5m or 1h"),
        "{}",
        stderr(&usage)
    );
}

fn motd_on_disk(dir: &Path) -> String {
    fs::read_to_string(dir.join(".meridian/resourceset/web/fake/current/files/motd")).unwrap()
}

#[test]
fn drift_exits_2_only_for_a_difference_nobody_asked_for() {
    let dir = scratch("drift", "welcome");

    let never = meridian(&dir, &["drift", "site.yaml"]);
    assert_eq!(never.status.code(), Some(0), "{}", stderr(&never));
    assert_eq!(
        stdout(&never),
        "network (Infrastructure, target fake)\n\
         \x20 never applied, so nothing to compare\n\
         web (ResourceSet, target fake)\n\
         \x20 deferred: needs the output \"address\" of Infrastructure \"network\", which has not been applied yet\n\
         drift: 0 of 2 stages drifted\n"
    );

    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    let in_sync = meridian(&dir, &["drift", "site.yaml"]);
    assert_eq!(in_sync.status.code(), Some(0));
    assert_eq!(
        stdout(&in_sync),
        "network (Infrastructure, target fake)\n\
         \x20 in sync with what was applied\n\
         web (ResourceSet, target fake on 10.0.0.5)\n\
         \x20 in sync with what was applied\n\
         drift: 0 of 2 stages drifted\n"
    );

    // Someone changes the machine behind Meridian's back. The fake tool's
    // idea of the world is its state file.
    let tool_state = dir.join(".meridian/resourceset/web/fake/state.json");
    let text = fs::read_to_string(&tool_state).unwrap();
    fs::write(&tool_state, text.replace("welcome", "edited by hand")).unwrap();
    let drifted = meridian(&dir, &["drift", "site.yaml"]);
    assert_eq!(drifted.status.code(), Some(2), "{}", stderr(&drifted));
    assert_eq!(
        stdout(&drifted),
        "network (Infrastructure, target fake)\n\
         \x20 in sync with what was applied\n\
         web (ResourceSet, target fake on 10.0.0.5)\n\
         \x20 ~ update files/motd: 7 bytes\n\
         \x20 DRIFTED: 1 difference from what was applied\n\
         drift: 1 of 2 stages drifted\n"
    );
    assert_eq!(
        motd_on_disk(&dir),
        "welcome",
        "a drift check changed something"
    );
    let status = stdout(&meridian(&dir, &["status"]));
    assert!(status.contains(": drift, succeeded\n"), "{status}");

    // Applying puts it right. Then an edited document is not drift: it is a
    // change waiting to be applied.
    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    write(&dir, "a new message");
    let pending = meridian(&dir, &["drift", "site.yaml"]);
    assert_eq!(pending.status.code(), Some(0));
    assert!(
        stdout(&pending).contains(
            "  ~ update files/motd: 13 bytes\n\
             \x20 pending: the document has changed since the last apply, so 1 change is \
             waiting to be applied and cannot be told from drift\n\
             drift: 0 of 2 stages drifted\n"
        ),
        "{}",
        stdout(&pending)
    );
}

#[test]
fn a_failed_apply_can_put_back_what_it_changed() {
    let dir = scratch("on-failure", "one");
    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());

    write(&dir, "FAIL two");
    let failed = meridian(&dir, &["apply", "site.yaml", "--on-failure", "rollback"]);
    assert_eq!(failed.status.code(), Some(1));
    let text = stdout(&failed);
    let Some(at) = text.find("rolling back") else {
        panic!("{text}");
    };
    let rolled = &text[at..];
    assert!(
        rolled.starts_with(
            "rolling back web (ResourceSet, target fake on 10.0.0.5)\n\
             \x20 rolled back to what was applied before (artifact "
        ),
        "{rolled}"
    );
    assert!(
        rolled.ends_with(
            "), 0 changes\n\
             rolling back network (Infrastructure, target fake)\n\
             \x20 nothing to put back: the run applied what was already applied\n"
        ),
        "{rolled}"
    );
    // It was put back, so there is nothing to resume, and it does not say
    // there is.
    assert!(!stderr(&failed).contains("--resume"), "{}", stderr(&failed));
    assert_eq!(motd_on_disk(&dir), "one");

    let status = stdout(&meridian(&dir, &["status"]));
    assert!(
        status.contains("  command:  meridian apply site.yaml --on-failure rollback\n"),
        "{status}"
    );
    assert!(
        status.contains("  web (ResourceSet, target fake on 10.0.0.5): rolled-back\n"),
        "{status}"
    );

    let usage = meridian(&dir, &["apply", "site.yaml", "--on-failure", "panic"]);
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn rollback_puts_the_last_apply_back() {
    let dir = scratch("rollback", "one");

    let nothing = meridian(&dir, &["rollback", "site.yaml"]);
    assert_eq!(nothing.status.code(), Some(1));
    assert_eq!(
        stderr(&nothing),
        "error: there is no apply to roll back: none has been recorded in this state\n"
    );

    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    let first = last_run(&dir);
    write(&dir, "two");
    assert!(meridian(&dir, &["apply", "site.yaml"]).status.success());
    let second = last_run(&dir);
    assert_eq!(motd_on_disk(&dir), "two");

    let back = meridian(&dir, &["rollback", "site.yaml"]);
    assert!(back.status.success(), "{}{}", stdout(&back), stderr(&back));
    let text = stdout(&back);
    assert!(
        text.starts_with(
            "rolling back web (ResourceSet, target fake on 10.0.0.5)\n\
             \x20 ~ update files/motd: 3 bytes\n\
             \x20 rolled back to what was applied before (artifact "
        ),
        "{text}"
    );
    assert!(
        text.ends_with(
            "), 1 change\n\
             rolling back network (Infrastructure, target fake)\n\
             \x20 nothing to put back: the run applied what was already applied\n"
        ),
        "{text}"
    );
    assert_eq!(motd_on_disk(&dir), "one");
    // The tool's own output is still relayed, on standard error.
    assert!(
        stderr(&back).contains("  web | update files/motd\n"),
        "{}",
        stderr(&back)
    );

    let runs = stdout(&meridian(&dir, &["runs"]));
    assert!(
        runs.lines()
            .last()
            .unwrap()
            .contains("  rollback succeeded"),
        "{runs}"
    );

    // The first apply had nothing before it, and by now is not what is
    // there.
    let older = meridian(&dir, &["rollback", "site.yaml", "--run", &first]);
    assert_eq!(older.status.code(), Some(1));
    assert!(
        stdout(&older).contains("  NOT ROLLED BACK: it has been applied again since run"),
        "{}",
        stdout(&older)
    );
    let _ = second;
}

/// A policy engine that speaks the protocol: it denies with each line of
/// its policy file that starts `deny `, warns with each that starts `warn `.
const ENGINE: &str = r#"#!/bin/sh
cat > /dev/null
deny=""
warn=""
while read -r verb message; do
  case "$verb" in
    deny) deny="$deny${deny:+,}\"$message\"" ;;
    warn) warn="$warn${warn:+,}\"$message\"" ;;
  esac
done < "$1"
printf '{"deny":[%s],"warn":[%s]}\n' "$deny" "$warn"
"#;

/// `meridian`, with a policy engine named `lines` on its plugin path.
fn meridian_with_engine(dir: &Path, args: &[&str]) -> Output {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("engines");
    fs::create_dir_all(&bin).unwrap();
    let engine = bin.join("meridian-policy-lines");
    fs::write(&engine, ENGINE).unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .current_dir(dir)
        .env(
            "MERIDIAN_PLUGIN_DIR",
            format!("{}:{}", plugin_dirs(), bin.display()),
        )
        .env("MERIDIAN_POLICY_ENGINE", "lines")
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_BACKEND")
        .output()
        .expect("meridian runs")
}

#[test]
fn a_policy_is_asked_before_an_apply_and_a_denial_applies_nothing() {
    let dir = scratch("policy", "welcome");
    fs::write(dir.join("rules"), "warn check the change window\n").unwrap();

    let plan = meridian_with_engine(&dir, &["plan", "site.yaml", "--policy", "rules"]);
    assert!(plan.status.success(), "{}", stderr(&plan));
    let text = stdout(&plan);
    assert!(
        text.contains("  policy warning: check the change window\n  policy: allowed\n"),
        "{text}"
    );

    fs::write(
        dir.join("rules"),
        "deny no changes on a Friday\ndeny the network is frozen\n",
    )
    .unwrap();
    let denied = meridian_with_engine(&dir, &["apply", "site.yaml", "--policy", "rules"]);
    assert_eq!(denied.status.code(), Some(1));
    let text = stdout(&denied);
    assert!(
        text.contains(
            "  FAILED: the policy does not allow this change:\n      - no changes on a Friday\n      \
             - the network is frozen\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("  not run, because Infrastructure \"network\" failed\n"),
        "{text}"
    );
    let run = last_run(&dir);
    // The tool's preview, which the policy was asked about, is on standard
    // error with the rest of what the tool says.
    assert!(
        stderr(&denied).ends_with(&format!(
            "run {run} was stopped by its policy before anything was applied\n"
        )),
        "{}",
        stderr(&denied)
    );
    assert!(!dir
        .join(".meridian/infrastructure/network/fake/current")
        .exists());

    // The record keeps that a policy said no, and not what it said.
    let events = meridian_with_engine(&dir, &["events", "--type", "policy.*"]);
    let text = stdout(&events);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("\"type\":\"policy.denied\""), "{text}");
    assert!(text.contains("\"denials\":2"), "{text}");
    assert!(!text.contains("Friday"), "{text}");
    let status = meridian_with_engine(&dir, &["status"]);
    assert!(
        stdout(&status)
            .contains("  network (Infrastructure, target fake): failed (policy-denied)\n"),
        "{}",
        stdout(&status)
    );
    assert!(
        stdout(&status).contains("--policy rules"),
        "{}",
        stdout(&status)
    );

    // With the policy satisfied, the same command applies.
    fs::write(dir.join("rules"), "").unwrap();
    let applied = meridian_with_engine(&dir, &["apply", "site.yaml", "--policy", "rules"]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert!(stdout(&applied).contains("  policy: allowed\n"));
}

#[test]
fn a_policy_that_cannot_be_used_is_refused_before_the_run_starts() {
    let dir = scratch("policy-refused", "welcome");
    fs::write(dir.join("rules"), "").unwrap();

    let no_engine = meridian(&dir, &["apply", "site.yaml", "--policy", "rules"]);
    assert_eq!(no_engine.status.code(), Some(1));
    assert!(
        stderr(&no_engine).starts_with(
            "error: no policy engine \"opa\": there is no program named meridian-policy-opa in "
        ),
        "{}",
        stderr(&no_engine)
    );

    let no_file = meridian_with_engine(&dir, &["plan", "site.yaml", "--policy", "absent.rego"]);
    assert_eq!(no_file.status.code(), Some(1));
    assert_eq!(
        stderr(&no_file),
        "error: the policy absent.rego does not exist\n"
    );

    let drift = meridian_with_engine(&dir, &["drift", "site.yaml", "--policy", "rules"]);
    assert_eq!(drift.status.code(), Some(1));
    assert_eq!(
        stderr(&drift),
        "error: --policy is checked by plan and apply, and drift asks no policy; remove it\n"
    );
    // Nothing ran, so nothing was recorded.
    assert!(!dir.join(".meridian/runs").exists());
}
