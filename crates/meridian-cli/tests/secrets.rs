//! Secrets through the real command: a secret decides a `when`, and its
//! value is in nothing the run prints or leaves on disk. Not in the plan, not
//! in what the tool logged, not in an error, not in the state directory, not
//! in what `compile --out` writes.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PASSWORD: &str = "kq7Zp2-not-a-real-password-9fXw";
const TOKEN: &str = "tok_Zx81-not-a-real-token";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secrets-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the command with `PASSWORD` in the environment variable the
/// documents read, and with any further variables.
fn meridian_with(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("SITE_DB_PASSWORD");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("meridian runs")
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    meridian_with(dir, args, &[("SITE_DB_PASSWORD", PASSWORD)])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every file under `dir`, with its content.
fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else if let Ok(bytes) = fs::read(&path) {
            found.push((path, bytes));
        }
    }
    found
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Asserts no secret is in what the run printed or in any file under `dir`
/// other than the ones the test wrote there itself.
fn nothing_leaked(output: &Output, dir: &Path, except: &[&str]) {
    for secret in [PASSWORD, TOKEN] {
        assert!(
            !stdout(output).contains(secret),
            "stdout:\n{}",
            stdout(output)
        );
        assert!(
            !stderr(output).contains(secret),
            "stderr:\n{}",
            stderr(output)
        );
        for (path, bytes) in files_under(dir) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if except.contains(&name.as_str()) {
                continue;
            }
            assert!(!holds(&bytes, secret), "{} holds a secret", path.display());
        }
    }
}

/// A document whose conditions read three secrets, one from each provider.
/// No param holds one, so every stage can run.
const BY_SECRET: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: fake
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
    - name: token
      from: file
      path: token.txt
    - name: flag
      from: exec
      command: [printf, "%s", "on"]
    - name: extra
      from: env
      key: SITE_NOT_SET_ANYWHERE
      optional: true
  conditions:
    has_extra: secret.extra != null
    strong: secret.db_password != 'changeme' && secret.db_password != ''
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
    - id: strong
      type: file
      when: cond.strong
      params:
        content: "the password is not the default\n"
    - id: flagged
      type: file
      # The token is compared in an encoded form, so that this file does not
      # hold it and the tests can search every file for it.
      when: secret.flag == 'on' && base64Encode(secret.token) == 'dG9rX1p4ODEtbm90LWEtcmVhbC10b2tlbg=='
      params:
        content: "flag on\n"
    - id: extra
      type: file
      when: cond.has_extra
      params:
        content: "extra\n"
"#;

fn by_secret(test: &str) -> PathBuf {
    let dir = scratch(test);
    fs::write(dir.join("site.yaml"), BY_SECRET).unwrap();
    let token = dir.join("token.txt");
    fs::write(&token, format!("{TOKEN}\n")).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    dir
}

#[test]
fn a_secret_decides_a_when_and_is_in_nothing_the_run_prints_or_writes() {
    let dir = by_secret("decides");

    let plan = meridian(&dir, &["plan", "site.yaml"]);
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    let text = stdout(&plan);
    assert!(text.contains("+ create files/motd"), "{text}");
    assert!(text.contains("+ create files/strong"), "{text}");
    assert!(text.contains("+ create files/flagged"), "{text}");
    assert!(!text.contains("files/extra"), "{text}");
    // What a condition removed is reported in the document's own words.
    assert!(text.contains("resource \"extra\""), "{text}");
    assert!(text.contains("cond.has_extra"), "{text}");
    nothing_leaked(&plan, &dir, &["token.txt"]);

    let apply = meridian(&dir, &["apply", "site.yaml"]);
    assert!(
        apply.status.success(),
        "{}{}",
        stdout(&apply),
        stderr(&apply)
    );
    assert!(
        stdout(&apply).contains("applied 3 changes"),
        "{}",
        stdout(&apply)
    );
    nothing_leaked(&apply, &dir, &["token.txt"]);
    // The run left a state directory behind, and it was searched.
    assert!(dir.join(".meridian/state.json").exists());
    assert!(files_under(&dir.join(".meridian")).len() >= 4);

    // The other answer, from another value of the same secret.
    let weak = meridian_with(
        &dir,
        &["plan", "site.yaml"],
        &[("SITE_DB_PASSWORD", "changeme")],
    );
    assert!(weak.status.success(), "{}", stderr(&weak));
    let text = stdout(&weak);
    assert!(text.contains("resource \"strong\""), "{text}");
    assert!(text.contains("cond.strong"), "{text}");
    assert!(!text.contains("changeme"), "{text}");
}

#[test]
fn a_secret_that_cannot_be_fetched_stops_the_stage_and_names_only_where_it_was_looked_for() {
    let dir = by_secret("missing");
    // The environment variable is not set.
    let out = meridian_with(&dir, &["apply", "site.yaml"], &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains(
            "spec.secrets: secret \"db_password\" (from: env): the environment variable \
             SITE_DB_PASSWORD is not set"
        ),
        "{text}"
    );
    assert!(!dir
        .join(".meridian/resourceset/site/fake/current/files/motd")
        .exists());

    // The file is readable by others.
    fs::set_permissions(dir.join("token.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    let out = meridian(&dir, &["plan", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains("secret \"token\" (from: file)") && text.contains("has mode 0644"),
        "{text}"
    );
    nothing_leaked(&out, &dir, &["token.txt"]);

    // The command fails, having printed something.
    fs::set_permissions(dir.join("token.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    let failing = BY_SECRET.replace(
        "command: [printf, \"%s\", \"on\"]",
        "command: [sh, -c, \"echo tok_Zx81-not-a-real-token; exit 4\"]",
    );
    fs::write(dir.join("site.yaml"), failing).unwrap();
    let out = meridian(&dir, &["plan", "site.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains("secret \"flag\" (from: exec): the command sh exited with status 4"),
        "{text}"
    );
    // Neither what it printed nor its arguments are repeated.
    assert!(!text.contains("exit 4") && !text.contains(TOKEN), "{text}");
}

#[test]
fn validate_and_compile_never_fetch_a_secret() {
    let dir = by_secret("unfetched");
    // Nothing the document needs exists: no variable, no file.
    fs::remove_file(dir.join("token.txt")).unwrap();

    let out = meridian_with(&dir, &["validate", "site.yaml"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));

    // The document's conditions read secrets, and compile does not fetch
    // one, so it says so instead of guessing.
    let out = meridian_with(&dir, &["compile", "site.yaml"], &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains(
            "is not fetched here: a secret is fetched by plan and apply, never by \
                       validate or compile"
        ),
        "{text}"
    );
}

/// A document that places a secret in a param.
const IN_A_PARAM: &str = concat!(
    r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: fake
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
  resources:
    - id: conf
      type: file
      params:
        content: "password=$"#,
    r#"{{ secret.db_password }}\n"
    - id: url
      type: file
      params:
        content: "postgres://app:$"#,
    r#"{{ urlEncode(secret.db_password) }}@db/app"
"#
);

#[test]
fn compile_writes_the_name_of_a_secret_in_a_param_and_never_its_value() {
    let dir = scratch("placeholder");
    fs::write(dir.join("site.yaml"), IN_A_PARAM).unwrap();
    // The variable is set, and compile still does not read it.
    let out = meridian(&dir, &["compile", "site.yaml", "--out", "out"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    nothing_leaked(&out, &dir, &[]);
    let written: Vec<String> = files_under(&dir.join("out"))
        .into_iter()
        .map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    assert!(
        written
            .iter()
            .any(|text| text == "password=(secret db_password)\n"),
        "{written:#?}"
    );
    assert!(
        written
            .iter()
            .any(|text| text == "postgres://app:urlEncode((secret db_password))@db/app"),
        "{written:#?}"
    );
}

#[test]
fn plan_and_apply_stop_before_a_secret_in_a_param_would_be_written_to_a_file() {
    let dir = scratch("refused");
    fs::write(dir.join("site.yaml"), IN_A_PARAM).unwrap();
    for command in ["plan", "apply"] {
        let out = meridian(&dir, &[command, "site.yaml"]);
        assert_eq!(out.status.code(), Some(1), "{command}: {}", stdout(&out));
        let text = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            text.contains(
                "resource \"conf\": params.content holds a secret, and target \"fake\" cannot be \
                 handed a secret yet"
            ),
            "{command}: {text}"
        );
        nothing_leaked(&out, &dir, &[]);
        // Nothing was compiled into the work directory.
        for (path, bytes) in files_under(&dir.join(".meridian")) {
            assert!(!holds(&bytes, "password="), "{}", path.display());
        }
    }
}

#[test]
fn a_secret_read_from_the_environment_is_not_handed_to_the_plugin() {
    // The fake tool logs its inputs; a plugin is a child process, and the
    // variable was removed from the environment before it was started.
    let dir = by_secret("not-inherited");
    let probe = BY_SECRET.replace(
        "command: [printf, \"%s\", \"on\"]",
        "command: [sh, -c, \"env | grep -c SITE_DB_PASSWORD; true\"]",
    );
    // `flag` now holds how many times the variable appears in the
    // environment of a program started after `db_password` was fetched.
    let probe = probe.replace("secret.flag == 'on'", "trim(secret.flag) == '0'");
    fs::write(dir.join("site.yaml"), probe).unwrap();
    let out = meridian(&dir, &["plan", "site.yaml"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(
        stdout(&out).contains("+ create files/flagged"),
        "the variable was still in the environment:\n{}",
        stdout(&out)
    );
    nothing_leaked(&out, &dir, &["token.txt"]);
}

#[test]
fn a_document_left_out_by_its_own_when_fetches_nothing() {
    let dir = scratch("left-out");
    let doc = BY_SECRET.replace(
        "  secrets:\n",
        "  when: data.enabled ?? false\n  secrets:\n",
    );
    fs::write(dir.join("site.yaml"), doc).unwrap();
    // No token file and no variable: fetching would fail.
    let out = meridian_with(&dir, &["plan", "site.yaml"], &[]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("skipped"), "{}", stdout(&out));
}

/// Two stages. The first exposes a value as a plain output; the second is
/// handed it as an input, which the fake tool echoes when it applies, and
/// declares a secret that happens to hold the same value.
const ECHOED: &str = r#"apiVersion: meridian/v1
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
    - id: shared
      type: file
      params:
        content: kq7Zp2-not-a-real-password-9fXw
  outputs:
    - name: address
      value: "${vm.content}"
    - name: shared
      value: "${shared.content}"
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
    - id: motd
      type: file
      when: secret.db_password != ''
      params:
        content: "welcome\n"
"#;

#[test]
fn what_a_tool_prints_is_searched_for_the_secrets_of_its_stage() {
    let dir = scratch("echoed");
    fs::write(dir.join("site.yaml"), ECHOED).unwrap();
    let out = meridian(&dir, &["apply", "site.yaml"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    let log = stderr(&out);
    // The tool of the second stage echoed its inputs, one of which is the
    // very text of a secret that stage fetched.
    let echoed: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("web ! inputs:"))
        .collect();
    assert_eq!(echoed.len(), 1, "{log}");
    assert!(echoed[0].contains("(sensitive)"), "{log}");
    assert!(!echoed[0].contains(PASSWORD), "{log}");
    // The address, which is no secret, is still there to read.
    assert!(echoed[0].contains("10.0.0.5"), "{log}");
}

/// Whether this test runs as root, which may lock memory whatever the limit
/// says, so the tests of memory that cannot be locked have nothing to see.
fn is_root() -> bool {
    let id = Command::new("id").arg("-u").output().expect("id runs");
    String::from_utf8_lossy(&id.stdout).trim() == "0"
}

/// Runs the command with the limit on locked memory at nothing, as on a
/// machine where `ulimit -l` is 0.
fn meridian_unlockable(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new("sh");
    command
        .args(["-c", "ulimit -l 0 && exec \"$0\" \"$@\""])
        .arg(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("MERIDIAN_STRICT_SECRETS")
        .env("SITE_DB_PASSWORD", PASSWORD);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("sh runs")
}

/// Two documents, each with a secret of its own.
fn two_stages(test: &str) -> PathBuf {
    let dir = by_secret(test);
    let second = BY_SECRET.replace("name: site", "name: second");
    fs::write(dir.join("second.yaml"), second).unwrap();
    dir
}

#[test]
fn a_secret_that_cannot_be_locked_in_memory_is_warned_about_once_in_a_run() {
    let dir = two_stages("unlocked");
    let plan = meridian_unlockable(&dir, &["plan", "site.yaml", "second.yaml"], &[]);
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    let text = stdout(&plan);
    nothing_leaked(&plan, &dir, &["token.txt"]);
    // Both stages ran: the second read the same environment variable as the
    // first, which had by then been taken out of the environment.
    assert_eq!(text.matches("+ create files/motd").count(), 2, "{text}");
    if is_root() {
        assert!(!text.contains("could not be locked"), "{text}");
        return;
    }
    // Once, on the first stage that held one, and it names the secrets and
    // what to do, never a value.
    assert_eq!(
        text.matches("could not be locked in memory").count(),
        1,
        "{text}"
    );
    assert!(text.contains("warning: secrets: the secrets "), "{text}");
    assert!(text.contains("\"db_password\""), "{text}");
    assert!(text.contains("ulimit -l"), "{text}");
    assert!(text.contains("--strict-secrets"), "{text}");
}

#[test]
fn strict_secrets_refuses_a_secret_that_cannot_be_locked_in_memory() {
    let dir = two_stages("strict");
    for (args, env) in [
        (
            vec!["apply", "--strict-secrets", "site.yaml", "second.yaml"],
            vec![],
        ),
        (
            vec!["apply", "site.yaml", "second.yaml"],
            vec![("MERIDIAN_STRICT_SECRETS", "true")],
        ),
    ] {
        let apply = meridian_unlockable(&dir, &args, &env);
        nothing_leaked(&apply, &dir, &["token.txt"]);
        let text = format!("{}{}", stdout(&apply), stderr(&apply));
        if is_root() {
            assert!(apply.status.success(), "{text}");
            continue;
        }
        assert!(!apply.status.success(), "{text}");
        assert!(text.contains("could not be locked in memory"), "{text}");
        assert!(text.contains("--strict-secrets is set"), "{text}");
        // Nothing was applied: the stage stopped before its tool was run.
        assert!(!text.contains("applied"), "{text}");
        assert!(!dir.join(".meridian/fake/site/files/motd").exists());
    }
}

#[test]
fn strict_secrets_changes_nothing_where_memory_can_be_locked() {
    let dir = by_secret("strict-locked");
    let plan = meridian(&dir, &["plan", "--strict-secrets", "site.yaml"]);
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    assert!(
        !stdout(&plan).contains("could not be locked"),
        "{}",
        stdout(&plan)
    );
}

/// A document with a secret of every kind of answer a check can give.
const TO_CHECK: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: site
spec:
  target: fake
  targetHost:
    from: inline
    host: web01
  secrets:
    - name: db_password
      from: env
      key: SITE_DB_PASSWORD
    - name: token
      from: file
      path: token.txt
    - name: flag
      from: exec
      command: [touch, the-command-was-run]
    - name: extra
      from: env
      key: SITE_NOT_SET_ANYWHERE
      optional: true
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
"#;

#[test]
fn secret_check_says_where_each_secret_comes_from_and_reads_none() {
    let dir = by_secret("check");
    fs::write(dir.join("site.yaml"), TO_CHECK).unwrap();
    let check = meridian(&dir, &["secret", "check", "site.yaml"]);
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    let text = stdout(&check);
    assert!(text.contains("site (site.yaml)"), "{text}");
    assert!(
        text.contains("db_password  env    ok: the environment variable SITE_DB_PASSWORD is set"),
        "{text}"
    );
    assert!(
        text.contains("token        file   ok: the file token.txt is there"),
        "{text}"
    );
    assert!(
        text.contains("flag         exec   not checked: the command touch is there"),
        "{text}"
    );
    assert!(
        text.contains(
            "extra        env    absent, optional: the environment variable \
             SITE_NOT_SET_ANYWHERE is not set"
        ),
        "{text}"
    );
    // A command is found, never run, and no value is shown.
    assert!(!dir.join("the-command-was-run").exists());
    nothing_leaked(&check, &dir, &["token.txt"]);
    // Nothing was written either.
    assert!(!dir.join(".meridian").exists());
}

#[test]
fn secret_check_fails_when_a_secret_would_not_be_found_and_says_which() {
    let dir = by_secret("check-fails");
    let document = TO_CHECK.replace("[touch, the-command-was-run]", "[no-such-command-anywhere]");
    fs::write(dir.join("site.yaml"), document).unwrap();
    // A file others can read is refused by a fetch, so a check refuses it.
    fs::set_permissions(dir.join("token.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    let check = meridian_with(&dir, &["secret", "check", "site.yaml"], &[]);
    assert!(!check.status.success());
    let text = stdout(&check);
    assert!(
        text.contains("MISSING: the environment variable SITE_DB_PASSWORD is not set"),
        "{text}"
    );
    assert!(
        text.contains("REFUSED: the file token.txt has mode 0644"),
        "{text}"
    );
    assert!(text.contains("allowWideMode: true"), "{text}");
    assert!(
        text.contains("REFUSED: the command no-such-command-anywhere was not found"),
        "{text}"
    );
    assert!(text.contains("absent, optional"), "{text}");
    assert!(
        stderr(&check).contains("error: 3 secrets would not be found by plan or apply"),
        "{}",
        stderr(&check)
    );
    nothing_leaked(&check, &dir, &["token.txt"]);
}

#[test]
fn secret_check_of_documents_without_secrets_says_so() {
    let dir = scratch("check-none");
    let start = TO_CHECK.find("  secrets:").unwrap();
    let end = TO_CHECK.find("  resources:").unwrap();
    let document = format!("{}{}", &TO_CHECK[..start], &TO_CHECK[end..]);
    fs::write(dir.join("site.yaml"), document).unwrap();
    let check = meridian(&dir, &["secret", "check", "site.yaml"]);
    assert!(check.status.success(), "{}", stderr(&check));
    assert_eq!(stdout(&check), "no secret is declared\n");
}
