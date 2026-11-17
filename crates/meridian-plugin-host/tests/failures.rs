//! What the host does when a plugin is not a plugin.
//!
//! Each case is a small shell script standing in for a broken binary. The host
//! has to turn every one of them into an error that says what happened, within
//! a bounded time, and leave no process behind.

// Left out on Windows: each broken plugin here is a shell script started as
// the plugin binary itself, which Windows cannot do. Starting, driving and
// stopping a real plugin on Windows is covered by `runner.rs` and by every
// plugin's `tests/plugin.rs`.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use meridian_plugin_host::{Client, Error, Registry, StartOptions};
use meridian_sdk::{version, PROTOCOL_VERSION, VERSION};

/// Writes an executable script and returns its path.
fn script(name: &str, body: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fake-plugins");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    // Written under another name and renamed, so the file is never executed
    // while it is still open for writing.
    let staged = dir.join(format!("{name}.staged"));
    fs::write(&staged, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&staged, &path).unwrap();
    path
}

fn quick() -> StartOptions {
    StartOptions {
        handshake_timeout: Duration::from_millis(500),
        ..StartOptions::default()
    }
}

fn start_err(path: &Path, options: &StartOptions) -> Error {
    Client::start(path.to_str().unwrap(), options).expect_err("the plugin should not start")
}

#[test]
fn a_binary_that_does_not_exist_is_reported_with_its_path() {
    let err = start_err(Path::new("/nonexistent/meridian-target-nope"), &quick());
    assert_eq!(err.msg, "could not start the plugin");
    assert_eq!(err.path, "/nonexistent/meridian-target-nope");
    assert!(!err.cause.is_empty());
    assert!(
        err.to_string()
            .starts_with("plugin /nonexistent/meridian-target-nope: could not start the plugin: "),
        "{err}"
    );
}

#[test]
fn a_program_that_is_not_a_plugin_is_named_as_such() {
    let path = script("not-a-plugin", "echo 'hello world'\ncat >/dev/null");
    let err = start_err(&path, &quick());
    assert_eq!(err.msg, "handshake failed");
    assert_eq!(
        err.cause,
        "the first line of output was \"hello world\", which is not a Meridian plugin handshake"
    );
}

#[test]
fn a_plugin_that_exits_at_once_is_reported_with_what_it_said() {
    let path = script(
        "exits-early",
        "echo 'cannot open the config file' >&2\nexit 3",
    );
    let err = start_err(&path, &quick());
    assert_eq!(err.msg, "handshake failed");
    assert_eq!(err.cause, "the plugin exited before announcing itself");
    assert_eq!(err.stderr, "cannot open the config file");
    assert!(
        err.to_string()
            .ends_with("\nplugin stderr: cannot open the config file"),
        "{err}"
    );
}

#[test]
fn a_silent_plugin_is_given_up_on_and_killed() {
    let path = script("silent", "exec sleep 600");
    let began = Instant::now();
    let err = start_err(&path, &quick());
    let took = began.elapsed();
    assert_eq!(err.msg, "handshake failed");
    assert_eq!(err.cause, "no handshake line within 500ms");
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

#[test]
fn a_plugin_from_another_protocol_version_is_refused_before_any_call() {
    let path = script(
        "from-the-future",
        "echo 'MERIDIAN-PLUGIN|99|0.1.0|unix|/tmp/never-dialled'\ncat >/dev/null",
    );
    let err = start_err(&path, &quick());
    assert_eq!(err.msg, "protocol mismatch");
    assert_eq!(
        err.cause,
        "the plugin speaks protocol version 99 and this host speaks 2; \
         the plugin is newer than this host: upgrade meridian"
    );

    let path = script(
        "from-the-past",
        "echo 'MERIDIAN-PLUGIN|1|0.1.0|unix|/tmp/never-dialled'\ncat >/dev/null",
    );
    let err = start_err(&path, &quick());
    assert_eq!(err.msg, "protocol mismatch");
    assert_eq!(
        err.cause,
        "the plugin speaks protocol version 1 and this host speaks 2; \
         the plugin is older than this host supports: rebuild it against a current SDK"
    );
}

#[test]
fn a_plugin_built_against_an_sdk_this_host_does_not_ship_is_refused_by_name() {
    let path = script(
        "newer-sdk",
        &format!(
            "echo 'MERIDIAN-PLUGIN|{PROTOCOL_VERSION}|9.9.9|unix|/tmp/never-dialled'\ncat >/dev/null"
        ),
    );
    let err = start_err(&path, &quick());
    assert_eq!(err.msg, "SDK mismatch");
    assert_eq!(
        err.cause,
        format!(
            "the plugin was built against SDK 9.9.9 (9.x), and this host ships SDK {VERSION} \
             (0.1.x); the plugin's SDK is newer than this host supports: upgrade meridian"
        )
    );
}

#[test]
fn the_host_offers_the_protocol_versions_it_speaks() {
    // The plugin picks its version from this offer, so it has to arrive even
    // when the caller replaced the plugin's whole environment.
    let path = script(
        "says-what-it-was-offered",
        "echo \"offered: [$MERIDIAN_PLUGIN_PROTOCOLS]\" >&2\nexit 3",
    );
    let want = format!(
        "offered: [{}]",
        version::offer(&version::supported_protocols())
    );
    for options in [
        quick(),
        StartOptions {
            env: Some(vec![("PATH".to_owned(), std::env::var("PATH").unwrap())]),
            ..quick()
        },
    ] {
        let err = start_err(&path, &options);
        assert!(err.stderr.contains(&want), "{err:?}");
    }
}

#[test]
fn a_plugin_that_announces_an_address_nobody_serves_is_reported() {
    let path = script(
        "nobody-home",
        &format!(
            "echo 'MERIDIAN-PLUGIN|{PROTOCOL_VERSION}|0.1.0|unix|/nonexistent/meridian.sock'\ncat >/dev/null"
        ),
    );
    let began = Instant::now();
    let err = start_err(&path, &quick());
    assert!(
        err.msg == "could not connect to the plugin"
            || err.msg == "the plugin did not describe itself",
        "{err}"
    );
    assert!(!err.cause.is_empty());
    assert!(began.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_plugin_that_accepts_but_never_answers_is_given_up_on() {
    // Something listens, so the dial succeeds, but it is not a plugin: the
    // describe call gets no reply and the deadline has to end the wait.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fake-plugins");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("mute.sock");
    let _ = fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        // Every accepted connection is kept open and never written to.
        let mut held = Vec::new();
        for connection in listener.incoming() {
            held.push(connection);
        }
    });

    let path = script(
        "mute",
        &format!(
            "echo 'MERIDIAN-PLUGIN|{PROTOCOL_VERSION}|0.1.0|unix|{}'\ncat >/dev/null",
            socket.display()
        ),
    );
    let began = Instant::now();
    let err = start_err(&path, &quick());
    assert_eq!(err.msg, "the plugin did not describe itself", "{err}");
    assert!(began.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_noisy_plugin_cannot_make_the_host_keep_all_of_its_stderr() {
    let path = script(
        "noisy",
        "echo 'the first thing that went wrong' >&2\n\
         head -c 300000 /dev/zero | tr '\\0' 'x' >&2\n\
         exit 1",
    );
    let err = start_err(&path, &quick());
    assert!(
        err.stderr.starts_with("the first thing that went wrong\n"),
        "the start of stderr is what explains a failure"
    );
    assert!(err.stderr.len() < 70_000, "kept {} bytes", err.stderr.len());
}

#[test]
fn a_failed_load_leaves_the_registry_as_it_was() {
    let path = script("not-a-plugin-either", "echo nope\ncat >/dev/null");
    let mut registry = Registry::new();
    let err = registry
        .load(path.to_str().unwrap(), &quick())
        .expect_err("the load should fail");
    assert_eq!(err.msg, "handshake failed");
    assert!(registry.names().is_empty());
    registry.close().unwrap();
}

#[test]
fn discovering_a_directory_that_does_not_exist_finds_nothing() {
    let mut registry = Registry::new();
    let infos = registry
        .discover("/nonexistent/meridian-plugins", &quick())
        .expect("a missing plugin directory is not an error");
    assert!(infos.is_empty());
}
