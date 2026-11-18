//! Stage three: the constraints only the tool's own rules can state, checked
//! on the whole model once every resource has been mapped.

use std::collections::BTreeMap;

use meridian_sdk::rules;
use meridian_sdk::CompileError;

use crate::{refuse_at, Entry};

pub(crate) fn validate(entries: &[Entry]) -> Result<(), CompileError> {
    // TODO(plugin): replace these with what your tool would reject. The aim
    // is that a document the tool would refuse is refused here instead, with
    // the author's own line, before anything is run.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for entry in entries {
        if !entry.path.starts_with('/') {
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                &rules::INVALID_PATH,
                format!(
                    "has the path {:?}, and filesync needs an absolute one",
                    entry.path
                ),
            ));
        }
        // The manifest is one line per file, tab separated.
        if entry.path.contains(['\t', '\n']) || entry.mode.contains(['\t', '\n']) {
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                &rules::INVALID_PATH,
                "has a tab or a line break in its path or mode, which the manifest cannot hold"
                    .to_owned(),
            ));
        }
        if let Some(first) = seen.insert(&entry.path, &entry.id) {
            return Err(refuse_at(
                &entry.id,
                &entry.pos,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "manages {:?}, which resource {first:?} already manages",
                    entry.path
                ),
            ));
        }
    }
    Ok(())
}
