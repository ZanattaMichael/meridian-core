//! The built-in providers: where each reads from, what each refuses, and
//! that no error ever carries a value.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use meridian_secret::{from_env, from_exec, from_file, FetchError, FileOptions};

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("provider-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str, mode: u32) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn command(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

#[test]
fn env_reads_the_variable_and_removes_it() {
    // Each test uses a variable of its own: the environment is shared.
    std::env::set_var("MERIDIAN_TEST_ENV_READS", "from the environment");
    let value = from_env("MERIDIAN_TEST_ENV_READS").unwrap();
    assert!(value.expose(|bytes| bytes == b"from the environment"));
    assert!(std::env::var_os("MERIDIAN_TEST_ENV_READS").is_none());

    // A second read of the same variable, as by a second document of the
    // run, still answers: from protected memory, not from the environment.
    let again = from_env("MERIDIAN_TEST_ENV_READS").unwrap();
    assert!(again.same(&value));
    assert!(std::env::var_os("MERIDIAN_TEST_ENV_READS").is_none());

    // A variable set again later is read anew.
    std::env::set_var("MERIDIAN_TEST_ENV_READS", "a newer value");
    let newer = from_env("MERIDIAN_TEST_ENV_READS").unwrap();
    assert!(newer.expose(|bytes| bytes == b"a newer value"));
    assert!(std::env::var_os("MERIDIAN_TEST_ENV_READS").is_none());
}

#[test]
fn env_that_was_never_set_is_missing() {
    assert_eq!(
        from_env("MERIDIAN_TEST_ENV_NOT_SET").unwrap_err(),
        FetchError::Missing(
            "the environment variable MERIDIAN_TEST_ENV_NOT_SET is not set".to_owned()
        )
    );
}

#[test]
fn env_keeps_an_empty_value_apart_from_a_missing_one() {
    std::env::set_var("MERIDIAN_TEST_ENV_EMPTY", "");
    assert!(from_env("MERIDIAN_TEST_ENV_EMPTY").unwrap().is_empty());
    assert!(matches!(
        from_env("MERIDIAN_TEST_ENV_NEVER_SET"),
        Err(FetchError::Missing(_))
    ));
}

#[test]
fn file_reads_a_private_file_without_its_trailing_newline() {
    let dir = scratch("file");
    let path = dir.join("token");
    write(&path, "tok_from_file\n", 0o600);
    let value = from_file(&path, FileOptions::default()).unwrap();
    assert!(value.expose(|bytes| bytes == b"tok_from_file"));

    let kept = from_file(
        &path,
        FileOptions {
            keep_trailing_newline: true,
            ..FileOptions::default()
        },
    )
    .unwrap();
    assert!(kept.expose(|bytes| bytes == b"tok_from_file\n"));

    // A key of several lines keeps all but the last line ending.
    write(&path, "line one\nline two\n", 0o400);
    let value = from_file(&path, FileOptions::default()).unwrap();
    assert!(value.expose(|bytes| bytes == b"line one\nline two"));
}

#[test]
fn file_refuses_one_that_others_can_read_unless_told_otherwise() {
    let dir = scratch("wide");
    let path = dir.join("token");
    for mode in [0o644, 0o640, 0o604, 0o660, 0o777] {
        write(&path, "tok_wide_open", mode);
        let err = from_file(&path, FileOptions::default()).unwrap_err();
        let FetchError::Failed(why) = &err else {
            panic!("{err:?}");
        };
        assert!(
            why.contains(&format!("has mode {mode:04o}")) && why.contains("allowWideMode: true"),
            "{why}"
        );
        assert!(
            !why.contains("tok_wide_open"),
            "the error holds the value: {why}"
        );
    }
    write(&path, "tok_wide_open", 0o644);
    let allowed = from_file(
        &path,
        FileOptions {
            allow_wide_mode: true,
            ..FileOptions::default()
        },
    )
    .unwrap();
    assert!(allowed.expose(|bytes| bytes == b"tok_wide_open"));
}

#[test]
fn file_says_what_is_wrong_with_the_place_and_never_what_is_in_it() {
    let dir = scratch("missing");
    let missing = dir.join("nope");
    assert_eq!(
        from_file(&missing, FileOptions::default()).unwrap_err(),
        FetchError::Missing(format!("the file {} does not exist", missing.display()))
    );
    let err = from_file(&dir, FileOptions::default()).unwrap_err();
    assert!(err.to_string().contains("is not a file"), "{err}");

    // Too long to be a secret: refused, and the content is not quoted.
    let big = dir.join("big");
    write(
        &big,
        &"A".repeat(meridian_secret::DEFAULT_MAX_LEN + 1),
        0o600,
    );
    let err = from_file(&big, FileOptions::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("longer than"), "{err}");
    assert!(!err.contains("AAAA"), "{err}");
}

#[test]
fn exec_reads_what_the_command_prints_without_a_shell() {
    let value = from_exec(&command(&["printf", "%s\\n", "tok_from_command"])).unwrap();
    assert!(value.expose(|bytes| bytes == b"tok_from_command"));

    // No shell: what would be shell syntax is an argument like any other.
    let value = from_exec(&command(&["printf", "%s", "$HOME; echo $(id)"])).unwrap();
    assert!(value.expose(|bytes| bytes == b"$HOME; echo $(id)"));
}

#[test]
fn exec_that_fails_says_so_and_keeps_what_it_printed_to_itself() {
    let err = from_exec(&command(&[
        "sh",
        "-c",
        "echo tok_printed_then_failed; echo tok_on_stderr >&2; exit 3",
    ]))
    .unwrap_err();
    assert_eq!(
        err,
        FetchError::Failed("the command sh exited with status 3".to_owned())
    );

    let err = from_exec(&command(&["meridian-no-such-command-anywhere"])).unwrap_err();
    assert_eq!(
        err,
        FetchError::Failed(
            "the command meridian-no-such-command-anywhere was not found".to_owned()
        )
    );
    assert_eq!(
        from_exec(&command(&["true"])).unwrap_err(),
        FetchError::Missing("the command true printed nothing".to_owned())
    );
    assert!(matches!(from_exec(&[]), Err(FetchError::Failed(_))));
    let err = from_exec(&command(&["sh", "-c", "kill -9 $$"])).unwrap_err();
    assert_eq!(
        err,
        FetchError::Failed("the command sh was stopped by a signal".to_owned())
    );
}

#[test]
fn exec_arguments_are_not_part_of_any_error() {
    // An argument is not where a secret belongs, but a path or a name in
    // one should still not be echoed back through an error.
    let err = from_exec(&command(&["sh", "-c", "exit 1", "vault-path/prod/db"]))
        .unwrap_err()
        .to_string();
    assert!(!err.contains("vault-path"), "{err}");
}

fn source(from: &str) -> meridian_secret::Source {
    meridian_secret::Source {
        from: from.to_owned(),
        ..Default::default()
    }
}

#[test]
fn a_probe_says_whether_a_place_would_answer_and_reads_nothing() {
    use meridian_secret::Probe;
    let dir = scratch("probe");

    // The environment: set, taken by an earlier read, or never there.
    std::env::set_var("MERIDIAN_TEST_PROBE", "a value a probe must not show");
    let mut env = source("env");
    env.key = "MERIDIAN_TEST_PROBE".to_owned();
    let set = Probe::Present("the environment variable MERIDIAN_TEST_PROBE is set".to_owned());
    assert_eq!(env.probe(), set);
    // A probe does not take the variable out of the environment.
    assert!(std::env::var_os("MERIDIAN_TEST_PROBE").is_some());
    env.fetch().unwrap();
    assert_eq!(env.probe(), set);
    env.key = "MERIDIAN_TEST_PROBE_NOT_SET".to_owned();
    assert!(matches!(env.probe(), Probe::Absent(_)));

    // A file: there, missing, readable by others, a directory, too large.
    let path = dir.join("token");
    let mut file = source("file");
    file.path = path.display().to_string();
    assert!(matches!(file.probe(), Probe::Absent(_)));
    write(&path, "a value a probe must not show", 0o600);
    assert_eq!(
        file.probe(),
        Probe::Present(format!("the file {} is there", path.display()))
    );
    write(&path, "a value a probe must not show", 0o640);
    let Probe::Refused(why) = file.probe() else {
        panic!("a file its group can read is refused")
    };
    assert!(why.contains("has mode 0640"), "{why}");
    file.allow_wide_mode = true;
    assert!(matches!(file.probe(), Probe::Present(_)));
    write(&path, &"x".repeat(256 * 1024 + 1), 0o600);
    let Probe::Refused(why) = file.probe() else {
        panic!("a file larger than a secret is refused")
    };
    assert!(
        why.contains("larger than a secret can be (256 KiB)"),
        "{why}"
    );
    file.path = dir.display().to_string();
    assert_eq!(
        file.probe(),
        Probe::Refused(format!("{} is not a file", dir.display()))
    );

    // A command is found and never run.
    let marker = dir.join("ran");
    let mut exec = source("exec");
    exec.command = command(&["touch", marker.to_str().unwrap()]);
    let Probe::Unknown(why) = exec.probe() else {
        panic!("only a fetch can tell what a command prints")
    };
    assert_eq!(
        why,
        "the command touch is there, and is only run by plan and apply"
    );
    assert!(!marker.exists());
    exec.command = command(&["/bin/sh", "-c", "exit 1"]);
    assert!(matches!(exec.probe(), Probe::Unknown(_)));
    exec.command = command(&["meridian-test-no-such-program", "an-argument-not-to-repeat"]);
    assert_eq!(
        exec.probe(),
        Probe::Refused("the command meridian-test-no-such-program was not found".to_owned())
    );
    exec.command = command(&["./not/here"]);
    assert!(matches!(exec.probe(), Probe::Refused(_)));

    // No probe's words hold what the place holds.
    for probe in [env.probe(), file.probe(), exec.probe()] {
        assert!(!probe.words().contains("must not show"));
    }
    assert!(matches!(source("vault").probe(), Probe::Refused(_)));
}

#[test]
fn a_source_fetches_by_its_provider_and_says_where_it_looks() {
    let dir = scratch("source");
    let path = dir.join("token");
    write(&path, "from a file\n", 0o600);
    let mut file = source("file");
    file.path = path.display().to_string();
    assert!(file.fetch().unwrap().expose(|b| b == b"from a file"));
    assert_eq!(file.place(), format!("the file {}", path.display()));
    file.keep_trailing_newline = true;
    assert!(file.fetch().unwrap().expose(|b| b == b"from a file\n"));

    let mut exec = source("exec");
    exec.command = command(&["printf", "%s", "from a command"]);
    assert!(exec.fetch().unwrap().expose(|b| b == b"from a command"));
    // Only the program: an argument may be a path or a name worth hiding.
    assert_eq!(exec.place(), "the command printf");

    std::env::set_var("MERIDIAN_TEST_SOURCE", "from the environment");
    let mut env = source("env");
    env.key = "MERIDIAN_TEST_SOURCE".to_owned();
    assert!(env
        .fetch()
        .unwrap()
        .expose(|b| b == b"from the environment"));
    assert_eq!(env.place(), "the environment variable MERIDIAN_TEST_SOURCE");

    let Err(FetchError::Failed(why)) = source("vault").fetch() else {
        panic!("an unknown provider fetches nothing")
    };
    assert!(why.contains("is not a provider this version has"), "{why}");
}
