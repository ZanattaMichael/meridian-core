//! Compiles a Meridian document into Podman Quadlet units and runs them with
//! systemd.
//!
//! # Why Quadlet
//!
//! Podman has no daemon and no `apply`. What it has is Quadlet: a unit file
//! per container, pod, volume, network or image, which systemd turns into a
//! service that runs `podman`. That is Podman's own declarative form, it
//! survives a reboot without anything else running, and it gives a plan
//! something exact to compare: the unit files on the host with the unit
//! files a document compiles to.
//!
//! # What a document looks like to this target
//!
//! | `type` | Becomes | Runs as |
//! |---|---|---|
//! | `container` | `<id>.container` | `<id>.service` |
//! | `pod` | `<id>.pod` | `<id>-pod.service` |
//! | `kube` | `<id>.kube` | `<id>.service` |
//! | `volume` | `<id>.volume` | `<id>-volume.service` |
//! | `network` | `<id>.network` | `<id>-network.service` |
//! | `image` | `<id>.image` | `<id>-image.service` |
//! | `build` | `<id>.build` | `<id>-build.service` |
//!
//! A resource's `params` are Quadlet's own keys, spelled as Podman documents
//! them (`Image`, `PublishPort`, `Volume`). This target does not hide them
//! behind a smaller vocabulary. Four lower-case params are the other sections
//! of the unit file: `unit`, `service`, `install` and `quadlet`.
//!
//! What Meridian adds is the wiring. `${data}` in a param refers to another
//! resource of the document and becomes its unit file name (`data.volume`),
//! which is how Quadlet itself links units. `${data.name}` is the name Podman
//! knows it by and `${data.service}` its systemd service. `dependsOn` becomes
//! `Requires=` and `After=` unless such a reference already implies the
//! order, and `notifies` with the action `restart` becomes `PartOf=`.
//!
//! Either kind of document compiles. A `ResourceSet` runs its containers on
//! the host it names; an `Infrastructure` document runs them on the machine
//! running Meridian and may declare outputs.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod keys;
mod runner;
mod script;
mod sort;
mod transform;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph,
};

/// The name of the Podman secret a document's secret becomes. The runner
/// creates it on the host from the value, over the script's own standard
/// input, before the units are installed; the unit names it and never
/// holds the value.
pub fn podman_secret(name: &str) -> String {
    format!("meridian_secret_{name}")
}

/// What a secret in a container's `Environment` is written as: Quadlet's
/// `Secret=` key, which has Podman put the secret's value in the variable
/// when the container starts.
pub fn secret_reference(name: &str, variable: &str) -> String {
    format!("{},type=env,target={variable}", podman_secret(name))
}

pub use emit::META_PATH;
pub use runner::{PodmanRunner, Scope};

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "podman";

/// The kind of Quadlet unit a resource becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Container,
    Pod,
    Kube,
    Volume,
    Network,
    Image,
    Build,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Build,
        Kind::Container,
        Kind::Image,
        Kind::Kube,
        Kind::Network,
        Kind::Pod,
        Kind::Volume,
    ];

    /// The resource type, which is also the unit file's extension.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Container => "container",
            Kind::Pod => "pod",
            Kind::Kube => "kube",
            Kind::Volume => "volume",
            Kind::Network => "network",
            Kind::Image => "image",
            Kind::Build => "build",
        }
    }

    pub fn from_type(t: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == t)
    }

    /// The unit file's own section: `[Container]`, `[Pod]` and so on.
    pub fn section(self) -> &'static str {
        match self {
            Kind::Container => "Container",
            Kind::Pod => "Pod",
            Kind::Kube => "Kube",
            Kind::Volume => "Volume",
            Kind::Network => "Network",
            Kind::Image => "Image",
            Kind::Build => "Build",
        }
    }

    /// The service Quadlet generates for a unit named `stem`, unless the unit
    /// names its own with `ServiceName`.
    pub fn service(self, stem: &str) -> String {
        match self {
            Kind::Container | Kind::Kube => format!("{stem}.service"),
            other => format!("{stem}-{}.service", other.as_str()),
        }
    }

    /// Whether the unit is something that runs, as opposed to something a
    /// running unit needs. Only these are started at boot by default.
    pub fn is_workload(self) -> bool {
        matches!(self, Kind::Container | Kind::Pod | Kind::Kube)
    }
}

/// One Quadlet unit file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Unit {
    /// The resource's id, which is the unit file's stem.
    pub id: String,
    pub kind: Kind,
    /// The systemd service Quadlet generates from the file.
    pub service: String,
    /// The name Podman knows the thing by, when it has one.
    pub runtime_name: Option<String>,
    /// The unit's own section: key to its values, one line each.
    pub body: BTreeMap<String, Vec<String>>,
    /// `[Unit]`, `[Service]`, `[Install]` and `[Quadlet]`, by section name.
    pub sections: BTreeMap<&'static str, BTreeMap<String, Vec<String>>>,
    /// The resources this unit names by unit file. Quadlet orders a unit
    /// after every unit file it names.
    pub linked: BTreeSet<String>,
    pub pos: Position,
}

impl Unit {
    /// The unit file's name.
    pub fn file(&self) -> String {
        format!("{}.{}", self.id, self.kind.as_str())
    }

    pub fn section_mut(&mut self, name: &'static str) -> &mut BTreeMap<String, Vec<String>> {
        self.sections.entry(name).or_default()
    }
}

/// One piece of an output's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputPart {
    /// Known when the document is compiled.
    Text(String),
    /// Read from Podman once the thing exists: `podman <noun> inspect`.
    Inspect {
        noun: &'static str,
        name: String,
        path: String,
    },
}

/// A value the document promises to later stages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputValue {
    pub name: String,
    pub parts: Vec<OutputPart>,
    pub pos: Position,
}

/// Everything a document compiles to.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Units {
    /// The source document's name, for the header of each file.
    pub document: String,
    pub units: Vec<Unit>,
    pub outputs: Vec<OutputValue>,
}

impl Units {
    pub fn unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    pub fn index(&self, id: &str) -> Option<usize> {
        self.units.iter().position(|u| u.id == id)
    }
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// A refusal that is not about a resource still in hand.
pub(crate) fn refuse_at(resource: &str, pos: &Position, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        resource: resource.to_owned(),
        rule: rule.to_owned(),
        msg,
        pos: pos.clone(),
    }
}

/// The Podman emitter.
pub struct Podman;

impl Emitter for Podman {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        Kind::ALL.iter().map(|k| k.as_str().to_owned()).collect()
    }

    /// systemd restarts a unit when a unit it is `PartOf=` restarts, and the
    /// runner restarts a unit whose file changed. Together that is a real
    /// change-triggered restart, so notify is declared native. `restart` is
    /// the only action it can carry.
    ///
    /// A unit's `Condition*=` settings are checked when it starts, but they
    /// are a fixed list of tests, not an expression, so `runtimeWhen` is
    /// refused.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: false,
            secrets: true,
            credentials: false,
        }
    }

    fn category(&self) -> Category {
        Category::Containerization
    }

    fn emit(&self, graph: &ResourceGraph, _data: &Data) -> Result<Emitted, CompileError> {
        let mut units = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut units)?;
        let warnings = validate::validate(&units)?;
        Ok(Emitted {
            artifact: Artifact {
                files: emit::serialise(&units),
            },
            warnings,
        })
    }
}
