//! Importing an existing Terraform configuration.
//!
//! The Terraform language is shared with OpenTofu, so the reading is done by
//! `meridian_import::tf`; this module says which flavour of it this target
//! is.

use meridian_import::tf;
use meridian_sdk::{ImportError, ImportFormat, ImportSource, Imported, Importer};

/// Reads `*.tf` and `*.tf.json` into a document for this target.
pub struct TerraformImporter;

impl Importer for TerraformImporter {
    fn formats(&self) -> Vec<ImportFormat> {
        tf::TERRAFORM.formats()
    }

    fn import(&self, source: &ImportSource) -> Result<Imported, ImportError> {
        tf::import(source, &tf::TERRAFORM)
    }
}
