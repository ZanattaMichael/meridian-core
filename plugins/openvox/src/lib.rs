//! Compiles a Meridian resource tree into a manifest for OpenVox and applies
//! it.
//!
//! OpenVox is the community-maintained fork of Puppet. It reads the same
//! language, ships the same `puppet` command and applies a catalog the same
//! way, so this plugin does not carry a second compiler. The four compile
//! stages are the Puppet plugin's own, used as a library: what `openvox`
//! compiles is byte for byte what `puppet` compiles, and a test holds the two
//! to that. A mapping fixed for one is fixed for both.
//!
//! What is this plugin's own is the half that touches a machine:
//!
//! - It finds the tool where OpenVox's packages put it, and is configured
//!   through its own variables, so one machine can drive Puppet and OpenVox
//!   side by side.
//! - It checks that the `puppet` it is about to run really is OpenVox. The two
//!   products install a command of the same name, and a document that says
//!   `target: openvox` is a statement about which one the operator means.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod runner;

use meridian_sdk::{Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_puppet::Puppet;

pub use meridian_target_puppet::{Transport, MANIFEST_PATH, SITE_PATH};
pub use runner::{
    find_program, OpenVoxRunner, Verify, ARGS_VARIABLE, OPENBOLT_VARIABLE, OPENVOX_VARIABLE,
    PACKAGE_BIN, TRANSPORT_VARIABLE, VERIFY_VARIABLE,
};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "openvox";

/// The OpenVox emitter.
pub struct OpenVox;

impl Emitter for OpenVox {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        Puppet.supported_resource_types()
    }

    /// The types are the language's, so they are Puppet's.
    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        Puppet.resource_schemas()
    }

    /// The capabilities are the language's, so they are Puppet's: a native
    /// `notify`, and no apply-time conditional.
    fn capabilities(&self) -> Capabilities {
        Puppet.capabilities()
    }

    fn category(&self) -> Category {
        Category::ConfigurationManagement
    }

    fn emit(&self, graph: &ResourceGraph, data: &Data) -> Result<Emitted, CompileError> {
        // A refusal names the target the author chose, not the library that
        // did the work.
        Puppet.emit(graph, data).map_err(|refusal| CompileError {
            target: NAME.to_owned(),
            ..refusal
        })
    }
}
