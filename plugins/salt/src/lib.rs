//! Compiles a Meridian resource tree into a Salt state file and top file, and
//! runs it with `salt-call --local` or `salt-ssh`.
//!
//! The library depends on `meridian-sdk` and nothing else of Meridian's but
//! the shared YAML writer. The binary that serves it over the plugin protocol
//! is `main.rs`; the library is ordinary code, so the emitter can be tested
//! without a process boundary.
//!
//! The four compile stages run in the order the design plan mandates:
//! transform, sort, validate, emit. Each is a separate module, and emit does
//! no mapping or ordering work of its own.

// A refusal carries its position and resource by value, as the SDK's does.
#![allow(clippy::result_large_err)]

mod emit;
mod runner;
mod sort;
mod transform;
mod validate;

use meridian_sdk::{
    Artifact, Capabilities, Category, CompileError, Data, Emitted, Emitter, Position, Resource,
    ResourceGraph, Value,
};

pub use emit::{sls_path, STATES_DIR, TOP_PATH};
pub use runner::{
    SaltRunner, Transport, ARGS_VARIABLE, SALT_CALL_VARIABLE, SALT_SSH_VARIABLE, TRANSPORT_VARIABLE,
};
pub use transform::supported_resource_types;

/// The target name a document selects with `spec.target`.
pub const NAME: &str = "salt";

/// Another state, as a requisite names it: the state module and the state's
/// ID, which Salt writes `- pkg: nginx_pkg`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Reference {
    pub module: &'static str,
    pub id: String,
}

/// One native Salt state: an ID, the state function to call, its arguments
/// and its requisites. It is the native tree the transform stage produces and
/// the emit stage serialises.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct State {
    /// The ID declaration. Requisites name a state by it, so it must be
    /// unique within the file.
    pub id: String,
    /// The state module, for example `pkg`.
    pub module: &'static str,
    /// The function within it, for example `installed`.
    pub function: &'static str,
    /// The function's arguments in the order they are written: `name` first,
    /// then the rest by native name.
    pub args: Vec<(String, Value)>,
    /// States that must have succeeded first. Filled in by the sort stage
    /// from the dependency edges.
    pub require: Vec<Reference>,
    /// States whose changes make this one react: a watched service restarts
    /// or reloads.
    pub watch: Vec<Reference>,
    /// States whose changes are the only reason this one runs at all.
    pub onchanges: Vec<Reference>,
    /// A Jinja expression the minion evaluates when it renders the file. It
    /// is only ever populated from a resource's `runtimeWhen`: a compile-time
    /// `when` is pruned long before this stage and must never reach the
    /// output.
    pub when: String,
    /// Provenance, kept so a validation failure can name the resource the
    /// author wrote and not the state it became.
    pub resource_id: String,
    pub pos: Position,
}

impl State {
    pub(crate) fn reference(&self) -> Reference {
        Reference {
            module: self.module,
            id: self.id.clone(),
        }
    }
}

/// A whole state file, and the minion it is for.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Sls {
    /// The name the file is applied by: `salt-call state.apply <name>`.
    pub name: String,
    pub host: String,
    /// One state per resource, in apply order.
    pub states: Vec<State>,
    /// The states a notify edge needed beyond the notified resource's own:
    /// a reload, a start, a stop, a command run again. They follow every
    /// resource's state, sorted by ID.
    pub reactions: Vec<State>,
}

impl Sls {
    pub(crate) fn all(&self) -> impl Iterator<Item = &State> {
        self.states.iter().chain(&self.reactions)
    }
}

/// A refusal of one resource, stamped with this target.
pub(crate) fn refuse(r: &Resource, rule: &str, msg: String) -> CompileError {
    CompileError {
        target: NAME.to_owned(),
        ..CompileError::for_resource(r, rule, msg)
    }
}

/// The Salt emitter.
pub struct Salt;

impl Emitter for Salt {
    fn name(&self) -> &str {
        NAME
    }

    fn supported_resource_types(&self) -> Vec<String> {
        supported_resource_types()
    }

    fn resource_schemas(&self) -> Vec<meridian_sdk::ResourceSchema> {
        transform::resource_schemas()
    }

    /// `native_notify`: `watch` and `onchanges` are Salt's own requisites, so
    /// a notification compiles to the tool's mechanism and nothing is shelled
    /// out.
    ///
    /// `runtime_condition`: a state file is rendered by the minion itself,
    /// with its grains and pillar, every time it is applied. A `runtimeWhen`
    /// compiles to a Jinja test in the file, which is evaluated on the node
    /// at apply time. It is deliberately not used for ordinary `when`, which
    /// Meridian evaluates and prunes long before this stage.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: true,
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
        let (mut sls, warnings) = transform::transform(graph)?;
        sort::apply_ordering(graph, &mut sls)?;
        validate::validate(&sls)?;
        let files = emit::serialise(&sls);
        Ok(Emitted {
            artifact: Artifact { files },
            warnings,
        })
    }
}
