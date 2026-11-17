//! Importing an existing OpenTofu configuration.
//!
//! The Terraform language is shared with Terraform, so the reading is done by
//! `meridian_import::tf`; this module says which flavour of it this target
//! is.

use meridian_import::tf;
use meridian_sdk::{ImportError, ImportFormat, ImportSource, Imported, Importer};

/// Reads `*.tofu`, `*.tf` and their `.json` forms` into a document for this target.
pub struct OpenTofuImporter;

impl Importer for OpenTofuImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        tf::OPENTOFU.formats()
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        tf::import(source, &tf::OPENTOFU)
    }
}
