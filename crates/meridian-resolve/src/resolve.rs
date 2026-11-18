use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use meridian_ir::Value;

use crate::hierarchy::{Hierarchy, Scope, Strategy, Strictness};
use crate::merge::{deep_merge, unique_array_merge};

/// One layer's contents, or a fully resolved result. Ordered by key.
pub type Data = BTreeMap<String, Value>;

/// A resolution failure. The message names the hierarchy entry, layer or key
/// at fault; `kind` says which of the three it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: Kind,
    msg: String,
}

/// What a resolution failure is about, each with a fixed code listed in
/// `docs/reference/errors.md`. A code is never reused for another meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The hierarchy file itself: it does not parse, declares no layers,
    /// names a strategy that does not exist, or two of its entries resolve
    /// to the same layer.
    Hierarchy,
    /// A layer the hierarchy names could not be loaded.
    Layer,
    /// Two layers' values for one key could not be merged under its rule.
    Merge,
}

impl Kind {
    /// Every kind, in the order the reference lists them.
    pub const ALL: [Kind; 3] = [Kind::Hierarchy, Kind::Layer, Kind::Merge];

    /// The fixed code.
    pub fn code(self) -> &'static str {
        match self {
            Kind::Hierarchy => "resolve-hierarchy",
            Kind::Layer => "resolve-layer",
            Kind::Merge => "resolve-merge",
        }
    }
}

impl Error {
    pub(crate) fn new(kind: Kind, msg: impl Into<String>) -> Self {
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

/// Why a [`Loader`] could not produce a layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The layer does not exist. Whether that is fatal depends on the
    /// hierarchy's strictness.
    NotFound,
    /// The layer exists and could not be used.
    Failed(String),
}

/// Reads one hierarchy layer by its expanded path. Keeping this a trait is
/// what lets resolution stay a pure, disk-free unit under test.
pub trait Loader {
    fn load(&self, path: &str) -> Result<Data, LoadError>;
}

/// Reads layers as YAML or JSON files beneath a root directory.
///
/// A layer path comes from interpolating scope variables, so it is not
/// trusted: one that is absolute, or that climbs out of the root with `..`, is
/// refused before the filesystem is touched.
#[derive(Clone, Debug)]
pub struct FsLoader {
    root: PathBuf,
}

impl FsLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FsLoader { root: root.into() }
    }
}

impl Loader for FsLoader {
    fn load(&self, path: &str) -> Result<Data, LoadError> {
        let relative = Path::new(path);
        if !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(LoadError::Failed(
                "layer path must stay inside the hierarchy root".to_owned(),
            ));
        }
        let raw = match std::fs::read(self.root.join(relative)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::NotFound),
            Err(e) => return Err(LoadError::Failed(e.to_string())),
        };
        match meridian_ir::parse_value(path, &raw) {
            Ok(Value::Map(map)) => Ok(map),
            Ok(Value::Null) => Ok(Data::new()),
            Ok(other) => Err(LoadError::Failed(format!(
                "a layer must be a mapping, found {}",
                other.describe()
            ))),
            Err(e) => Err(LoadError::Failed(e.to_string())),
        }
    }
}

/// Serves layers from memory.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapLoader(pub BTreeMap<String, Data>);

impl Loader for MapLoader {
    fn load(&self, path: &str) -> Result<Data, LoadError> {
        self.0.get(path).cloned().ok_or(LoadError::NotFound)
    }
}

/// What one hierarchy level contributed, in priority order. Kept for
/// `meridian explain` and for debugging a surprising merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub path: String,
    pub found: bool,
    /// The keys the layer declares, sorted.
    pub keys: Vec<String>,
}

/// A completed resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub data: Data,
    pub layers: Vec<Layer>,
}

/// Merges every hierarchy layer for one node into a single flat map.
///
/// Layers are listed lowest-priority first, so a later layer overrides an
/// earlier one. The result depends only on the hierarchy, the scope and the
/// layer contents: repeated runs over the same inputs produce identical
/// output.
pub fn resolve(h: &Hierarchy, scope: &Scope, loader: &dyn Loader) -> Result<Resolved, Error> {
    h.validate()?;
    let paths = h.expand_paths(scope)?;

    let mut layers = Vec::with_capacity(paths.len());
    let mut loaded: Vec<Data> = Vec::with_capacity(paths.len());
    for path in paths {
        match loader.load(&path) {
            Ok(data) => {
                layers.push(Layer {
                    found: true,
                    keys: data.keys().cloned().collect(),
                    path,
                });
                loaded.push(data);
            }
            Err(LoadError::NotFound) if h.strictness == Strictness::Lenient => {
                layers.push(Layer {
                    path,
                    found: false,
                    keys: Vec::new(),
                });
            }
            Err(LoadError::NotFound) => {
                return Err(Error::new(
                    Kind::Layer,
                    format!("resolve: {path:?}: hierarchy layer not found"),
                ));
            }
            Err(LoadError::Failed(why)) => {
                return Err(Error::new(Kind::Layer, format!("resolve: {path:?}: {why}")))
            }
        }
    }

    // Every key any layer declares, in sorted order.
    let keys: BTreeSet<String> = loaded
        .iter()
        .flat_map(|data| data.keys().cloned())
        .collect();

    let mut out = Data::new();
    for key in keys {
        let (strategy, dedup_key) = h.rule_for(&key);
        // Values in priority order, lowest first, from the layers declaring the key.
        let mut values: Vec<Value> = loaded
            .iter_mut()
            .filter_map(|data| data.remove(&key))
            .collect();
        let merged = match strategy {
            Strategy::FirstFound => values.pop(),
            Strategy::DeepMerge => values.into_iter().reduce(deep_merge),
            Strategy::UniqueArrayMerge => Some(Value::List(
                unique_array_merge(values, dedup_key)
                    .map_err(|e| Error::new(Kind::Merge, format!("resolve: key {key:?}: {e}")))?,
            )),
        };
        if let Some(value) = merged {
            out.insert(key, value);
        }
    }
    Ok(Resolved { data: out, layers })
}
