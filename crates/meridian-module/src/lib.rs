//! Meridian modules: packaged, versioned configuration that can be fetched
//! from somewhere else and trusted to be what was published.
//!
//! A module is a directory with a `meridian.yaml` manifest. A project is a
//! module nobody depends on. This crate reads manifests, resolves the
//! versions a project's requirements allow, fetches modules from a path, a
//! git repository or an OCI registry, pins them in `meridian.lock` by a hash
//! of their content, and checks that hash every time a module is used.
//!
//! It does not interpret what a module holds. The guarantee is only that
//! every file of a pinned module is on disk, unmodified, at a known path, and
//! that `@<module>:<path>` names exactly one of them. The language decides
//! what a document, a composite or a data layer found there means.
//!
//! - [`install`] resolves, fetches and writes the lockfile.
//! - [`Modules::load`] opens the pinned modules for use, checking each.
//! - [`verify`] reports on every pinned module without stopping at the first
//!   that is wrong.
//! - [`pack`] writes the archive a registry stores, and [`push`] publishes it.
//! - [`sign`] signs a module, and every function above checks signatures
//!   when the [`Project`] it is given says to.

use std::fmt;

mod git;
mod integrity;
mod lock;
mod manifest;
mod oci;
mod pack;
mod project;
mod signature;

pub use integrity::hash_dir;
pub use lock::{valid_digest, Lock, Locked, LockedBundle, LockedSchemas, LOCK_FILE, LOCK_VERSION};
pub use manifest::{
    valid_name, Manifest, Requirement, SchemaRegistry, SchemaSettings, Source, MANIFEST_FILE,
};
pub use oci::{
    list_tags, pull_artifact, push, push_artifact, valid_tag, Artifact, ArtifactKind, Pushed,
    ARTIFACT_TYPE, LAYER_MEDIA_TYPE, MODULE_ARTIFACT, SCHEMA_BUNDLE_ARTIFACT,
    SCHEMA_INDEX_ARTIFACT,
};
pub use pack::{pack, unpack, MAX_UNPACKED_BYTES};
pub use project::{
    install, resolve_login, verify, Change, Check, Mode, Modules, Project, Report,
    MODULE_REFERENCE_PREFIX,
};

/// A resolved credential and what a project declares one as, re-exported for
/// callers of [`push`] and readers of [`Manifest::credentials`].
pub use meridian_secret::{Credential, CredentialDeclaration};
pub use signature::{sign, Signed, NO_SIGNATURES, SIGNATURE_FILE};

/// The signing mechanism modules share with plugins, re-exported so a caller
/// can build the [`Project::signatures`] it passes in.
pub use meridian_signing::{Policy, PublicKey, SecretKey, Signature, Verification};

/// Why something to do with modules failed. The text is the whole message:
/// it names the file, the module and what to do about it. `kind` says which
/// step of handling modules it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: Kind,
    msg: String,
}

/// Which step failed, each with a fixed code listed in
/// `docs/reference/errors.md`. A code is never reused for another meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `meridian.yaml` cannot be read or says something it cannot.
    Manifest,
    /// `meridian.lock` cannot be read, or does not match the manifest.
    Lock,
    /// A module's files are not the files its hash or its lock entry names.
    Integrity,
    /// A registry could not be reached, refused the request, or answered
    /// with something that is not a module.
    Registry,
    /// A git source could not be fetched or checked out.
    Git,
    /// A module archive could not be packed or unpacked.
    Pack,
    /// A module's signature is missing, or does not pass.
    Signature,
    /// The modules' versions cannot be agreed on: a constraint has no
    /// version, two demands conflict, or a module needs another core.
    Resolution,
    /// A resolved module could not be fetched and written into place, or an
    /// installed module is not the one the lock pins.
    Install,
    /// The credential a registry needs is not declared, or cannot be read.
    Credential,
}

impl Kind {
    /// Every kind, in the order the reference lists them.
    pub const ALL: [Kind; 10] = [
        Kind::Manifest,
        Kind::Lock,
        Kind::Integrity,
        Kind::Registry,
        Kind::Git,
        Kind::Pack,
        Kind::Signature,
        Kind::Resolution,
        Kind::Install,
        Kind::Credential,
    ];

    /// The fixed code.
    pub fn code(self) -> &'static str {
        match self {
            Kind::Manifest => "module-manifest",
            Kind::Lock => "module-lock",
            Kind::Integrity => "module-integrity",
            Kind::Registry => "module-registry",
            Kind::Git => "module-git",
            Kind::Pack => "module-pack",
            Kind::Signature => "module-signature",
            Kind::Resolution => "module-resolution",
            Kind::Install => "module-install",
            Kind::Credential => "module-credential",
        }
    }
}

impl Error {
    pub(crate) fn new(kind: Kind, msg: impl Into<String>) -> Error {
        Error {
            kind,
            msg: msg.into(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl From<Error> for String {
    fn from(error: Error) -> String {
        error.msg
    }
}
