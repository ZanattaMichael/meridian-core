//! Compiles a Meridian document into a Compose file and brings it up with
//! `docker compose`.
//!
//! # What a document looks like to this target
//!
//! A resource's `type` is one of the things a Compose file declares
//! (`service`, `network`, `volume`, `config`, `secret`) and its `params` are
//! that element's fields, exactly as the Compose specification documents
//! them. The resource's id is the element's name in the file.
//!
//! One resource may have the type `project`. It is not part of the Compose
//! file: it says what the project is called, which Docker daemon it runs on
//! and how it is brought up (`name`, `host`, `context`, `directory`,
//! `profiles`, `wait`, `wait_timeout`). A document that names a target host
//! runs on that host's daemon over SSH.
//!
//! # What is checked before Docker sees it
//!
//! An attribute Compose does not know is refused, with the one that was
//! probably meant. So is a network, volume, config or secret a service uses
//! and the document does not declare, a `depends_on` that names something
//! that is not a service, and services that wait for each other in a circle.
//!
//! # References
//!
//! Inside `params`, `${id}` is the name of another element of the document.
//! An output is read after the project is up: `${web.port.80}` is the host
//! port that port 80 of the service is published on, and any other path, such
//! as `${web.State.Status}`, is read from `docker inspect` of the service's
//! container.
//!
//! # Ordering and notification
//!
//! `dependsOn` between two services is Compose's own `depends_on`, waiting
//! for the other service to be healthy when it declares a healthcheck and to
//! have started when it does not. `notifies`
//! with the action `restart` from one service to another is `depends_on` with
//! `restart: true`, which Compose acts on itself. From anything else (a
//! config, a secret, a network, a volume) it is a checksum of the notifier in
//! a label on the service: when the notifier changes the label does, and
//! Compose recreates the container. That one is a synthetic notify and is
//! declared as one.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod check;
mod emit;
mod import;
mod runner;
mod sort;
mod transform;
mod validate;

use std::collections::BTreeMap;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};

pub use emit::{COMPOSE_PATH, PROJECT_PATH};
pub use import::DockerImporter;
pub use runner::DockerRunner;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "docker";

/// The top-level sections of a Compose file, in the order they are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Section {
    Service,
    Network,
    Volume,
    Config,
    Secret,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Service,
        Section::Network,
        Section::Volume,
        Section::Config,
        Section::Secret,
    ];

    /// The resource type that selects the section.
    pub fn type_name(self) -> &'static str {
        match self {
            Section::Service => "service",
            Section::Network => "network",
            Section::Volume => "volume",
            Section::Config => "config",
            Section::Secret => "secret",
        }
    }

    /// The section's key in the Compose file.
    pub fn key(self) -> &'static str {
        match self {
            Section::Service => "services",
            Section::Network => "networks",
            Section::Volume => "volumes",
            Section::Config => "configs",
            Section::Secret => "secrets",
        }
    }
}

/// One element of the Compose file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Element {
    /// The resource's id, which is also the element's name in the file.
    pub id: String,
    pub section: Section,
    pub body: BTreeMap<String, Value>,
    pub pos: Position,
}

/// What the project is called and where it runs, from the document's
/// `project` resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Project {
    pub name: String,
    /// A daemon address such as `ssh://deploy@web01`, or empty for the local
    /// one.
    pub host: String,
    /// A Docker context, or empty.
    pub context: String,
    /// The directory relative paths in the file are resolved against. Empty
    /// means the directory Meridian keeps the compiled file in.
    pub directory: String,
    /// The Compose profiles to bring up with the project.
    pub profiles: Vec<String>,
    /// Wait for every service to be running, or healthy, before an apply
    /// reports success.
    pub wait: bool,
    /// How long to wait, in seconds. Zero leaves it to Compose.
    pub wait_timeout: u64,
}

/// One piece of an output's value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OutputPart {
    Text(String),
    /// The host port a service's port is published on.
    Port {
        service: String,
        port: String,
    },
    /// A path into `docker inspect` of the service's container.
    Inspect {
        service: String,
        path: String,
    },
}

/// A whole Compose project.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Composition {
    pub project: Project,
    pub elements: Vec<Element>,
    pub outputs: Vec<(String, Vec<OutputPart>)>,
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

pub(crate) fn refuse_at(resource: &str, pos: &Position, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        rule: rule.to_owned(),
        msg,
        pos: pos.clone(),
    }
}

/// The Docker emitter.
pub struct Docker;

impl Emitter for Docker {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        let mut types: Vec<String> = Section::ALL
            .iter()
            .map(|s| s.type_name())
            .chain(["project"])
            .map(str::to_owned)
            .collect();
        // Sorted, as the SDK asks of every target.
        types.sort();
        types
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: true,
            synthetic_notify: true,
            runtime_condition: false,
        }
    }

    fn category(&self) -> Category {
        Category::Containerization
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut composition = transform::transform(graph)?;
        let mut warnings = sort::apply_ordering(graph, &mut composition)?;
        validate::validate(&composition)?;
        if composition.project.directory.is_empty() {
            for element in &composition.elements {
                warnings.extend(check::relative_paths(element));
            }
        }
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&composition),
            },
            warnings,
        })
    }
}
