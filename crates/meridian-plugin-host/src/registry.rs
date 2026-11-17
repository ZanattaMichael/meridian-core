use std::collections::BTreeMap;

use meridian_sdk::{
    Applied, Artifact, Data, Emitted, Emitter, ImportSource, Imported, Importer, LogLine,
    Operations, Output, Plan, ResourceGraph, RunContext, PROTOCOL_VERSION, VERSION,
};

use crate::client::{Client, StartOptions};
use crate::{EmitError, Error, ImportFailure, Info, RunFailure, BINARY_PREFIX};

/// A registered target: an emitter compiled into this binary, or a plugin
/// process. Code that compiles a document does not need to tell them apart.
pub struct Target {
    info: Info,
    kind: Kind,
}

enum Kind {
    InProcess(Box<dyn Emitter + Send + Sync>, Option<Box<dyn Importer>>),
    Plugin(Box<Client>),
}

impl Target {
    /// What the target reported about itself.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Compiles a tree with this target.
    pub fn emit(&self, graph: &ResourceGraph, data: &Data) -> Result<Emitted, EmitError> {
        match &self.kind {
            Kind::Plugin(client) => client.emit(graph, data),
            // A compiled-in emitter does not know its own registered name is
            // what the reader needs to see, so it is stamped here, exactly as
            // it is for a refusal that crossed the process boundary.
            Kind::InProcess(emitter, _) => emitter.emit(graph, data).map_err(|mut e| {
                e.target = self.info.name.clone();
                EmitError::Refused(e)
            }),
        }
    }

    /// Reads a tool's existing configuration into Meridian documents. Every
    /// document that names no target is stamped with this one.
    pub fn import(&self, source: &ImportSource) -> Result<Imported, ImportFailure> {
        match &self.kind {
            Kind::Plugin(client) => client.import(source),
            Kind::InProcess(_, Some(importer)) => {
                let mut imported = importer.import(source).map_err(ImportFailure::Source)?;
                for document in &mut imported.documents {
                    if document.target.is_empty() {
                        document.target = self.info.name.clone();
                    }
                }
                Ok(imported)
            }
            Kind::InProcess(_, None) => Err(ImportFailure::Plugin(Error {
                plugin: self.info.name.clone(),
                msg: "this target does not import".to_owned(),
                ..Error::default()
            })),
        }
    }

    /// Previews what applying an artifact would change.
    pub fn plan(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &mut dyn FnMut(LogLine),
    ) -> Result<Plan, RunFailure> {
        self.client("plan")?.plan(context, artifact, log)
    }

    /// Makes an artifact's desired state real.
    pub fn apply(
        &self,
        context: &RunContext,
        artifact: &Artifact,
        log: &mut dyn FnMut(LogLine),
    ) -> Result<Applied, RunFailure> {
        self.client("apply")?.apply(context, artifact, log)
    }

    /// Reads the values the last apply produced.
    pub fn outputs(&self, context: &RunContext) -> Result<Vec<Output>, RunFailure> {
        self.client("report outputs")?.outputs(context)
    }

    /// A compiled-in emitter only compiles; running a tool is a plugin's job.
    fn client(&self, verb: &str) -> Result<&Client, RunFailure> {
        match &self.kind {
            Kind::Plugin(client) => Ok(client),
            Kind::InProcess(..) => Err(RunFailure::Plugin(Error {
                plugin: self.info.name.clone(),
                msg: format!("this target does not {verb}"),
                ..Error::default()
            })),
        }
    }
}

/// Resolves a target name to the target that implements it.
///
/// It holds both compiled-in and subprocess targets behind one lookup. That is
/// what lets a target move from built-in to plugin, or the reverse, without
/// touching the compiler.
#[derive(Default)]
pub struct Registry {
    targets: BTreeMap<String, Target>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// Adds a compiled-in emitter.
    ///
    /// A duplicate target name is an error, not an overwrite: two targets
    /// claiming "ansible" means the operator's plugin directory disagrees with
    /// their build, and silently picking one would make which output they got
    /// depend on load order.
    pub fn register(&mut self, emitter: Box<dyn Emitter + Send + Sync>) -> Result<(), Error> {
        self.register_with_importer(emitter, None)
    }

    /// Adds a compiled-in emitter together with the importer that reads its
    /// tool's existing configuration.
    pub fn register_with_importer(
        &mut self,
        emitter: Box<dyn Emitter + Send + Sync>,
        importer: Option<Box<dyn Importer>>,
    ) -> Result<(), Error> {
        let name = emitter.name().to_owned();
        if name.is_empty() {
            return Err(Error {
                msg: "cannot register an emitter with no target name".to_owned(),
                ..Error::default()
            });
        }
        self.claim(&name, "")?;
        let info = Info {
            protocol_version: PROTOCOL_VERSION,
            sdk_version: VERSION.to_owned(),
            name: name.clone(),
            resource_types: emitter.supported_resource_types(),
            capabilities: emitter.capabilities(),
            category: emitter.category(),
            target_versions: emitter.target_versions(),
            resource_schemas: emitter.resource_schemas(),
            import_formats: importer.as_ref().map(|i| i.formats()).unwrap_or_default(),
            operations: Operations::COMPILE_ONLY,
            accepts_inputs: false,
            path: String::new(),
            signature: crate::Signature::NotChecked,
        };
        self.targets.insert(
            name,
            Target {
                info,
                kind: Kind::InProcess(emitter, importer),
            },
        );
        Ok(())
    }

    /// Starts one plugin binary and registers the target it reports.
    pub fn load(&mut self, path: &str, options: &StartOptions) -> Result<Info, Error> {
        let client = Client::start(path, options)?;
        // On a conflict the client is dropped here, which shuts it down.
        self.claim(client.name(), path)?;
        let info = client.info().clone();
        self.targets.insert(
            info.name.clone(),
            Target {
                info: info.clone(),
                kind: Kind::Plugin(Box::new(client)),
            },
        );
        Ok(info)
    }

    /// Checks that a target name is free.
    fn claim(&self, name: &str, path: &str) -> Result<(), Error> {
        let Some(existing) = self.targets.get(name) else {
            return Ok(());
        };
        let held_by = if existing.info.path.is_empty() {
            "compiled into this binary"
        } else {
            &existing.info.path
        };
        let claimant = if path.is_empty() {
            "a compiled-in emitter"
        } else {
            path
        };
        Err(Error {
            path: path.to_owned(),
            msg: format!("target {name:?} is already registered from {held_by}; {claimant} claims the same name"),
            ..Error::default()
        })
    }

    /// Loads every plugin binary in a directory, by naming convention.
    ///
    /// A directory that does not exist is not an error: an operator with no
    /// plugins installed is the normal case, not a misconfiguration. Loading
    /// stops at the first plugin that fails; those loaded before it stay
    /// registered.
    pub fn discover(&mut self, dir: &str, options: &StartOptions) -> Result<Vec<Info>, Error> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(Error {
                    path: dir.to_owned(),
                    msg: "could not read the plugin directory".to_owned(),
                    cause: e.to_string(),
                    ..Error::default()
                })
            }
        };
        // Sorted, so two hosts with the same directory load in the same order
        // and a duplicate-name conflict reports the same pair every time.
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
            .filter(|e| is_plugin_name(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        paths.sort();

        let mut loaded = Vec::with_capacity(paths.len());
        for path in paths {
            loaded.push(self.load(&path.to_string_lossy(), options)?);
        }
        Ok(loaded)
    }

    /// The target registered under a name.
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.targets.get(name)
    }

    /// Every registered target name, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.targets.keys().map(String::as_str).collect()
    }

    /// Shuts down every plugin process the registry started.
    ///
    /// All of them are closed even if one fails, because leaving processes
    /// running because a sibling misbehaved is the leak this exists to
    /// prevent. Dropping the registry does the same without the report.
    pub fn close(self) -> Result<(), Error> {
        let failures: Vec<String> = self
            .targets
            .into_values()
            .filter_map(|t| match t.kind {
                Kind::Plugin(client) => client.close().err(),
                Kind::InProcess(..) => None,
            })
            .map(|e| e.to_string())
            .collect();
        if failures.is_empty() {
            return Ok(());
        }
        Err(Error {
            msg: format!("shutting down plugins: {}", failures.join("; ")),
            ..Error::default()
        })
    }
}

/// Applies the naming convention, so discovery never executes a binary just to
/// find out what it is.
///
/// A target name holds no dot, so the files a build leaves beside a binary
/// (`meridian-target-ansible.d`, `.pdb`) are not taken for plugins.
fn is_plugin_name(name: &str) -> bool {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name.strip_prefix(BINARY_PREFIX)
        .is_some_and(|target| !target.is_empty() && !target.contains('.'))
}

/// The file name a plugin for the given target must have.
pub fn binary_name(target: &str) -> String {
    format!("{BINARY_PREFIX}{target}{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_goes_by_name() {
        for (name, want) in [
            ("meridian-target-ansible", true),
            ("meridian-target-ansible.exe", true),
            ("meridian-target-dsc-v3", true),
            ("meridian-target-ansible.d", false),
            ("meridian-target-ansible.pdb", false),
            ("meridian-target-", false),
            ("meridian-target-.exe", false),
            ("meridian", false),
            ("README.md", false),
            ("not-meridian-target-x", false),
        ] {
            assert_eq!(is_plugin_name(name), want, "{name}");
        }
        assert!(binary_name("puppet").starts_with("meridian-target-puppet"));
    }
}
