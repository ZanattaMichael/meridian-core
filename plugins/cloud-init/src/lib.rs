//! Compiles a Meridian resource tree into cloud-init user-data, the
//! `#cloud-config` document a machine reads the first time it boots, and
//! checks and follows that first boot with `cloud-init` itself.
//!
//! cloud-init is not a tool that is pointed at a running machine. A machine
//! is created with user-data and applies it once, on its own, before anyone
//! can reach it. That shapes everything here:
//!
//! - The order in which things happen is cloud-init's and not the document's:
//!   files are written, then groups and users are created, then packages are
//!   installed, then deferred files are written, then commands run. The sort
//!   stage places every resource in that order and refuses a `dependsOn` it
//!   cannot honour.
//! - Nothing is ever "changed" on a second run, so there is no native notify.
//!   On a first boot every resource changes, so a notified action is emulated
//!   by running it once, last.
//! - There is no moment to evaluate a `runtimeWhen`, so one is refused.
//! - What cloud-init has no module for (a service, a directory, removing a
//!   file) becomes a first-boot command, and the compile says so.
//!
//! The library depends on `meridian-sdk` and the YAML writer and nothing else
//! of Meridian's. The four compile stages run in the order the design plan
//! mandates: transform, sort, validate, emit.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod runner;
mod shell;
mod sort;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value, Warning,
};

pub use emit::{META_DATA_PATH, USER_DATA_PATH};
pub use runner::{
    CloudInitRunner, Wait, PROGRAM_VARIABLE, SSH_ARGS_VARIABLE, SSH_VARIABLE, VALIDATE_VARIABLE,
    WAIT_VARIABLE,
};
pub use transform::supported_resource_types;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "cloud-init";

/// The size above which user-data is worth a warning: Amazon EC2 accepts
/// 16 KB of it, which is the smallest limit among the large clouds.
pub const SMALLEST_CLOUD_LIMIT: usize = 16 * 1024;

/// Something in the native tree, with the resource it came from, so a failure
/// in a later stage can still name what the author wrote.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed<T> {
    pub id: String,
    pub pos: Position,
    pub item: T,
}

impl<T> Placed<T> {
    fn refuse(&self, rule: &str, msg: String) -> CompileError {
        CompileError {
            target: NAME.to_owned(),
            resource: self.id.clone(),
            rule: rule.to_owned(),
            msg,
            pos: self.pos.clone(),
        }
    }
}

/// One entry under `groups:`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Group {
    pub name: String,
    pub members: Vec<String>,
}

/// One entry under `users:`, as cloud-init's own keys.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct User {
    pub name: String,
    /// Every key but `name`, by cloud-init's name for it.
    pub fields: BTreeMap<String, Value>,
}

/// One entry under `packages:`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Package {
    pub name: String,
    /// Empty for whichever version the distribution offers.
    pub version: String,
}

/// One entry under `write_files:`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WriteFile {
    pub path: String,
    pub content: String,
    pub owner: String,
    pub group: String,
    pub permissions: String,
    pub encoding: String,
    pub append: bool,
    /// Written after users exist and packages are installed, instead of
    /// first. Decided by the sort stage, never by the author.
    pub defer: bool,
}

/// One entry under `runcmd:`: a line of shell.
///
/// cloud-init accepts a list of words as well, and turns it into exactly such
/// a line, each word quoted, before it runs anything: every entry ends up in
/// one script. Writing the line here keeps the output readable and loses
/// nothing, because a command that asked for no shell has its words quoted
/// the same way.
pub(crate) type Command = String;

/// What one resource became.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    File(WriteFile),
    Group(Group),
    User(User),
    Package(Package),
    /// cloud-init has no module for the resource, or the resource is a
    /// command: it runs at the end of the first boot.
    Commands(Vec<Command>),
}

/// A whole `#cloud-config` document, each list in the order it is written.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CloudConfig {
    /// The source document's name.
    pub document: String,
    pub host: String,
    pub files: Vec<Placed<WriteFile>>,
    pub groups: Vec<Placed<Group>>,
    pub users: Vec<Placed<User>>,
    pub packages: Vec<Placed<Package>>,
    pub commands: Vec<Command>,
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

pub(crate) fn warning(resource: &str, msg: String) -> Warning {
    Warning {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        msg,
    }
}

/// The cloud-init emitter.
pub struct CloudInit;

impl Emitter for CloudInit {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        supported_resource_types()
    }

    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        transform::resource_schemas()
    }

    /// `synthetic_notify`: cloud-init applies user-data once and has no idea
    /// of a later change. On that one run every resource does change, so a
    /// notified action is run once, after everything else, and the compile
    /// warns that this is what a notify means here.
    ///
    /// `runtime_condition` is false. A `runtimeWhen` is written in the
    /// language of the tool that evaluates it, and cloud-init evaluates
    /// nothing per resource.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: false,
            synthetic_notify: true,
            runtime_condition: false,
        }
    }

    fn category(&self) -> Category {
        Category::ConfigurationManagement
    }

    /// Runs the four compile stages in order.
    ///
    /// The resolved hierarchy data is not consumed: Meridian bakes resolved
    /// values into the tree as literals before a plugin sees it.
    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        // A resource here has nothing to read at apply time, so a reference
        // is settled now, from what the document declares, or refused.
        let graph = &meridian_sdk::expr::resolve_declared(graph, NAME)?;
        let transformed = transform::transform(graph)?;
        let mut warnings = transformed.warnings;
        let config = sort::arrange(graph, transformed.units, transformed.handlers)?;
        validate::validate(&config)?;
        let files = emit::serialise(&config);

        let size = files[USER_DATA_PATH].len();
        if size > SMALLEST_CLOUD_LIMIT {
            warnings.push(warning(
                "",
                format!(
                    "the user-data is {size} bytes; Amazon EC2 accepts {SMALLEST_CLOUD_LIMIT} \
                     and other clouds have limits of their own, so compress it or move large \
                     files out of the document if the machine is created there"
                ),
            ));
        }
        Ok(Emitted {
            artifact: Artifact { files },
            warnings,
        })
    }
}
