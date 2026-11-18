//! Meridian's hierarchical data resolution: a Hiera-style layered merge owned
//! natively by the core, with no dependency on Puppet's Hiera or PowerShell's
//! Datum.
//!
//! Resolution is a pure function of (hierarchy, scope, layer contents). Given
//! the same inputs it produces byte-identical output, which is what makes
//! compilation deterministic downstream.

mod hierarchy;
mod merge;
mod resolve;

pub use hierarchy::{Hierarchy, KeyRule, Scope, Strategy, Strictness};
pub use resolve::{
    resolve, Data, Error, FsLoader, Kind, Layer, LoadError, Loader, MapLoader, Resolved,
};
