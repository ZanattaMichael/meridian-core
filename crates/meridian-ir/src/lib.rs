//! Meridian's intermediate representation: the document types, the YAML/JSON
//! parser that produces them, and the validation that every later stage relies
//! on having passed.
//!
//! Nothing in this crate panics on malformed input. Every failure is an
//! [`Error`] carrying the source position it was found at, and a parse or
//! validation pass reports every problem it found, not only the first.

mod api;
mod condition;
mod error;
mod migrate;
mod node;
mod parse;
mod resolve;
mod retarget;
pub mod schema;
mod types;
mod validate;

pub use api::{api_version_name, ApiVersion, Status, Unsupported, VersionTable, API_GROUP};
pub use condition::{prune, Condition, Data};
pub use error::{Error, ErrorList, Position};
pub use meridian_value::{canonical_map, PlacedEscape, Value};
pub use migrate::{migrate, Decisions, MigrateError, Migrated, Step, STEPS};
pub use parse::{parse, parse_file, parse_stream, parse_value};
pub use resolve::{
    check_run_id, resolve, resolve_for, resolve_with, resolved_labels, set_run_id, Removed,
    Resolution, Secrets, RESERVED_DATA_KEY,
};
pub use retarget::{retarget, Moved, Retarget, Retargeted};
pub use types::{
    Credential, Dependency, Document, Endpoint, IfMissing, Import, Infrastructure,
    InfrastructureSpec, Kind, Metadata, Notification, Output, Resource, ResourceSet,
    ResourceSetSpec, Secret, TargetHost, API_VERSION_V1, CREDENTIAL_FROM_AMBIENT,
    ENDPOINT_TYPE_CLOUD, ENDPOINT_TYPE_CLUSTER, IMPORT_FROM_ARTIFACT, IMPORT_FROM_OUTPUT,
    NOTIFY_ON_CHANGED, TARGET_HOST_FROM_INFRASTRUCTURE, TARGET_HOST_FROM_INLINE,
};
pub use validate::{
    endpoint_credential_types, is_document_name, load, secret_reference, validate, Bundle,
    CLOUD_CREDENTIAL_TYPES, CLUSTER_CREDENTIAL_TYPES, ENDPOINT_TYPES, HOST_CREDENTIAL_TYPES,
    MAX_DOCUMENT_NAME,
};
