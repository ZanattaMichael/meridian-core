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
pub use lock::{Lock, Locked, LOCK_FILE, LOCK_VERSION};
pub use manifest::{valid_name, Manifest, Requirement, Source, MANIFEST_FILE};
pub use oci::{push, Pushed, ARTIFACT_TYPE, LAYER_MEDIA_TYPE};
pub use pack::{pack, unpack, MAX_UNPACKED_BYTES};
pub use project::{
    install, verify, Change, Check, Mode, Modules, Project, Report, MODULE_REFERENCE_PREFIX,
};
pub use signature::{sign, Signed, NO_SIGNATURES, SIGNATURE_FILE};

/// The signing mechanism modules share with plugins, re-exported so a caller
/// can build the [`Project::signatures`] it passes in.
pub use meridian_signing::{Policy, PublicKey, SecretKey, Signature, Verification};

/// Why something to do with modules failed. The text is the whole message:
/// it names the file, the module and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
