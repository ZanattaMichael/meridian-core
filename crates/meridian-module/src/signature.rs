//! Signed modules: who published this content.
//!
//! The lockfile's hash proves a module has not changed since it was pinned.
//! A signature says who vouches for it. A signed module holds a file,
//! `meridian.sig`, with an Ed25519 signature made by `meridian module sign`
//! over a short statement: the module's name, its version and its content
//! hash. The file is not part of the content hash, so signing a module does
//! not change what it is pinned as, and the same signature travels with the
//! module through git, a registry or a mirror.
//!
//! The mechanism and the keys are the ones plugins are signed with
//! ([`meridian_signing`]): one set of trusted keys covers both.

use std::fs;
use std::path::Path;

use meridian_signing::{verify_bytes, Policy, SecretKey, Signature, Verification};

use crate::integrity::hash_dir;
use crate::manifest::Manifest;
use crate::Error;

/// Signatures off: nothing is checked. What a caller that has no trust
/// settings passes as [`crate::Project::signatures`].
pub static NO_SIGNATURES: Verification = Verification {
    policy: Policy::Off,
    trusted: Vec::new(),
};

/// The file in a module's top directory that holds its signature.
pub const SIGNATURE_FILE: &str = "meridian.sig";

/// What is signed. Naming the module and its version means a signature for
/// one module, or one version, cannot be presented with another's content
/// even when the files are the same.
fn statement(name: &str, version: &str, integrity: &str) -> Vec<u8> {
    format!("meridian-module-statement-v1\nname {name}\nversion {version}\nintegrity {integrity}\n")
        .into_bytes()
}

/// What signing a module wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    pub name: String,
    /// Empty when the module declares no version.
    pub version: String,
    pub integrity: String,
    /// The id of the key that signed.
    pub key_id: String,
}

/// Signs the module in `dir` as it is now, and writes `meridian.sig` in it.
/// Any change to the module's files afterwards makes the signature stale, so
/// this is the last step before a version is tagged or pushed.
pub fn sign(dir: &Path, key: &SecretKey) -> Result<Signed, Error> {
    let manifest = Manifest::load(dir)?;
    let version = manifest
        .version
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let integrity = hash_dir(dir)?;
    let text = key.sign_bytes(&statement(&manifest.name, &version, &integrity));
    let path = dir.join(SIGNATURE_FILE);
    fs::write(&path, text).map_err(|e| Error::new(format!("writing {}: {e}", path.display())))?;
    Ok(Signed {
        name: manifest.name,
        version,
        integrity,
        key_id: key.public().id(),
    })
}

/// What the signature in `dir` says about the module pinned as `name`,
/// `version` and `integrity`, whatever the policy.
pub(crate) fn found(
    trusted: &Verification,
    dir: &Path,
    name: &str,
    version: &str,
    integrity: &str,
) -> Signature {
    let text = match fs::read_to_string(dir.join(SIGNATURE_FILE)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Signature::Unsigned,
        Err(e) => return Signature::Invalid(format!("{SIGNATURE_FILE} cannot be read: {e}")),
    };
    verify_bytes(
        &text,
        &statement(name, version, integrity),
        &trusted.trusted,
        "module",
    )
}

/// Applies the policy to one fetched module. `Ok` carries what was found,
/// which under [`Policy::Warn`] may be a signature that did not pass. `Err`
/// is a refusal and says why; `label` names the module in it.
pub(crate) fn check(
    trusted: &Verification,
    dir: &Path,
    label: &str,
    (name, version, integrity): (&str, &str, &str),
) -> Result<Signature, String> {
    if trusted.policy == Policy::Off {
        return Ok(Signature::NotChecked);
    }
    let found = found(trusted, dir, name, version, integrity);
    match (&found, trusted.policy) {
        (Signature::Unsigned, Policy::Require) => Err(format!(
            "{label} is not signed: it holds no {SIGNATURE_FILE}. Module signatures are \
             required; relax the check with --module-signatures warn, or ask its publisher to \
             sign it with `meridian module sign`"
        )),
        (Signature::Invalid(why), Policy::Require) => Err(format!(
            "{label}: its signature does not pass: {why}. Module signatures are required, so \
             it was not used; trust its signer's key with --trusted-key"
        )),
        _ => Ok(found),
    }
}

/// What to tell the operator about a module that was used although its
/// signature did not pass, which only [`Policy::Warn`] allows.
pub(crate) fn warning(label: &str, found: &Signature) -> Option<String> {
    match found {
        Signature::Unsigned => Some(format!("{label} is not signed")),
        Signature::Invalid(why) => Some(format!("{label}: its signature does not pass: {why}")),
        Signature::Verified { .. } | Signature::NotChecked => None,
    }
}
