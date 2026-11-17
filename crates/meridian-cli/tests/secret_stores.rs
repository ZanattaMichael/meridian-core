//! The operating system's stores through the real command: a secret kept in
//! one decides a `when`, `secret set` puts a value there without it ever
//! being an argument, a secret with several places is answered by the first
//! that has it, and the value is in nothing any of this prints or writes.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const VALUE: &str = "kq7Zp2-not-a-real-password-9fXw";
/// `VALUE` as `base64Encode` gives it, so a document can compare a secret
/// without holding it.
const VALUE_ENCODED: &str = "a3E3WnAyLW5vdC1hLXJlYWwtcGFzc3dvcmQtOWZYdw==";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("secret-stores-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the command in `dir` with `input` on its standard input, without a
/// session bus or unit credentials unless `env` gives them.
fn meridian(dir: &Path, args: &[&str], env: &[(&str, &str)], input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("CREDENTIALS_DIRECTORY")
        .env_remove("SITE_DEPLOY_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("meridian runs");
    // A command that does not read its input may end first; that is fine.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().expect("meridian ends")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Everything the command printed, on either stream: a stage that failed is
/// reported with the plan, and the run's own error after it.
fn said(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

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

/// Asserts the value is in nothing the command printed and in no file under
/// `dir` but the ones named, which the test put there to stand for a store.
fn nothing_leaked(output: &Output, dir: &Path, except: &[&str]) {
    assert!(
        !stdout(output).contains(VALUE),
        "stdout:\n{}",
        stdout(output)
    );
    assert!(
        !stderr(output).contains(VALUE),
        "stderr:\n{}",
        stderr(output)
    );
    for (path, bytes) in files_under(dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if except.contains(&name.as_str()) {
            continue;
        }
        assert!(
            !bytes
                .windows(VALUE.len())
                .any(|window| window == VALUE.as_bytes()),
            "{} holds the value",
            path.display()
        );
    }
}

/// A document whose one secret is declared by `declaration` (the lines
/// under its name) and decides whether a resource is there.
fn site(declaration: &str) -> String {
    let declaration: String = declaration
        .lines()
        .map(|line| format!("      {line}\n"))
        .collect();
    format!(
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
    - name: deploy
{declaration}  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
    - id: deployed
      type: file
      when: base64Encode(secret.deploy) == '{VALUE_ENCODED}'
      params:
        content: "the deploy token is the one expected\n"
"#
    )
}

fn write_site(dir: &Path, declaration: &str) {
    fs::write(dir.join("site.yaml"), site(declaration)).unwrap();
}

/// A stand-in for `secret-tool` that keeps each item in a file named by its
/// attributes and records the arguments it was run with. Gives the `PATH`
/// that finds it first.
fn stand_in_secret_tool(dir: &Path) -> String {
    let bin = dir.join("bin");
    let items = dir.join("items");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&items).unwrap();
    let script = format!(
        "#!/bin/sh\nitems='{items}'\nprintf '%s\\n' \"$*\" >> \"$items/../arguments\"\nop=\"$1\"\n\
         shift\ncase \"$op\" in\n  lookup)\n    name=$(printf '%s_' \"$@\")\n    [ -f \
         \"$items/$name\" ] || exit 1\n    cat \"$items/$name\"\n    ;;\n  store)\n    shift\n    \
         name=$(printf '%s_' \"$@\")\n    cat > \"$items/$name\"\n    ;;\n  *)\n    exit 2\n    \
         ;;\nesac\n",
        items = items.display()
    );
    let tool = bin.join("secret-tool");
    fs::write(&tool, script).unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

const BUS: (&str, &str) = ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
/// The file the stand-in keeps the item `app=meridian name=deploy` in.
const ITEM: &str = "app_meridian_name_deploy_";

#[test]
fn secret_set_stores_a_value_from_standard_input_and_a_run_reads_it_back() {
    let dir = scratch("set-service");
    let path = stand_in_secret_tool(&dir);
    let env = [BUS, ("PATH", path.as_str())];
    write_site(
        &dir,
        "from: secret-service\nattributes: {app: meridian, name: deploy}",
    );

    // Before the value is stored the run stops, and says where it looked.
    let plan = meridian(&dir, &["plan", "site.yaml"], &env, "");
    assert!(!plan.status.success());
    assert!(
        said(&plan).contains(
            "secret \"deploy\" (from: secret-service): the Secret Service holds no item with \
             these attributes"
        ),
        "{}",
        said(&plan)
    );

    let set = meridian(
        &dir,
        &["secret", "set", "deploy", "site.yaml"],
        &env,
        &format!("{VALUE}\n"),
    );
    assert!(set.status.success(), "{}{}", stdout(&set), stderr(&set));
    assert_eq!(
        stdout(&set),
        "stored deploy in the Secret Service, item app=meridian name=deploy\n"
    );
    nothing_leaked(&set, &dir, &[ITEM]);
    // The line ending that ended the input is not part of the secret.
    assert_eq!(
        fs::read_to_string(dir.join("items").join(ITEM)).unwrap(),
        VALUE
    );

    // The value reached the store's tool on its standard input, never as an
    // argument, which every user could read in the process list.
    let arguments = fs::read_to_string(dir.join("arguments")).unwrap();
    assert!(
        arguments.contains("store --label=Meridian: deploy app meridian name deploy\n"),
        "{arguments}"
    );
    assert!(!arguments.contains(VALUE), "{arguments}");

    // A check does not ask the service; a plan does, and the secret decides.
    let asked = fs::read_to_string(dir.join("arguments")).unwrap();
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &env, "");
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    assert!(
        stdout(&check).contains(
            "deploy  secret-service  not checked: the Secret Service is only asked by plan and \
             apply"
        ),
        "{}",
        stdout(&check)
    );
    assert_eq!(fs::read_to_string(dir.join("arguments")).unwrap(), asked);

    let plan = meridian(&dir, &["plan", "site.yaml"], &env, "");
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    assert!(
        stdout(&plan).contains("+ create files/deployed"),
        "{}",
        stdout(&plan)
    );
    nothing_leaked(&plan, &dir, &[ITEM]);

    let apply = meridian(&dir, &["apply", "site.yaml"], &env, "");
    assert!(
        apply.status.success(),
        "{}{}",
        stdout(&apply),
        stderr(&apply)
    );
    // Not in the record of the run either.
    nothing_leaked(&apply, &dir, &[ITEM]);
}

#[test]
fn secret_set_refuses_what_it_cannot_store_and_stores_nothing() {
    let dir = scratch("set-refuses");
    let path = stand_in_secret_tool(&dir);
    let env = [BUS, ("PATH", path.as_str())];
    write_site(
        &dir,
        "from: secret-service\nattributes: {app: meridian, name: deploy}",
    );

    // A name the documents do not declare, with the names they do.
    let set = meridian(&dir, &["secret", "set", "deploi", "site.yaml"], &env, VALUE);
    assert!(!set.status.success());
    assert!(
        stderr(&set).contains("\"deploi\" is not declared (declared: deploy)"),
        "{}",
        stderr(&set)
    );
    nothing_leaked(&set, &dir, &[]);

    // No value: nothing is stored, and an item that was there is kept.
    let set = meridian(&dir, &["secret", "set", "deploy", "site.yaml"], &env, "\n");
    assert!(!set.status.success());
    assert!(
        stderr(&set).contains("no value was given, so nothing was stored"),
        "{}",
        stderr(&set)
    );
    assert!(!dir.join("items").join(ITEM).exists());

    // A secret whose places meridian does not write: each is named, and the
    // value on standard input is not read into anything shown.
    write_site(
        &dir,
        "from: env\nkey: SITE_DEPLOY_TOKEN\nfallback:\n  - {from: file, path: token.txt}",
    );
    let set = meridian(&dir, &["secret", "set", "deploy", "site.yaml"], &env, VALUE);
    assert!(!set.status.success());
    let text = stderr(&set);
    assert!(
        text.contains(
            "\"deploy\" names no place meridian can write. It is looked for in the environment \
             variable SITE_DEPLOY_TOKEN, then in the file token.txt."
        ),
        "{text}"
    );
    assert!(text.contains("from: keyring"), "{text}");
    assert!(text.contains("from: secret-service"), "{text}");
    nothing_leaked(&set, &dir, &[]);

    // Documents that declare no secret at all.
    let document = site("from: env\nkey: K");
    let start = document.find("  secrets:").unwrap();
    let end = document.find("  resources:").unwrap();
    let without = format!("{}{}", &document[..start], &document[end..]).replace(
        &format!("      when: base64Encode(secret.deploy) == '{VALUE_ENCODED}'\n"),
        "",
    );
    fs::write(dir.join("site.yaml"), without).unwrap();
    let set = meridian(&dir, &["secret", "set", "deploy", "site.yaml"], &env, VALUE);
    assert!(!set.status.success());
    assert!(
        stderr(&set).contains("\"deploy\" is not declared: these documents declare no secret"),
        "{}",
        stderr(&set)
    );
}

#[test]
fn secret_set_takes_no_value_as_an_argument() {
    let dir = scratch("set-argument");
    write_site(&dir, "from: keyring\nkey: meridian-test:never-stored");
    // What follows the name is a document to read, never a value: a value
    // given there is refused as a file that is not there.
    let set = meridian(
        &dir,
        &["secret", "set", "deploy", "site.yaml", VALUE],
        &[],
        "",
    );
    assert!(!set.status.success());
    assert!(!stdout(&set).contains("stored"), "{}", stdout(&set));
    let help = meridian(&dir, &["secret", "set", "--help"], &[], "");
    let text = stdout(&help);
    assert!(text.contains("never from an argument"), "{text}");
    assert!(!text.contains("--value"), "{text}");
}

#[test]
fn a_store_this_machine_does_not_have_is_passed_over_for_the_next_place() {
    let dir = scratch("fallback");
    let token = dir.join("token.txt");
    fs::write(&token, format!("{VALUE}\n")).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    // A laptop's store first, then what a pipeline is given.
    write_site(
        &dir,
        "from: secret-service\nattributes: {app: meridian, name: deploy}\nfallback:\n  - from: \
         systemd-creds\n    key: deploy-token\n  - from: env\n    key: SITE_DEPLOY_TOKEN\n  - \
         from: file\n    path: token.txt",
    );

    // No session bus, no unit credentials, no variable: the file answers.
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    assert!(
        stdout(&check).contains("deploy  file   ok: the file token.txt is there"),
        "{}",
        stdout(&check)
    );
    let plan = meridian(&dir, &["plan", "site.yaml"], &[], "");
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    assert!(
        stdout(&plan).contains("+ create files/deployed"),
        "{}",
        stdout(&plan)
    );
    nothing_leaked(&plan, &dir, &["token.txt"]);

    // With the variable set, it answers before the file: the order written
    // is the order looked in. The file now holds something else.
    fs::write(&token, "something else\n").unwrap();
    let env = [("SITE_DEPLOY_TOKEN", VALUE)];
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &env, "");
    assert!(
        stdout(&check)
            .contains("deploy  env    ok: the environment variable SITE_DEPLOY_TOKEN is set"),
        "{}",
        stdout(&check)
    );
    let plan = meridian(&dir, &["plan", "site.yaml"], &env, "");
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    assert!(
        stdout(&plan).contains("+ create files/deployed"),
        "{}",
        stdout(&plan)
    );
    nothing_leaked(&plan, &dir, &[]);

    // And a credential the unit was given answers before both.
    let credentials = dir.join("credentials");
    fs::create_dir_all(&credentials).unwrap();
    fs::write(credentials.join("deploy-token"), format!("{VALUE}\n")).unwrap();
    let given = credentials.display().to_string();
    let env = [
        ("CREDENTIALS_DIRECTORY", given.as_str()),
        ("SITE_DEPLOY_TOKEN", "not the token"),
    ];
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &env, "");
    assert!(
        stdout(&check)
            .contains("deploy  systemd-creds  ok: this unit was given the credential deploy-token"),
        "{}",
        stdout(&check)
    );
    let plan = meridian(&dir, &["plan", "site.yaml"], &env, "");
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    assert!(
        stdout(&plan).contains("+ create files/deployed"),
        "{}",
        stdout(&plan)
    );
    nothing_leaked(&plan, &dir, &["deploy-token"]);
}

#[test]
fn when_no_place_has_the_secret_every_place_and_its_reason_is_given() {
    let dir = scratch("fallback-none");
    write_site(
        &dir,
        "from: secret-service\nattributes: {app: meridian, name: deploy}\nfallback:\n  - from: \
         systemd-creds\n    key: deploy-token\n  - from: env\n    key: SITE_DEPLOY_TOKEN",
    );
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
    assert!(!check.status.success());
    let text = stdout(&check);
    assert!(
        text.contains(
            "MISSING: the Secret Service needs a session bus, and DBUS_SESSION_BUS_ADDRESS is \
             not set; a server or a pipeline usually has none; no credentials were passed to \
             this process"
        ),
        "{text}"
    );
    assert!(
        text.contains("; the environment variable SITE_DEPLOY_TOKEN is not set"),
        "{text}"
    );
    assert!(
        stderr(&check).contains("error: 1 secret would not be found by plan or apply"),
        "{}",
        stderr(&check)
    );

    let plan = meridian(&dir, &["plan", "site.yaml"], &[], "");
    assert!(!plan.status.success());
    let text = said(&plan);
    assert!(
        text.contains("secret \"deploy\" (from: secret-service, then systemd-creds, then env): "),
        "{text}"
    );
    assert!(
        text.contains("the environment variable SITE_DEPLOY_TOKEN is not set"),
        "{text}"
    );

    // An optional secret that no place has reads as null, and the run goes
    // on without the resource that needed it.
    let document = site(
        "from: systemd-creds\nkey: deploy-token\noptional: true\nfallback:\n  - from: env\n    \
         key: SITE_DEPLOY_TOKEN",
    )
    .replace(
        &format!("base64Encode(secret.deploy) == '{VALUE_ENCODED}'"),
        "secret.deploy != null",
    );
    fs::write(dir.join("site.yaml"), document).unwrap();
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    assert!(
        stdout(&check).contains("absent, optional"),
        "{}",
        stdout(&check)
    );
    let plan = meridian(&dir, &["plan", "site.yaml"], &[], "");
    assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
    assert!(
        stdout(&plan).contains("+ create files/motd"),
        "{}",
        stdout(&plan)
    );
    assert!(
        !stdout(&plan).contains("files/deployed"),
        "{}",
        stdout(&plan)
    );
}

#[test]
fn a_place_that_refuses_stops_the_run_and_the_next_place_is_not_asked() {
    let dir = scratch("fallback-refused");
    let token = dir.join("token.txt");
    fs::write(&token, format!("{VALUE}\n")).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    write_site(
        &dir,
        "from: file\npath: token.txt\nfallback:\n  - from: exec\n    command: [touch, \
         the-next-place-was-asked]",
    );
    let plan = meridian(&dir, &["plan", "site.yaml"], &[], "");
    assert!(!plan.status.success());
    let text = said(&plan);
    assert!(
        text.contains(
            "secret \"deploy\" (from: file, then exec): the file token.txt has mode 0644"
        ),
        "{text}"
    );
    assert!(!dir.join("the-next-place-was-asked").exists());
    nothing_leaked(&plan, &dir, &["token.txt"]);

    let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
    assert!(!check.status.success());
    assert!(
        stdout(&check).contains("deploy  file   REFUSED: the file token.txt has mode 0644"),
        "{}",
        stdout(&check)
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_store_of_another_operating_system_is_said_to_be_unavailable() {
    let dir = scratch("keychain");
    write_site(&dir, "from: keychain\nservice: deploy\naccount: meridian");
    let validate = meridian(&dir, &["validate", "site.yaml"], &[], "");
    assert!(validate.status.success(), "{}", stderr(&validate));
    let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
    assert!(!check.status.success());
    assert!(
        stdout(&check).contains("deploy  keychain  UNAVAILABLE: the keychain is only on macOS"),
        "{}",
        stdout(&check)
    );
    let plan = meridian(&dir, &["plan", "site.yaml"], &[], "");
    assert!(!plan.status.success());
    assert!(
        said(&plan).contains("secret \"deploy\" (from: keychain): the keychain is only on macOS"),
        "{}",
        said(&plan)
    );
}

#[cfg(target_os = "linux")]
mod keyring {
    use super::*;

    /// Whether the kernel keyring can be used here; a container's sandbox
    /// often shuts it off, and then the command must say exactly that.
    fn is_open(set: &Output) -> bool {
        if set.status.success() {
            return true;
        }
        let text = stderr(set);
        assert!(
            text.contains("was not stored: the kernel keyring cannot be used here"),
            "{text}"
        );
        eprintln!("the kernel keyring is shut here, so only its refusal was tested");
        false
    }

    #[test]
    fn a_value_set_in_the_kernel_keyring_decides_a_when_and_is_nowhere_else() {
        let dir = scratch("keyring");
        let key = format!("meridian-test:cli:{}", std::process::id());
        write_site(&dir, &format!("from: keyring\nkey: \"{key}\""));

        let set = meridian(
            &dir,
            &["secret", "set", "deploy", "site.yaml"],
            &[],
            &format!("{VALUE}\n"),
        );
        if !is_open(&set) {
            return;
        }
        assert_eq!(
            stdout(&set),
            format!("stored deploy in the kernel keyring, key {key}\n")
        );
        nothing_leaked(&set, &dir, &[]);

        let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
        assert!(
            check.status.success(),
            "{}{}",
            stdout(&check),
            stderr(&check)
        );
        assert_eq!(
            stdout(&check),
            format!(
                "site (site.yaml)\n  deploy  keyring  ok: the kernel keyring holds the key {key}\n"
            )
        );

        let apply = meridian(&dir, &["apply", "site.yaml"], &[], "");
        assert!(
            apply.status.success(),
            "{}{}",
            stdout(&apply),
            stderr(&apply)
        );
        assert!(
            stdout(&apply).contains("files/deployed"),
            "{}",
            stdout(&apply)
        );
        // The value is in the kernel and nowhere on disk: not in the
        // working directory, not in the record of the run.
        nothing_leaked(&apply, &dir, &[]);
        assert!(dir.join(".meridian").exists());
    }

    #[test]
    fn the_keyring_is_passed_over_when_it_holds_nothing_and_written_first_when_it_can_be() {
        let dir = scratch("keyring-fallback");
        let key = format!("meridian-test:cli-fallback:{}", std::process::id());
        // The Secret Service has no bus here, so the keyring is the first
        // writable place that can be used.
        write_site(
            &dir,
            &format!(
                "from: env\nkey: SITE_DEPLOY_TOKEN\nfallback:\n  - from: secret-service\n    \
                 attributes: {{app: meridian}}\n  - from: keyring\n    key: \"{key}\""
            ),
        );
        let check = meridian(&dir, &["secret", "check", "site.yaml"], &[], "");
        if stdout(&check).contains("cannot be used here") {
            eprintln!("the kernel keyring is shut here");
            return;
        }
        assert!(!check.status.success());
        assert!(
            stdout(&check).contains(&format!("the kernel keyring holds no key {key}")),
            "{}",
            stdout(&check)
        );
        let set = meridian(&dir, &["secret", "set", "deploy", "site.yaml"], &[], VALUE);
        assert!(set.status.success(), "{}{}", stdout(&set), stderr(&set));
        assert_eq!(
            stdout(&set),
            format!("stored deploy in the kernel keyring, key {key}\n")
        );
        let plan = meridian(&dir, &["plan", "site.yaml"], &[], "");
        assert!(plan.status.success(), "{}{}", stdout(&plan), stderr(&plan));
        assert!(
            stdout(&plan).contains("+ create files/deployed"),
            "{}",
            stdout(&plan)
        );
        nothing_leaked(&plan, &dir, &[]);
    }
}
