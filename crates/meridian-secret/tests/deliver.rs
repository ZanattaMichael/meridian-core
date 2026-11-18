//! A secret file: only the user can reach it, it holds exactly what it was
//! given, and it is gone afterwards, even after a run that was killed.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use meridian_secret::{sweep_stale, Part, SecretFile, SecretValue};

fn secret(text: &str) -> SecretValue {
    SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn the_file_and_its_directory_are_for_the_user_alone() {
    let password = secret("p4ss w0rd");
    let file = SecretFile::create("vars.json", &[Part::Secret(&password)]).unwrap();
    let dir = file.path().parent().unwrap();

    assert_eq!(mode(file.path()), 0o600, "the file");
    assert_eq!(mode(dir), 0o700, "its directory");
    assert_eq!(file.path().file_name().unwrap(), "vars.json");
    // Nothing else is in the directory.
    assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
    // It is this user's.
    let owner = fs::metadata(file.path()).unwrap().uid();
    assert_eq!(owner, fs::metadata(dir).unwrap().uid());
    // The directory's name cannot be guessed from one run to the next.
    let other = SecretFile::create("vars.json", &[Part::Secret(&password)]).unwrap();
    assert_ne!(other.path(), file.path());
}

#[test]
fn the_file_holds_its_parts_in_order_and_nothing_else() {
    let password = secret("p4ss\"w0rd");
    let user = secret("deploy");
    let file = SecretFile::create(
        "login.txt",
        &[
            Part::Text("user="),
            Part::Secret(&user),
            Part::Text("\npassword="),
            Part::Secret(&password),
            Part::Text("\n"),
        ],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(file.path()).unwrap(),
        "user=deploy\npassword=p4ss\"w0rd\n"
    );
    let empty = SecretFile::create("empty", &[]).unwrap();
    assert_eq!(fs::read(empty.path()).unwrap(), b"");
}

#[test]
fn the_file_is_on_a_memory_filesystem_where_there_is_one() {
    let file = SecretFile::create("x", &[Part::Text("x")]).unwrap();
    let has_memory_place = ["/dev/shm"]
        .iter()
        .any(|p| std::path::Path::new(p).is_dir())
        || std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| PathBuf::from(d).is_dir());
    assert_eq!(file.in_memory(), has_memory_place, "{:?}", file.path());
    if file.in_memory() {
        assert!(
            !file.path().starts_with(std::env::temp_dir())
                || std::env::temp_dir().starts_with("/dev/shm"),
            "{:?}",
            file.path()
        );
    }
}

#[test]
fn dropping_the_file_removes_it_and_its_directory() {
    let password = secret("gone afterwards");
    let file = SecretFile::create("vars.json", &[Part::Secret(&password)]).unwrap();
    let path = file.path().to_owned();
    let dir = path.parent().unwrap().to_owned();
    assert!(path.exists());
    drop(file);
    assert!(!path.exists(), "the file is left behind");
    assert!(!dir.exists(), "the directory is left behind");
}

#[test]
fn a_tool_that_wrote_beside_the_file_does_not_stop_the_cleanup() {
    let file = SecretFile::create("vars.json", &[Part::Text("x")]).unwrap();
    let dir = file.path().parent().unwrap().to_owned();
    // Some tools drop a lock or a cache file next to what they are given.
    fs::write(dir.join("vars.json.lock"), "lock").unwrap();
    drop(file);
    assert!(!dir.exists());
}

#[test]
fn a_name_that_is_a_path_is_refused() {
    for name in ["", "a/b", "../x", "..", ".", "a\\b"] {
        let err = SecretFile::create(name, &[]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{name:?}");
    }
}

#[test]
fn what_a_killed_run_left_behind_is_removed_by_the_next() {
    const ROLE: &str = "MERIDIAN_SECRET_TEST_ROLE";
    if std::env::var(ROLE).as_deref() == Ok("leave-behind") {
        let password = secret("left by a killed run");
        let file = SecretFile::create("vars.json", &[Part::Secret(&password)]).unwrap();
        println!("{}", file.path().display());
        // Die without running any destructor, as a kill would.
        std::process::abort();
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "what_a_killed_run_left_behind_is_removed_by_the_next",
            "--exact",
            "--nocapture",
        ])
        .env(ROLE, "leave-behind")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "the child was meant to die");
    let printed = String::from_utf8_lossy(&output.stdout);
    let left = printed
        .lines()
        .find(|l| l.contains("meridian-secrets-"))
        .map(PathBuf::from)
        .expect("the child says where its file is");
    assert!(left.exists(), "the killed run's file is still there");
    assert_eq!(mode(&left), 0o600, "and still only the user's");

    // A live run's files are not touched.
    let mine = SecretFile::create("mine", &[Part::Text("x")]).unwrap();
    let removed = sweep_stale();
    assert!(removed >= 1, "nothing was swept");
    assert!(!left.exists(), "the file outlived the sweep");
    assert!(!left.parent().unwrap().exists());
    assert!(mine.path().exists(), "the sweep removed a live run's file");
}

#[test]
fn a_file_beside_the_secret_file_is_as_private_and_goes_with_it() {
    let password = secret("p4ss");
    let file = SecretFile::create("common.yaml", &[Part::Secret(&password)]).unwrap();
    let dir = file.dir().to_path_buf();
    assert_eq!(dir, file.path().parent().unwrap());
    let config = file.beside("hiera.yaml", "version: 5\n").unwrap();
    assert_eq!(config, dir.join("hiera.yaml"));
    assert_eq!(fs::read_to_string(&config).unwrap(), "version: 5\n");
    assert_eq!(mode(&config), 0o600);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
    // A name is a name: nothing is written outside the directory.
    for bad in ["", ".", "..", "a/b", "a\\b", "../escape"] {
        let err = file.beside(bad, "x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
    }
    drop(file);
    assert!(!config.exists());
    assert!(!dir.exists());
}

#[test]
fn a_shell_word_and_a_dotenv_value_are_escaped_for_what_reads_them() {
    let tricky = secret("it's $5 \"q\"\\\n\tend");
    let file = SecretFile::create(
        "both",
        &[
            Part::Text("printf '%s' '"),
            Part::ShellSingleQuoted(&tricky),
            Part::Text("'\nX=\""),
            Part::DotenvString(&tricky),
            Part::Text("\"\n"),
        ],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(file.path()).unwrap(),
        "printf '%s' 'it'\\''s $5 \"q\"\\\n\tend'\nX=\"it's $$5 \\\"q\\\"\\\\\\n\\tend\"\n"
    );
}

#[test]
fn a_value_put_together_from_parts_holds_exactly_their_text() {
    let password = secret("p4ss\"w0rd");
    let whole = SecretValue::from_parts(&[
        Part::Text("{\"password\":\""),
        Part::JsonString(&password),
        Part::Text("\"}\n"),
    ])
    .unwrap();
    whole.expose(|bytes| assert_eq!(bytes, b"{\"password\":\"p4ss\\\"w0rd\"}\n"));
    let nothing = SecretValue::from_parts(&[]).unwrap();
    assert!(nothing.is_empty());
}

#[test]
fn a_secret_that_is_not_text_cannot_be_escaped() {
    let bytes = SecretValue::from_reader(&[0xffu8, 0xfe][..], 16).unwrap();
    assert!(SecretValue::from_parts(&[Part::ShellSingleQuoted(&bytes)]).is_err());
    assert!(SecretValue::from_parts(&[Part::DotenvString(&bytes)]).is_err());
    assert!(SecretValue::from_parts(&[Part::JsonString(&bytes)]).is_err());
    // As bytes it is fine.
    assert_eq!(
        SecretValue::from_parts(&[Part::Secret(&bytes)])
            .unwrap()
            .len(),
        2
    );
}
