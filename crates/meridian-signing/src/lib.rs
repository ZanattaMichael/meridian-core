//! Signed files: deciding whether a file is the one a trusted key signed.
//!
//! This crate knows nothing about plugins. It signs and checks any file, and
//! the plugin host is its first user; the words below are about plugins
//! because that is where the check was first needed.
//!
//! A plugin is a program the host runs with the operator's own rights, so
//! "which program is this?" deserves an answer before it starts. A signed
//! plugin has a file beside it, `<binary>.sig`, holding an Ed25519 signature
//! over the binary's SHA-256 digest. The host checks it against the public
//! keys it has been told to trust and, depending on the [`Policy`], refuses,
//! warns about or accepts a plugin that does not pass.
//!
//! The check is local and offline: it needs no network, no clock and no
//! service to be up, so it can run every time a plugin is started. What it
//! proves is narrow and exact: these bytes are the bytes that the holder of a
//! trusted key signed. Where the key's trust comes from is the operator's
//! decision. A released `meridian` carries the public key its own release's
//! plugins were signed with; any other key is named with `--trusted-key`.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// The first line of a signature file, and of the message that is signed.
const SIGNATURE_HEADER: &str = "meridian-plugin-signature-v1";
/// The same, for a signature over bytes that are not a file: a statement.
/// It is a different name on purpose, so that a signature over a statement
/// is never also a valid signature for a file holding the statement's text,
/// and the other way round.
const STATEMENT_HEADER: &str = "meridian-statement-signature-v1";
/// The word a public key file starts with.
const PUBLIC_KEY_HEADER: &str = "meridian-public-key-v1";
/// The word a secret key file starts with.
const SECRET_KEY_HEADER: &str = "meridian-secret-key-v1";

/// What the host does about a plugin's signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    /// Signatures are not looked at.
    #[default]
    Off,
    /// A plugin that is unsigned, or whose signature does not pass, is
    /// started anyway and reported.
    Warn,
    /// Only a plugin with a signature from a trusted key is started.
    Require,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Off => "off",
            Policy::Warn => "warn",
            Policy::Require => "require",
        }
    }
}

impl std::str::FromStr for Policy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Policy::Off),
            "warn" => Ok(Policy::Warn),
            "require" => Ok(Policy::Require),
            other => Err(format!(
                "{other:?} is not a signature policy; use off, warn or require"
            )),
        }
    }
}

/// The policy and the keys a host checks plugins against.
#[derive(Clone, Debug, Default)]
pub struct Verification {
    pub policy: Policy,
    pub trusted: Vec<PublicKey>,
}

/// What checking one plugin's signature found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Signature {
    /// Signatures are off, or the target is compiled in.
    #[default]
    NotChecked,
    /// Signed by a trusted key, whose id this is.
    Verified { key_id: String },
    /// No signature file beside the binary.
    Unsigned,
    /// A signature file that does not vouch for this binary, and why.
    Invalid(String),
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Signature::NotChecked => f.write_str("not checked"),
            Signature::Verified { key_id } => write!(f, "verified, signed by key {key_id}"),
            Signature::Unsigned => f.write_str("unsigned"),
            Signature::Invalid(why) => write!(f, "invalid: {why}"),
        }
    }
}

impl Verification {
    /// A verification that checks nothing.
    pub fn off() -> Self {
        Verification::default()
    }

    /// Checks a plugin binary before it is started.
    ///
    /// `Ok` carries what was found, for showing; under [`Policy::Warn`] that
    /// may be a signature that did not pass. `Err` is a refusal: the plugin
    /// must not be started, and the text says why.
    pub fn check(&self, binary: &Path) -> Result<Signature, String> {
        if self.policy == Policy::Off {
            return Ok(Signature::NotChecked);
        }
        let found = verify_file(binary, &self.trusted);
        match (&found, self.policy) {
            (Signature::Verified { .. }, _) | (_, Policy::Warn) => Ok(found),
            (Signature::Unsigned, _) => Err(format!(
                "it is not signed: there is no {}. Plugin signatures are required; trust the \
                 plugin's signer with --trusted-key, or relax the check with \
                 --plugin-signatures warn",
                signature_path(binary).display()
            )),
            (Signature::Invalid(why), _) => Err(format!(
                "its signature does not pass: {why}. Plugin signatures are required, so it \
                 was not started"
            )),
            (Signature::NotChecked, _) => Ok(found),
        }
    }
}

/// The signature file that goes with a binary: its whole name, plus `.sig`.
pub fn signature_path(binary: &Path) -> PathBuf {
    let mut name = binary.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

/// An Ed25519 public key a host may trust.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// Reads a key from its text form: `meridian-public-key-v1 <hex>`, with
    /// anything after the key on the line treated as a comment. The bare hex
    /// is accepted too.
    pub fn parse(text: &str) -> Result<PublicKey, String> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .ok_or("the key is empty")?;
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        let hex = if first == PUBLIC_KEY_HEADER {
            words
                .next()
                .ok_or("the key has no value after its header")?
        } else if first == SECRET_KEY_HEADER {
            return Err("this is a secret key; a public key is needed here".to_owned());
        } else {
            first
        };
        let bytes: [u8; 32] = from_hex(hex)
            .and_then(|b| b.try_into().ok())
            .ok_or("a public key is 64 hexadecimal digits")?;
        VerifyingKey::from_bytes(&bytes)
            .map(PublicKey)
            .map_err(|_| "the value is not a valid Ed25519 public key".to_owned())
    }

    /// Reads a key from a file.
    pub fn read(path: &Path) -> Result<PublicKey, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("reading the key {}: {e}", path.display()))?;
        PublicKey::parse(&text).map_err(|e| format!("the key {}: {e}", path.display()))
    }

    /// A short name for the key: the first 16 hexadecimal digits of the
    /// SHA-256 of its bytes. It is what a signature file and a message call
    /// the key by; it identifies and is not itself trusted.
    pub fn id(&self) -> String {
        to_hex(&Sha256::digest(self.0.as_bytes())[..8])
    }

    /// The key's text form, as [`PublicKey::parse`] reads it.
    pub fn to_text(&self) -> String {
        format!("{PUBLIC_KEY_HEADER} {}", to_hex(self.0.as_bytes()))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.id())
    }
}

/// An Ed25519 signing key. It is never printed by `Debug`.
pub struct SecretKey(SigningKey);

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey(for {})", self.public().id())
    }
}

impl SecretKey {
    /// A new key from the operating system's random source.
    pub fn generate() -> Result<SecretKey, String> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)
            .map_err(|e| format!("the system's random source is not available: {e}"))?;
        Ok(SecretKey(SigningKey::from_bytes(&seed)))
    }

    /// Reads a key from its text form: `meridian-secret-key-v1 <hex>`.
    pub fn parse(text: &str) -> Result<SecretKey, String> {
        let mut words = text.split_whitespace();
        if words.next() != Some(SECRET_KEY_HEADER) {
            return Err(format!("a secret key file starts with {SECRET_KEY_HEADER}"));
        }
        let bytes: [u8; 32] = words
            .next()
            .and_then(from_hex)
            .and_then(|b| b.try_into().ok())
            .ok_or("a secret key is 64 hexadecimal digits")?;
        Ok(SecretKey(SigningKey::from_bytes(&bytes)))
    }

    /// Reads a key from a file.
    pub fn read(path: &Path) -> Result<SecretKey, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("reading the key {}: {e}", path.display()))?;
        SecretKey::parse(&text).map_err(|e| format!("the key {}: {e}", path.display()))
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// The key's text form, as [`SecretKey::parse`] reads it.
    pub fn to_text(&self) -> String {
        format!("{SECRET_KEY_HEADER} {}", to_hex(&self.0.to_bytes()))
    }

    /// Signs a binary and returns the text of its signature file.
    pub fn sign_file(&self, binary: &Path) -> Result<String, String> {
        let digest = digest_of(binary)?;
        let signature = self.0.sign(message(SIGNATURE_HEADER, &digest).as_bytes());
        Ok(format!(
            "{SIGNATURE_HEADER}\nkey {}\nsha256 {digest}\nsig {}\n",
            self.public().id(),
            to_hex(&signature.to_bytes())
        ))
    }
}

impl SecretKey {
    /// Signs bytes that are not a file on disk, such as a statement about
    /// something, and returns the text of the signature. It has the shape of
    /// a file's signature under another first line, and neither passes for
    /// the other: [`verify_file`] refuses it, and [`verify_bytes`] refuses
    /// what [`SecretKey::sign_file`] writes.
    pub fn sign_bytes(&self, bytes: &[u8]) -> String {
        let digest = to_hex(&Sha256::digest(bytes));
        let signature = self.0.sign(message(STATEMENT_HEADER, &digest).as_bytes());
        format!(
            "{STATEMENT_HEADER}\nkey {}\nsha256 {digest}\nsig {}\n",
            self.public().id(),
            to_hex(&signature.to_bytes())
        )
    }
}

/// What is signed: the format's name and the digest. Naming the format in
/// the message means a signature made for something else can never be
/// presented as a plugin's, and a statement's never as a file's.
fn message(header: &str, digest: &str) -> String {
    format!("{header}\n{digest}\n")
}

/// Checks the signature file beside a binary against a set of trusted keys.
pub fn verify_file(binary: &Path, trusted: &[PublicKey]) -> Signature {
    let text = match fs::read_to_string(signature_path(binary)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Signature::Unsigned,
        Err(e) => return Signature::Invalid(format!("the signature file cannot be read: {e}")),
    };
    let digest = match digest_of(binary) {
        Ok(digest) => digest,
        Err(e) => return Signature::Invalid(e),
    };
    match verify_text(
        &text,
        &digest,
        trusted,
        (SIGNATURE_HEADER, "binary", "file"),
    ) {
        Ok(key_id) => Signature::Verified { key_id },
        Err(why) => Signature::Invalid(why),
    }
}

/// Checks a signature's text against bytes that are not a file on disk: the
/// counterpart of [`SecretKey::sign_bytes`]. `what` is the noun for the thing
/// the bytes stand for in a refusal, as in "module". It never answers
/// [`Signature::Unsigned`]: whether there is a signature at all is the
/// caller's to know.
pub fn verify_bytes(signature: &str, bytes: &[u8], trusted: &[PublicKey], what: &str) -> Signature {
    let digest = to_hex(&Sha256::digest(bytes));
    match verify_text(signature, &digest, trusted, (STATEMENT_HEADER, what, what)) {
        Ok(key_id) => Signature::Verified { key_id },
        Err(why) => Signature::Invalid(why),
    }
}

/// Checks a signature file's text against a binary's digest, and returns the
/// id of the trusted key that signed it.
fn verify_text(
    text: &str,
    digest: &str,
    trusted: &[PublicKey],
    (header, what, signed_as): (&str, &str, &str),
) -> Result<String, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some(header) {
        return Err(format!("the signature file does not start with {header}"));
    }
    let (mut key_id, mut recorded, mut signature) = (None, None, None);
    for line in lines {
        match line.split_once(' ') {
            Some(("key", value)) => key_id = Some(value.trim()),
            Some(("sha256", value)) => recorded = Some(value.trim()),
            Some(("sig", value)) => signature = Some(value.trim()),
            // A later version may say more; what is here is still checked.
            _ => {}
        }
    }
    let key_id = key_id.ok_or("the signature file names no key")?;
    let signature: [u8; 64] = signature
        .and_then(from_hex)
        .and_then(|b| b.try_into().ok())
        .ok_or("the signature file holds no signature of the right length")?;
    let Some(key) = trusted.iter().find(|k| k.id() == key_id) else {
        return Err(format!(
            "it was signed by key {key_id}, which is not a trusted key"
        ));
    };
    // Said first because it is the likelier and the more useful thing to
    // know. It decides nothing: the signature is checked against the digest
    // of the file as it is, not against what the signature file claims.
    if recorded.is_some_and(|r| !r.eq_ignore_ascii_case(digest)) {
        return Err(format!(
            "the {what} is not the {signed_as} that was signed: its contents have changed"
        ));
    }
    let signature = ed25519_dalek::Signature::from_bytes(&signature);
    key.0
        .verify_strict(message(header, digest).as_bytes(), &signature)
        .map_err(|_| format!("the signature is not one key {key_id} made for this {what}"))?;
    Ok(key_id.to_owned())
}

/// The SHA-256 of a file, in hexadecimal.
fn digest_of(path: &Path) -> Result<String, String> {
    let fail = |e: std::io::Error| format!("reading {}: {e}", path.display());
    let mut file = fs::File::open(path).map_err(fail)?;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match file.read(&mut chunk).map_err(fail)? {
            0 => break,
            n => hasher.update(&chunk[..n]),
        }
    }
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meridian-signature-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A binary, signed by a new key, and that key.
    fn signed(dir: &Path) -> (PathBuf, SecretKey) {
        let binary = dir.join("meridian-target-x.exe");
        fs::write(&binary, b"the plugin's bytes").unwrap();
        let key = SecretKey::generate().unwrap();
        fs::write(signature_path(&binary), key.sign_file(&binary).unwrap()).unwrap();
        (binary, key)
    }

    #[test]
    fn bytes_are_signed_and_checked_like_a_file_holding_them() {
        let dir = scratch("bytes");
        let key = SecretKey::generate().unwrap();
        let trusted = [key.public()];
        let text = key.sign_bytes(b"a statement");
        assert_eq!(
            verify_bytes(&text, b"a statement", &trusted, "module"),
            Signature::Verified {
                key_id: key.public().id()
            }
        );
        assert_eq!(
            verify_bytes(&text, b"another statement", &trusted, "module"),
            Signature::Invalid(
                "the module is not the module that was signed: its contents have changed".into()
            )
        );
        assert_eq!(
            verify_bytes(&text, b"a statement", &[], "module"),
            Signature::Invalid(format!(
                "it was signed by key {}, which is not a trusted key",
                key.public().id()
            ))
        );
        // A statement's signature is not a file's, even for a file holding
        // exactly the statement: each is refused where the other is wanted.
        let file = dir.join("statement");
        fs::write(&file, b"a statement").unwrap();
        fs::write(signature_path(&file), &text).unwrap();
        assert_eq!(
            verify_file(&file, &trusted),
            Signature::Invalid(
                "the signature file does not start with meridian-plugin-signature-v1".into()
            )
        );
        let as_file = key.sign_file(&file).unwrap();
        assert_ne!(as_file, text);
        assert_eq!(
            verify_bytes(&as_file, b"a statement", &trusted, "module"),
            Signature::Invalid(
                "the signature file does not start with meridian-statement-signature-v1".into()
            )
        );
        // Nor does renaming the first line help: the name is in what was
        // signed.
        let renamed = text.replace(
            "meridian-statement-signature-v1",
            "meridian-plugin-signature-v1",
        );
        fs::write(signature_path(&file), &renamed).unwrap();
        assert_eq!(
            verify_file(&file, &trusted),
            Signature::Invalid(format!(
                "the signature is not one key {} made for this binary",
                key.public().id()
            ))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_signature_file_sits_beside_the_binary_under_its_whole_name() {
        assert_eq!(
            signature_path(Path::new("bin/meridian-target-x.exe")),
            Path::new("bin/meridian-target-x.exe.sig")
        );
        assert_eq!(
            signature_path(Path::new("meridian-target-x")),
            Path::new("meridian-target-x.sig")
        );
    }

    #[test]
    fn a_signed_binary_verifies_against_its_key_and_no_other() {
        let dir = scratch("verify");
        let (binary, key) = signed(&dir);
        let id = key.public().id();
        assert_eq!(id.len(), 16);
        assert_eq!(
            verify_file(&binary, &[key.public()]),
            Signature::Verified { key_id: id.clone() }
        );

        let stranger = SecretKey::generate().unwrap().public();
        assert_eq!(
            verify_file(&binary, &[stranger]),
            Signature::Invalid(format!(
                "it was signed by key {id}, which is not a trusted key"
            ))
        );
        assert_eq!(
            verify_file(&binary, &[]),
            Signature::Invalid(format!(
                "it was signed by key {id}, which is not a trusted key"
            ))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_binary_no_longer_verifies() {
        let dir = scratch("changed");
        let (binary, key) = signed(&dir);
        fs::write(&binary, b"the plugin's bytes, and one more").unwrap();
        assert_eq!(
            verify_file(&binary, &[key.public()]),
            Signature::Invalid(
                "the binary is not the file that was signed: its contents have changed".into()
            )
        );

        // Rewriting the recorded digest to match does not help: the signature
        // is over the digest, and it was made for the other one.
        let changed = digest_of(&binary).unwrap();
        let text: String = fs::read_to_string(signature_path(&binary))
            .unwrap()
            .lines()
            .map(|line| match line.starts_with("sha256 ") {
                true => format!("sha256 {changed}\n"),
                false => format!("{line}\n"),
            })
            .collect();
        fs::write(signature_path(&binary), text).unwrap();
        let Signature::Invalid(why) = verify_file(&binary, &[key.public()]) else {
            panic!("a forged digest verified");
        };
        assert!(why.starts_with("the signature is not one key"), "{why}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_signature_made_by_another_key_under_a_trusted_keys_id_is_refused() {
        let dir = scratch("forged");
        let (binary, key) = signed(&dir);
        let forger = SecretKey::generate().unwrap();
        let forged = forger
            .sign_file(&binary)
            .unwrap()
            .replace(&forger.public().id(), &key.public().id());
        fs::write(signature_path(&binary), forged).unwrap();
        let Signature::Invalid(why) = verify_file(&binary, &[key.public()]) else {
            panic!("a forged signature verified");
        };
        assert!(why.starts_with("the signature is not one key"), "{why}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_signature_file_says_what_is_wrong() {
        let key = SecretKey::generate().unwrap().public();
        for (text, want) in [
            ("", "does not start with meridian-plugin-signature-v1"),
            ("something else\n", "does not start with"),
            ("meridian-plugin-signature-v1\nsig 00\n", "names no key"),
            (
                "meridian-plugin-signature-v1\nkey abc\nsig zz\n",
                "no signature of the right length",
            ),
            (
                "meridian-plugin-signature-v1\nkey abc\n",
                "no signature of the right length",
            ),
        ] {
            let err = verify_text(
                text,
                "00",
                std::slice::from_ref(&key),
                (SIGNATURE_HEADER, "binary", "file"),
            )
            .unwrap_err();
            assert!(err.contains(want), "{text:?}: {err}");
        }
    }

    #[test]
    fn the_policy_decides_what_a_failed_check_means() {
        let dir = scratch("policy");
        let (binary, key) = signed(&dir);
        let unsigned = dir.join("meridian-target-y");
        fs::write(&unsigned, b"unsigned").unwrap();
        let with = |policy, trusted: Vec<PublicKey>| Verification { policy, trusted };

        // Off looks at nothing, even a key that would not pass.
        assert_eq!(
            with(Policy::Off, vec![]).check(&binary),
            Ok(Signature::NotChecked)
        );
        // Warn reports and lets the plugin start.
        assert_eq!(
            with(Policy::Warn, vec![key.public()]).check(&unsigned),
            Ok(Signature::Unsigned)
        );
        assert!(matches!(
            with(Policy::Warn, vec![]).check(&binary),
            Ok(Signature::Invalid(_))
        ));
        // Require starts only what a trusted key signed.
        assert!(matches!(
            with(Policy::Require, vec![key.public()]).check(&binary),
            Ok(Signature::Verified { .. })
        ));
        let err = with(Policy::Require, vec![key.public()])
            .check(&unsigned)
            .unwrap_err();
        assert!(err.starts_with("it is not signed: there is no "), "{err}");
        let err = with(Policy::Require, vec![]).check(&binary).unwrap_err();
        assert!(err.contains("which is not a trusted key"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_round_trip_through_their_text_and_are_not_confused() {
        let key = SecretKey::generate().unwrap();
        let public = key.public();
        assert_eq!(PublicKey::parse(&public.to_text()).unwrap(), public);
        assert_eq!(
            PublicKey::parse(&format!("# ours\n{} release key\n", public.to_text())).unwrap(),
            public
        );
        // The bare digits are a key too, which is how a build embeds one.
        let bare = public.to_text();
        let bare = bare.split(' ').nth(1).unwrap();
        assert_eq!(PublicKey::parse(bare).unwrap(), public);
        assert_eq!(SecretKey::parse(&key.to_text()).unwrap().public(), public);

        assert_eq!(
            PublicKey::parse(&key.to_text()).unwrap_err(),
            "this is a secret key; a public key is needed here"
        );
        assert!(SecretKey::parse(&public.to_text()).is_err());
        assert!(PublicKey::parse("").is_err());
        assert!(PublicKey::parse("meridian-public-key-v1 abcd").is_err());
        // A secret key does not print itself.
        assert!(!format!("{key:?}").contains(key.to_text().split(' ').nth(1).unwrap()));

        for (text, want) in [
            ("off", Policy::Off),
            ("Warn", Policy::Warn),
            (" require ", Policy::Require),
        ] {
            assert_eq!(text.parse::<Policy>().unwrap(), want);
        }
        assert!("strict".parse::<Policy>().is_err());
    }
}
