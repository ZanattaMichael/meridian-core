//! The operating system's stores as providers, and a secret with several
//! places: which place answers, which is passed over, which stops the
//! search, and that no error or report ever carries a value.
//!
//! The environment is shared by every test of this file, so each takes
//! [`ENVIRONMENT`] before it reads or changes a variable.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use meridian_secret::{
    fetch_first, from_keychain, from_keyring, from_secret_service, from_systemd_credential,
    probe_first, store_first, FetchError, Probe, SecretValue, Source, PROVIDERS,
};

const VALUE: &str = "kq7Zp2-not-a-real-password-9fXw";
const OTHER: &str = "tok_Zx81-not-a-real-token";

static ENVIRONMENT: Mutex<()> = Mutex::new(());

fn environment() -> MutexGuard<'static, ()> {
    ENVIRONMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("stores-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn secret(text: &str) -> SecretValue {
    SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
}

fn is(value: &SecretValue, text: &str) -> bool {
    value.expose(|bytes| bytes == text.as_bytes())
}

fn source(from: &str, key: &str) -> Source {
    Source {
        from: from.to_owned(),
        key: key.to_owned(),
        ..Default::default()
    }
}

fn attributes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn service(pairs: &[(&str, &str)]) -> Source {
    Source {
        from: "secret-service".to_owned(),
        attributes: attributes(pairs),
        ..Default::default()
    }
}

/// Asserts nothing a store said holds a value.
fn says_nothing_of(text: &str) {
    for value in [VALUE, OTHER] {
        assert!(!text.contains(value), "{text}");
    }
}

/// Sets a variable for as long as the guard lives, then puts back what it
/// was.
struct Set {
    name: &'static str,
    was: Option<std::ffi::OsString>,
}

impl Set {
    fn to(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Set {
        let was = std::env::var_os(name);
        std::env::set_var(name, value);
        Set { name, was }
    }

    fn removed(name: &'static str) -> Set {
        let was = std::env::var_os(name);
        std::env::remove_var(name);
        Set { name, was }
    }
}

impl Drop for Set {
    fn drop(&mut self) {
        match &self.was {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn the_providers_are_the_built_in_three_and_the_four_stores() {
    assert_eq!(
        PROVIDERS,
        [
            "env",
            "exec",
            "file",
            "keychain",
            "keyring",
            "secret-service",
            "systemd-creds"
        ]
    );
}

#[test]
fn each_store_says_what_it_needs_and_what_means_nothing_to_it() {
    let needs = |source: Source| {
        source
            .problems()
            .into_iter()
            .map(|(field, why)| format!("{field}: {why}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        needs(source("keyring", "")),
        ["key: from: keyring needs key, the description of the key in the kernel keyring"]
    );
    assert_eq!(
        needs(source("systemd-creds", "")),
        ["key: from: systemd-creds needs key, the name of the credential the unit was given"]
    );
    assert_eq!(
        needs(source("secret-service", "")),
        ["attributes: from: secret-service needs attributes, the attributes the item was stored \
          with"]
    );
    assert_eq!(
        needs(source("keychain", "")),
        [
            "service: from: keychain needs service, the keychain item's service",
            "account: from: keychain needs account, the keychain item's account"
        ]
    );
    // What belongs to another provider is refused, by name.
    let mut mixed = source("keyring", "deploy");
    mixed.path = "/etc/token".to_owned();
    mixed.service = "deploy".to_owned();
    mixed.attributes = attributes(&[("app", "meridian")]);
    mixed.keep_trailing_newline = true;
    assert_eq!(
        needs(mixed),
        [
            "path: path means nothing to from: keyring; remove it",
            "service: service means nothing to from: keyring; remove it",
            "attributes: attributes means nothing to from: keyring; remove it",
            "keepTrailingNewline: keepTrailingNewline means nothing to from: keyring; remove it"
        ]
    );
    let mut keychain = source("keychain", "a-key");
    keychain.service = "deploy".to_owned();
    keychain.account = "meridian".to_owned();
    assert_eq!(
        needs(keychain),
        ["key: key means nothing to from: keychain; remove it"]
    );
    // A credential may keep its line ending; nothing else of a file's.
    let mut credential = source("systemd-creds", "token");
    credential.keep_trailing_newline = true;
    assert!(credential.problems().is_empty());
    credential.allow_wide_mode = true;
    assert_eq!(
        needs(credential),
        ["allowWideMode: allowWideMode means nothing to from: systemd-creds; remove it"]
    );
    // A provider that is not one names the ones that are.
    assert_eq!(
        needs(source("credential-manager", "x")),
        [
            "from: \"credential-manager\" is not a provider this version has; from is one of env, \
          exec, file, keychain, keyring, secret-service, systemd-creds"
        ]
    );
}

#[test]
fn each_store_names_its_place_without_anything_read_from_it() {
    assert_eq!(
        source("keyring", "meridian:deploy").place(),
        "the kernel keyring, key meridian:deploy"
    );
    assert_eq!(
        source("systemd-creds", "deploy-token").place(),
        "the unit's credential deploy-token"
    );
    assert_eq!(
        service(&[("app", "meridian"), ("name", "deploy")]).place(),
        "the Secret Service, item app=meridian name=deploy"
    );
    let keychain = Source {
        from: "keychain".to_owned(),
        service: "deploy".to_owned(),
        account: "meridian".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        keychain.place(),
        "the keychain, service deploy and account meridian"
    );
    // Only the keyring and the Secret Service are written by meridian.
    let writable: Vec<&str> = PROVIDERS
        .iter()
        .copied()
        .filter(|from| source(from, "x").writable())
        .collect();
    assert_eq!(writable, ["keyring", "secret-service"]);
}

/// A key description no other test or run of the tests uses.
#[cfg(target_os = "linux")]
fn key(test: &str) -> String {
    format!("meridian-test:{test}:{}", std::process::id())
}

/// Whether the kernel keyring can be used here. A container's sandbox
/// often shuts it off, and then the provider must say exactly that.
#[cfg(target_os = "linux")]
fn keyring_is_open(test: &str) -> bool {
    match source("keyring", &key(test)).store("unused", &secret("probe")) {
        Ok(()) => true,
        Err(FetchError::Unavailable(why)) => {
            assert!(
                why.starts_with("the kernel keyring cannot be used here ("),
                "{why}"
            );
            eprintln!("the kernel keyring is shut here, so only its refusal was tested: {why}");
            false
        }
        Err(other) => panic!("the keyring neither took the key nor said it is shut: {other}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_kernel_keyring_gives_back_what_was_stored_in_it() {
    let _env = environment();
    if !keyring_is_open("round-trip") {
        return;
    }
    let name = key("round-trip-value");
    let place = source("keyring", &name);

    // Before anything is stored the key is missing, and a probe agrees.
    let missing = from_keyring(&name).unwrap_err();
    assert_eq!(
        missing,
        FetchError::Missing(format!("the kernel keyring holds no key {name}"))
    );
    assert_eq!(
        place.probe(),
        Probe::Absent(format!("the kernel keyring holds no key {name}"))
    );

    place
        .store("unused by the keyring", &secret(VALUE))
        .unwrap();
    assert_eq!(
        place.probe(),
        Probe::Present(format!("the kernel keyring holds the key {name}"))
    );
    let read = place.fetch().unwrap();
    assert!(is(&read, VALUE));
    assert_eq!(format!("{read:?}"), "(sensitive)");
    // The probe looked and did not read: what it says holds no value.
    says_nothing_of(place.probe().words());

    // Storing again replaces the key; it does not add a second.
    place.store("unused", &secret(OTHER)).unwrap();
    assert!(is(&from_keyring(&name).unwrap(), OTHER));
}

#[cfg(target_os = "linux")]
#[test]
fn the_kernel_keyring_keeps_every_byte_of_a_value() {
    let _env = environment();
    if !keyring_is_open("bytes") {
        return;
    }
    // A key's payload is bytes: a line ending, a NUL and a byte that is not
    // text all come back as they went in.
    let name = key("bytes-value");
    let payload: &[u8] = b"line one\nline two\n\0\xff end";
    let value = SecretValue::from_reader(payload, 4096).unwrap();
    source("keyring", &name).store("unused", &value).unwrap();
    let read = from_keyring(&name).unwrap();
    assert!(read.same(&value));
    assert_eq!(read.len(), payload.len());
}

#[cfg(target_os = "linux")]
#[test]
fn a_key_description_that_cannot_be_one_is_refused() {
    let _env = environment();
    let error = from_keyring("has\0a nul").unwrap_err();
    assert_eq!(
        error,
        FetchError::Failed("the key's description cannot hold a NUL character".to_owned())
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_kernel_keyring_is_unavailable_off_linux() {
    let unavailable = FetchError::Unavailable("the kernel keyring is only on Linux".to_owned());
    let place = source("keyring", "meridian:deploy");
    assert_eq!(from_keyring("meridian:deploy").unwrap_err(), unavailable);
    assert_eq!(place.fetch().unwrap_err(), unavailable);
    assert_eq!(place.store("x", &secret(VALUE)).unwrap_err(), unavailable);
    assert_eq!(
        place.probe(),
        Probe::Unavailable("the kernel keyring is only on Linux".to_owned())
    );
}

#[test]
fn a_unit_credential_is_read_from_the_directory_systemd_names() {
    let _env = environment();
    let dir = scratch("credential");
    fs::write(dir.join("deploy-token"), format!("{VALUE}\n")).unwrap();
    fs::write(dir.join("two-lines"), "first\nsecond\n\n").unwrap();
    let _set = Set::to("CREDENTIALS_DIRECTORY", &dir);

    // A credential written by a person usually ends its line; the secret
    // does not.
    let read = from_systemd_credential("deploy-token", false).unwrap();
    assert!(is(&read, VALUE));
    // Only one line ending goes, and none when it is asked for.
    assert!(is(
        &from_systemd_credential("two-lines", false).unwrap(),
        "first\nsecond\n"
    ));
    assert!(is(
        &from_systemd_credential("deploy-token", true).unwrap(),
        &format!("{VALUE}\n")
    ));

    let place = source("systemd-creds", "deploy-token");
    assert!(is(&place.fetch().unwrap(), VALUE));
    assert_eq!(
        place.probe(),
        Probe::Present("this unit was given the credential deploy-token".to_owned())
    );

    // A credential the unit was not given is missing.
    let absent = source("systemd-creds", "not-given");
    assert_eq!(
        absent.fetch().unwrap_err(),
        FetchError::Missing("this unit was given no credential not-given".to_owned())
    );
    assert_eq!(
        absent.probe(),
        Probe::Absent("this unit was given no credential not-given".to_owned())
    );
}

#[test]
fn a_unit_credential_is_one_name_and_never_a_path() {
    let _env = environment();
    let dir = scratch("credential-path");
    let inside = dir.join("given");
    fs::create_dir_all(&inside).unwrap();
    // A file beside the credentials directory, which a path could reach.
    fs::write(dir.join("outside"), VALUE).unwrap();
    let _set = Set::to("CREDENTIALS_DIRECTORY", &inside);
    for name in ["../outside", "..", ".", "a/b", "a\\b"] {
        let error = from_systemd_credential(name, false).unwrap_err();
        assert_eq!(
            error,
            FetchError::Failed(format!(
                "{name:?} is not the name of a credential: it is one name, not a path"
            ))
        );
        assert!(matches!(
            source("systemd-creds", name).probe(),
            Probe::Refused(_)
        ));
    }
}

#[test]
fn without_a_credentials_directory_the_unit_credentials_are_unavailable() {
    let _env = environment();
    let _unset = Set::removed("CREDENTIALS_DIRECTORY");
    let error = from_systemd_credential("deploy-token", false).unwrap_err();
    let FetchError::Unavailable(why) = &error else {
        panic!("{error:?}")
    };
    assert_eq!(
        why,
        "no credentials were passed to this process: CREDENTIALS_DIRECTORY is not set. systemd \
         sets it for a unit with LoadCredential= or LoadCredentialEncrypted="
    );
    assert_eq!(
        source("systemd-creds", "deploy-token").probe(),
        Probe::Unavailable(why.clone())
    );
    // An empty variable is no directory either.
    let _empty = Set::to("CREDENTIALS_DIRECTORY", "");
    assert!(matches!(
        from_systemd_credential("deploy-token", false),
        Err(FetchError::Unavailable(_))
    ));
}

#[test]
fn the_secret_service_is_unavailable_without_a_session_bus() {
    let _env = environment();
    let _unset = Set::removed("DBUS_SESSION_BUS_ADDRESS");
    let place = service(&[("app", "meridian")]);
    let why = "the Secret Service needs a session bus, and DBUS_SESSION_BUS_ADDRESS is not set; \
               a server or a pipeline usually has none";
    assert_eq!(
        from_secret_service(&place.attributes).unwrap_err(),
        FetchError::Unavailable(why.to_owned())
    );
    assert_eq!(
        place.fetch().unwrap_err(),
        FetchError::Unavailable(why.to_owned())
    );
    assert_eq!(
        place.store("Meridian: deploy", &secret(VALUE)).unwrap_err(),
        FetchError::Unavailable(why.to_owned())
    );
    assert_eq!(place.probe(), Probe::Unavailable(why.to_owned()));
}

#[cfg(unix)]
#[test]
fn the_secret_service_is_unavailable_without_its_tool() {
    let _env = environment();
    let empty = scratch("no-secret-tool");
    let _bus = Set::to("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
    let _path = Set::to("PATH", &empty);
    let place = service(&[("app", "meridian")]);
    let why = "the Secret Service is read with secret-tool, which was not found; it is in the \
               package libsecret-tools (or libsecret)";
    assert_eq!(
        place.fetch().unwrap_err(),
        FetchError::Unavailable(why.to_owned())
    );
    assert_eq!(
        place.store("Meridian: deploy", &secret(VALUE)).unwrap_err(),
        FetchError::Unavailable(why.to_owned())
    );
    assert_eq!(
        place.probe(),
        Probe::Unavailable(
            "the Secret Service is read with secret-tool, which was not found".to_owned()
        )
    );
}

/// A stand-in for `secret-tool` that keeps each item in a file named by its
/// attributes, and records the arguments it was run with.
#[cfg(unix)]
fn stand_in_secret_tool(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    let items = dir.join("items");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&items).unwrap();
    let script = format!(
        "#!/bin/sh\nitems='{items}'\nprintf '%s\\n' \"$*\" >> \"$items/../arguments\"\nop=\"$1\"\n\
         shift\ncase \"$op\" in\n  lookup)\n    name=$(printf '%s_' \"$@\")\n    [ -f \
         \"$items/$name\" ] || exit 1\n    cat \"$items/$name\"\n    ;;\n  store)\n    \
         label=\"$1\"\n    shift\n    name=$(printf '%s_' \"$@\")\n    case \"$name\" in \
         refuse_*) echo 'this is printed by a tool that fails' >&2; exit 1;; esac\n    cat > \
         \"$items/$name\"\n    printf '%s' \"$label\" > \"$items/$name.label\"\n    ;;\n  *)\n    \
         exit 2\n    ;;\nesac\n",
        items = items.display()
    );
    let tool = bin.join("secret-tool");
    fs::write(&tool, script).unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[cfg(unix)]
fn path_with(first: &Path) -> std::ffi::OsString {
    let mut dirs = vec![first.to_path_buf()];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).unwrap()
}

#[cfg(unix)]
#[test]
fn the_secret_service_is_read_and_written_through_its_tool() {
    let _env = environment();
    let dir = scratch("secret-service");
    let bin = stand_in_secret_tool(&dir);
    let _bus = Set::to("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
    let _path = Set::to("PATH", path_with(&bin));
    let place = service(&[("name", "deploy"), ("app", "meridian")]);

    // Nothing is stored yet. The tool fails and prints nothing; that is an
    // item that is missing, not a failure.
    assert_eq!(
        place.fetch().unwrap_err(),
        FetchError::Missing("the Secret Service holds no item with these attributes".to_owned())
    );
    // A probe does not ask the service: asking can open a prompt.
    let asked_before = fs::read_to_string(dir.join("arguments")).unwrap();
    assert_eq!(
        place.probe(),
        Probe::Unknown(
            "the Secret Service is only asked by plan and apply: asking can open a prompt to \
             unlock it (item app=meridian name=deploy)"
                .to_owned()
        )
    );
    assert_eq!(
        fs::read_to_string(dir.join("arguments")).unwrap(),
        asked_before
    );

    place.store("Meridian: deploy", &secret(VALUE)).unwrap();
    assert!(is(&place.fetch().unwrap(), VALUE));
    assert_eq!(
        fs::read_to_string(dir.join("items/app_meridian_name_deploy_.label")).unwrap(),
        "--label=Meridian: deploy"
    );

    // The value went to the tool on its standard input. It was in no
    // argument: arguments are in the process list for every user to read.
    let arguments = fs::read_to_string(dir.join("arguments")).unwrap();
    assert_eq!(
        arguments,
        "lookup app meridian name deploy\nstore --label=Meridian: deploy app meridian name \
         deploy\nlookup app meridian name deploy\n"
    );
    says_nothing_of(&arguments);

    // Another item is another file: the attributes are what finds it.
    let other = service(&[("app", "meridian"), ("name", "backup")]);
    assert!(matches!(other.fetch(), Err(FetchError::Missing(_))));
}

#[cfg(unix)]
#[test]
fn a_secret_service_that_refuses_says_so_without_the_value_or_what_the_tool_printed() {
    let _env = environment();
    let dir = scratch("secret-service-refuses");
    let bin = stand_in_secret_tool(&dir);
    let _bus = Set::to("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/bus");
    let _path = Set::to("PATH", path_with(&bin));
    let place = service(&[("refuse", "yes")]);
    let error = place.store("Meridian: x", &secret(VALUE)).unwrap_err();
    assert_eq!(
        error,
        FetchError::Failed("the Secret Service refused to store the item".to_owned())
    );
    says_nothing_of(&error.to_string());
    assert!(!error.to_string().contains("printed by a tool"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_keychain_is_unavailable_off_macos() {
    let unavailable = FetchError::Unavailable("the keychain is only on macOS".to_owned());
    assert_eq!(
        from_keychain("deploy", "meridian").unwrap_err(),
        unavailable
    );
    let place = Source {
        from: "keychain".to_owned(),
        service: "deploy".to_owned(),
        account: "meridian".to_owned(),
        ..Default::default()
    };
    assert_eq!(place.fetch().unwrap_err(), unavailable);
    assert_eq!(
        place.probe(),
        Probe::Unavailable("the keychain is only on macOS".to_owned())
    );
    // The keychain is written with its own tool, not by meridian.
    assert!(!place.writable());
    assert_eq!(
        place.store("x", &secret(VALUE)).unwrap_err(),
        FetchError::Unavailable(
            "from: keychain is not a place meridian writes; it is written with its own tool"
                .to_owned()
        )
    );
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::*;
    use std::process::Command;

    fn security(args: &[&str]) -> bool {
        Command::new("/usr/bin/security")
            .args(args)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    #[test]
    fn an_item_the_keychain_does_not_hold_is_missing_and_a_probe_does_not_ask() {
        let service = format!("meridian-test-absent-{}", std::process::id());
        assert_eq!(
            from_keychain(&service, "nobody").unwrap_err(),
            FetchError::Missing(format!(
                "the keychain holds no item for service {service} and account nobody"
            ))
        );
        let place = Source {
            from: "keychain".to_owned(),
            service: service.clone(),
            account: "nobody".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            place.probe(),
            Probe::Unknown(format!(
                "the keychain is only asked by plan and apply: asking can open a prompt \
                 (service {service}, account nobody)"
            ))
        );
    }

    /// Reads an item back from a keychain made for the test. It changes the
    /// user's keychain search list while it runs, so it is only run where
    /// that is nobody's own machine.
    #[test]
    fn an_item_in_a_keychain_is_read() {
        if std::env::var_os("CI").is_none() {
            eprintln!("not run outside CI: it adds a keychain to the search list");
            return;
        }
        let dir = scratch("keychain");
        let keychain = dir.join("meridian-test.keychain-db");
        let keychain = keychain.to_str().unwrap();
        let service = format!("meridian-test-{}", std::process::id());
        let listed = Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .output()
            .unwrap();
        let before: Vec<String> = String::from_utf8_lossy(&listed.stdout)
            .lines()
            .map(|line| line.trim().trim_matches('"').to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        assert!(security(&["create-keychain", "-p", "test", keychain]));
        assert!(security(&["set-keychain-settings", keychain]));
        assert!(security(&["unlock-keychain", "-p", "test", keychain]));
        let mut search = vec!["list-keychains", "-d", "user", "-s", keychain];
        search.extend(before.iter().map(String::as_str));
        assert!(security(&search));
        // `-A` lets any program read the item without a prompt, which is
        // what a keychain item made by another program otherwise opens.
        let added = security(&[
            "add-generic-password",
            "-s",
            &service,
            "-a",
            "meridian",
            "-w",
            VALUE,
            "-A",
            keychain,
        ]);
        let read = from_keychain(&service, "meridian");
        let missing = from_keychain(&service, "someone-else");
        // Put the search list back before anything is asserted.
        let mut restore = vec!["list-keychains", "-d", "user", "-s"];
        restore.extend(before.iter().map(String::as_str));
        security(&restore);
        security(&["delete-keychain", keychain]);

        assert!(added, "the item could not be added to the test keychain");
        let read = read.unwrap();
        assert!(is(&read, VALUE));
        assert!(matches!(missing, Err(FetchError::Missing(_))));
    }
}

/// A file only its owner can read, holding `text`.
fn private_file(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn file(path: &Path) -> Source {
    Source {
        from: "file".to_owned(),
        path: path.display().to_string(),
        ..Default::default()
    }
}

#[test]
fn the_first_place_that_has_the_secret_answers_and_later_places_are_not_asked() {
    let _env = environment();
    let dir = scratch("first");
    private_file(&dir.join("first"), VALUE);
    private_file(&dir.join("second"), OTHER);
    let places = [file(&dir.join("first")), file(&dir.join("second"))];
    let (at, value) = fetch_first(&places).unwrap();
    assert_eq!(at, 0);
    assert!(is(&value, VALUE));
    assert_eq!(
        probe_first(&places),
        (
            0,
            Probe::Present(format!("the file {} is there", dir.join("first").display()))
        )
    );

    // A place that has nothing is passed over for the next.
    let places = [
        file(&dir.join("not-there")),
        source("env", "MERIDIAN_TEST_STORES_NOT_SET"),
        file(&dir.join("second")),
    ];
    let (at, value) = fetch_first(&places).unwrap();
    assert_eq!(at, 2);
    assert!(is(&value, OTHER));
    assert_eq!(probe_first(&places).0, 2);
}

#[test]
fn a_store_this_machine_does_not_have_is_passed_over() {
    let _env = environment();
    let _no_bus = Set::removed("DBUS_SESSION_BUS_ADDRESS");
    let _no_credentials = Set::removed("CREDENTIALS_DIRECTORY");
    let dir = scratch("passed-over");
    private_file(&dir.join("token"), VALUE);
    // A laptop's stores first, then the file a pipeline is given.
    let places = [
        service(&[("app", "meridian")]),
        source("systemd-creds", "deploy-token"),
        file(&dir.join("token")),
    ];
    let (at, value) = fetch_first(&places).unwrap();
    assert_eq!(at, 2);
    assert!(is(&value, VALUE));
    let (at, probe) = probe_first(&places);
    assert_eq!(at, 2);
    assert!(matches!(probe, Probe::Present(_)));
}

#[cfg(unix)]
#[test]
fn a_place_that_is_there_and_refuses_stops_the_search() {
    use std::os::unix::fs::PermissionsExt;
    let _env = environment();
    let dir = scratch("refused");
    // A file others can read is a mistake to fix. The next place is not
    // asked: its command would leave a marker if it ran.
    fs::write(dir.join("wide"), VALUE).unwrap();
    fs::set_permissions(dir.join("wide"), fs::Permissions::from_mode(0o644)).unwrap();
    let marker = dir.join("the-next-place-was-asked");
    let places = [
        file(&dir.join("wide")),
        Source {
            from: "exec".to_owned(),
            command: vec!["touch".to_owned(), marker.display().to_string()],
            ..Default::default()
        },
    ];
    let error = fetch_first(&places).unwrap_err();
    let FetchError::Failed(why) = &error else {
        panic!("{error:?}")
    };
    assert!(why.contains("has mode 0644"), "{why}");
    says_nothing_of(why);
    assert!(!marker.exists());
    let (at, probe) = probe_first(&places);
    assert_eq!(at, 0);
    assert!(matches!(probe, Probe::Refused(_)));
    assert!(!marker.exists());
}

#[test]
fn when_no_place_answers_every_reason_is_given() {
    let _env = environment();
    let _no_bus = Set::removed("DBUS_SESSION_BUS_ADDRESS");
    let _no_credentials = Set::removed("CREDENTIALS_DIRECTORY");
    let dir = scratch("none");
    let not_there = dir.join("not-there");

    // One place could have held it: the secret is missing.
    let places = [
        service(&[("app", "meridian")]),
        file(&not_there),
        source("env", "MERIDIAN_TEST_STORES_NOT_SET"),
    ];
    let why = format!(
        "the Secret Service needs a session bus, and DBUS_SESSION_BUS_ADDRESS is not set; a \
         server or a pipeline usually has none; the file {} does not exist; the environment \
         variable MERIDIAN_TEST_STORES_NOT_SET is not set",
        not_there.display()
    );
    assert_eq!(
        fetch_first(&places).unwrap_err(),
        FetchError::Missing(why.clone())
    );
    assert_eq!(probe_first(&places), (2, Probe::Absent(why)));

    // No place can be used here at all: that is said instead.
    let places = [
        service(&[("app", "meridian")]),
        source("systemd-creds", "deploy-token"),
    ];
    let error = fetch_first(&places).unwrap_err();
    let FetchError::Unavailable(why) = &error else {
        panic!("{error:?}")
    };
    assert!(why.contains("needs a session bus"), "{why}");
    assert!(
        why.contains("; no credentials were passed to this process"),
        "{why}"
    );
    assert_eq!(probe_first(&places), (1, Probe::Unavailable(why.clone())));
}

#[test]
fn a_value_is_stored_in_the_first_writable_place_that_can_be_used() {
    let _env = environment();
    let _no_bus = Set::removed("DBUS_SESSION_BUS_ADDRESS");
    let value = secret(VALUE);

    // No place is one meridian writes.
    let error = store_first(
        &[
            source("env", "MERIDIAN_TEST_STORES_NOT_SET"),
            file(Path::new("token")),
        ],
        "Meridian: deploy",
        &value,
    )
    .unwrap_err();
    assert_eq!(
        error,
        FetchError::Unavailable("none of its places is one meridian writes".to_owned())
    );

    // The only writable place cannot be used here: its reason is given.
    let error = store_first(
        &[
            source("env", "MERIDIAN_TEST_STORES_NOT_SET"),
            service(&[("app", "meridian")]),
        ],
        "Meridian: deploy",
        &value,
    )
    .unwrap_err();
    let FetchError::Unavailable(why) = &error else {
        panic!("{error:?}")
    };
    assert!(why.starts_with("the Secret Service needs a session bus"));
    says_nothing_of(why);
}

#[cfg(target_os = "linux")]
#[test]
fn a_store_that_cannot_be_used_is_passed_over_when_storing() {
    let _env = environment();
    if !keyring_is_open("store-first") {
        return;
    }
    let _no_bus = Set::removed("DBUS_SESSION_BUS_ADDRESS");
    let name = key("store-first-value");
    let places = [
        source("env", "MERIDIAN_TEST_STORES_NOT_SET"),
        service(&[("app", "meridian")]),
        source("keyring", &name),
    ];
    let at = store_first(&places, "Meridian: deploy", &secret(VALUE)).unwrap();
    assert_eq!(at, 2);
    // And that is the place a fetch then finds it in.
    let (found, value) = fetch_first(&places).unwrap();
    assert_eq!(found, 2);
    assert!(is(&value, VALUE));
}
