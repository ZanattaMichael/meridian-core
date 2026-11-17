//! Plugin signatures, end to end: `meridian signing` makes a key and signs a
//! real plugin binary, and `meridian plugins` starts it or refuses to,
//! depending on the policy and the keys it is told to trust.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory holding a copy of the fake target's plugin binary.
fn plugin_dir(test: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let built = fs::read_dir(meridian_target_fake::plugin_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_stem()
                .is_some_and(|name| name == "meridian-target-fake")
        })
        .expect("the fake plugin is built");
    let binary = dir.join(built.file_name().unwrap());
    fs::copy(&built, &binary).unwrap();
    (dir, binary)
}

fn meridian(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .args(args)
        .env_remove("MERIDIAN_PLUGIN_DIR")
        .env_remove("MERIDIAN_PLUGIN_SIGNATURES")
        .env_remove("MERIDIAN_TRUSTED_KEYS")
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Makes a key pair in `dir` under `name`, and returns the public key's path.
fn keygen(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let secret = dir.join(format!("{name}.key"));
    let public = dir.join(format!("{name}.pub"));
    let made = meridian(&[
        "signing",
        "keygen",
        "--secret",
        secret.to_str().unwrap(),
        "--public",
        public.to_str().unwrap(),
    ]);
    assert!(made.status.success(), "{}", stderr(&made));
    (secret, public)
}

fn plugins(dir: &Path, more: &[&str]) -> Output {
    let mut args = vec!["plugins", "--plugin-dir", dir.to_str().unwrap()];
    args.extend_from_slice(more);
    meridian(&args)
}

#[test]
fn a_key_pair_is_made_once_and_the_secret_half_is_private() {
    let (dir, _) = plugin_dir("keygen");
    let (secret, public) = keygen(&dir, "release");
    assert!(fs::read_to_string(&secret)
        .unwrap()
        .starts_with("meridian-secret-key-v1 "));
    assert!(fs::read_to_string(&public)
        .unwrap()
        .starts_with("meridian-public-key-v1 "));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // A second run does not write over the first key.
    let before = fs::read_to_string(&secret).unwrap();
    let again = meridian(&[
        "signing",
        "keygen",
        "--secret",
        secret.to_str().unwrap(),
        "--public",
        public.to_str().unwrap(),
    ]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("already exists; a key is not written over"),
        "{}",
        stderr(&again)
    );
    assert_eq!(fs::read_to_string(&secret).unwrap(), before);
}

#[test]
fn a_signed_plugin_starts_when_its_key_is_trusted_and_only_then() {
    let (dir, binary) = plugin_dir("required");
    let (secret, public) = keygen(&dir, "release");
    let (_, stranger) = keygen(&dir, "stranger");
    let require = ["--plugin-signatures", "require"];

    // Unsigned, and signatures are required: it is not started.
    let refused = plugins(
        &dir,
        &[&require[..], &["--trusted-key", public.to_str().unwrap()]].concat(),
    );
    assert!(!refused.status.success());
    let err = stderr(&refused);
    assert!(err.contains("the plugin was not started"), "{err}");
    assert!(err.contains("it is not signed: there is no "), "{err}");

    let signed = meridian(&[
        "signing",
        "sign",
        "--key",
        secret.to_str().unwrap(),
        binary.to_str().unwrap(),
    ]);
    assert!(signed.status.success(), "{}", stderr(&signed));
    assert!(dir
        .join(format!(
            "{}.sig",
            binary.file_name().unwrap().to_str().unwrap()
        ))
        .is_file());

    // Signed by a trusted key: it starts, and the listing says who signed it.
    let listed = plugins(
        &dir,
        &[&require[..], &["--trusted-key", public.to_str().unwrap()]].concat(),
    );
    assert!(listed.status.success(), "{}", stderr(&listed));
    let out = stdout(&listed);
    assert!(out.contains("fake\n"), "{out}");
    assert!(
        out.contains("  signature: verified, signed by key "),
        "{out}"
    );

    // A directory of keys is every `.pub` file in it.
    let listed = plugins(
        &dir,
        &[&require[..], &["--trusted-key", dir.to_str().unwrap()]].concat(),
    );
    assert!(listed.status.success(), "{}", stderr(&listed));

    // Signed, but not by a key this run trusts.
    let refused = plugins(
        &dir,
        &[&require[..], &["--trusted-key", stranger.to_str().unwrap()]].concat(),
    );
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("which is not a trusted key"),
        "{}",
        stderr(&refused)
    );

    // `signing verify` answers the same question without starting anything.
    let verified = meridian(&[
        "signing",
        "verify",
        "--trusted-key",
        public.to_str().unwrap(),
        binary.to_str().unwrap(),
    ]);
    assert!(verified.status.success(), "{}", stderr(&verified));
    assert!(stdout(&verified).contains(": verified, signed by key "));
    let unverified = meridian(&[
        "signing",
        "verify",
        "--trusted-key",
        stranger.to_str().unwrap(),
        binary.to_str().unwrap(),
    ]);
    assert!(!unverified.status.success());
    assert!(stdout(&unverified).contains(": invalid: it was signed by key "));
}

#[test]
fn a_plugin_changed_after_signing_is_refused() {
    let (dir, binary) = plugin_dir("changed");
    let (secret, public) = keygen(&dir, "release");
    let signed = meridian(&[
        "signing",
        "sign",
        "--key",
        secret.to_str().unwrap(),
        binary.to_str().unwrap(),
    ]);
    assert!(signed.status.success(), "{}", stderr(&signed));

    let mut bytes = fs::read(&binary).unwrap();
    bytes.extend_from_slice(b"\0");
    fs::write(&binary, bytes).unwrap();

    let refused = plugins(
        &dir,
        &[
            "--plugin-signatures",
            "require",
            "--trusted-key",
            public.to_str().unwrap(),
        ],
    );
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("its contents have changed"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn warn_starts_the_plugin_and_says_so_and_off_does_not_look() {
    let (dir, _) = plugin_dir("warn");

    let warned = plugins(&dir, &["--plugin-signatures", "warn"]);
    assert!(warned.status.success(), "{}", stderr(&warned));
    assert!(
        stderr(&warned).contains("warning: plugin ") && stderr(&warned).contains(" is not signed"),
        "{}",
        stderr(&warned)
    );
    assert!(stdout(&warned).contains("  signature: unsigned"));

    // A build from source checks nothing unless asked to.
    let quiet = plugins(&dir, &[]);
    assert!(quiet.status.success(), "{}", stderr(&quiet));
    assert_eq!(stderr(&quiet), "");
    assert!(stdout(&quiet).contains("  signature: not checked"));

    let bad = plugins(&dir, &["--plugin-signatures", "strict"]);
    assert_eq!(bad.status.code(), Some(2));
}
