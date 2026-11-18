//! Finding the rules file on disk and keeping it out of the hierarchy.

use std::path::{Path, PathBuf};

use meridian_resolve::{FsLoader, Hierarchy, Scope};

use crate::{enforce, load, Error, Kind, Layer, Loaded, Refusal, Rules};

/// The name the rules file is looked for under, beside the hierarchy file.
pub const RULES_FILE: &str = "rules.yaml";

/// Which files to check.
#[derive(Clone, Debug, Default)]
pub struct Files {
    /// The hierarchy file. Layer paths are relative to its directory.
    pub hierarchy: PathBuf,
    /// A rules file to use instead of `rules.yaml` beside the hierarchy.
    pub rules: Option<PathBuf>,
    /// Refuse when there is no rules file, so removing it cannot switch the
    /// rules off unnoticed.
    pub require_rules: bool,
}

/// A hierarchy that was loaded and passed every rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    pub loaded: Loaded,
    /// The rules file that was applied and its rules, or `None` when there
    /// is none and none was required.
    pub rules: Option<(PathBuf, Rules)>,
    /// Each layer's path and group, in order, for a report.
    pub groups: Vec<(String, String)>,
}

/// Reads the hierarchy file and its rules file from disk, loads the layers
/// and enforces the rules.
pub fn check_files(
    files: &Files,
    scope: &Scope,
    command_line: &[Layer],
) -> Result<Checked, Refusal> {
    let name = files.hierarchy.to_string_lossy();
    let raw = std::fs::read(&files.hierarchy).map_err(|e| {
        Error::new(
            Kind::Hierarchy,
            format!("reading the hierarchy file {name}: {e}"),
        )
    })?;
    let hierarchy = Hierarchy::parse(&name, &raw).map_err(Error::from)?;
    let root = match files.hierarchy.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let loader = FsLoader::new(&root);

    let rules_path = match &files.rules {
        Some(path) => Some(path.clone()),
        None => Some(root.join(RULES_FILE)).filter(|p| p.is_file()),
    };
    let Some(rules_path) = rules_path else {
        if files.require_rules {
            return Err(Error::new(
                Kind::File,
                format!(
                    "compile rules are required for this run, and there is no {RULES_FILE} beside \
                     {name} and no --rules file"
                ),
            )
            .into());
        }
        let loaded = load(&hierarchy, scope, &loader, command_line)?;
        return Ok(Checked {
            groups: Vec::new(),
            loaded,
            rules: None,
        });
    };

    let rules_name = rules_path.to_string_lossy();
    let raw = std::fs::read(&rules_path).map_err(|e| {
        Error::new(
            Kind::File,
            format!("reading the rules file {rules_name}: {e}"),
        )
    })?;
    let rules = Rules::parse(&rules_name, &raw)?;

    // The rules file must be outside what it constrains. A layer path that
    // resolves to it would put the rules into the merge, where a layer could
    // answer them.
    let paths = hierarchy.expand_paths(scope).map_err(Error::from)?;
    for path in &paths {
        if same_file(&root.join(path), &rules_path) {
            return Err(Error::new(
                Kind::File,
                format!(
                    "the rules file {rules_name} is also the hierarchy layer {path:?}; the rules \
                     file is never a layer, so take it out of the hierarchy"
                ),
            )
            .into());
        }
    }

    let loaded = enforce(&rules, &hierarchy, scope, &loader, command_line)?;
    let groups = loaded
        .layers
        .iter()
        .map(|layer| {
            let group = match layer.origin {
                crate::Origin::CommandLine | crate::Origin::Set => crate::COMMAND_LINE,
                crate::Origin::Hierarchy => rules.group_of(&layer.path),
            };
            (layer.path.clone(), group.to_owned())
        })
        .collect();
    Ok(Checked {
        loaded,
        rules: Some((rules_path, rules)),
        groups,
    })
}

/// Whether two paths name one file, through links and `..` alike. A path
/// that does not exist is no file, so it is the same as nothing.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
