//! `meridian import`: existing configuration in, Meridian documents out.
//!
//! The command knows nothing about any tool. It gathers the files it was
//! pointed at, finds the plugin that imports them, and writes what the plugin
//! returns. Everything the plugin could not carry over is printed as a
//! report and written into the documents as `TODO` markers.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use meridian_import::{render_all, Rendered, TODO_MARKER};
use meridian_plugin_host::{ImportFailure, Info, Registry};
use meridian_sdk::{ImportNote, ImportSource, Imported, NoteLevel};

use crate::{count, Failure};

/// The most source text one import may carry. A plugin call is one message,
/// and a configuration anywhere near this size is not one configuration.
const MAX_BYTES: usize = 3 << 20;

pub(crate) struct Request<'a> {
    pub paths: &'a [PathBuf],
    pub from: Option<&'a str>,
    pub name: Option<&'a str>,
    pub out: Option<&'a Path>,
    pub recursive: bool,
    pub force: bool,
    pub strict: bool,
}

/// How many characters of a pattern are not wildcards: the more a pattern
/// spells out, the more it says about a file it matches.
fn specificity(pattern: &str) -> usize {
    pattern.chars().filter(|c| !matches!(c, '*' | '?')).count()
}

/// How strongly a target claims a file by its name; zero when it does not.
fn claim(info: &Info, file_name: &str) -> usize {
    info.import_formats
        .iter()
        .flat_map(|f| f.matching(file_name))
        .map(specificity)
        .max()
        .unwrap_or(0)
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A file found for the import: the name the plugin sees it under, and where
/// it is on disk.
struct Found {
    shown: String,
    path: PathBuf,
    /// Named by the operator, and not found by looking in a directory.
    explicit: bool,
}

/// Lists a directory's files, sorted, descending into its directories when
/// asked. Hidden entries are skipped: they are tool state, not configuration.
fn walk(dir: &Path, prefix: &str, recursive: bool, found: &mut Vec<Found>) -> Result<(), Failure> {
    let failed = |e: std::io::Error| {
        Failure::Message(format!("reading the directory {}: {e}", dir.display()))
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(failed)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()
        .map_err(failed)?;
    entries.sort();
    for path in entries {
        let name = base_name(&path);
        if name.starts_with('.') {
            continue;
        }
        let shown = format!("{prefix}{name}");
        if path.is_dir() {
            if recursive {
                walk(&path, &format!("{shown}/"), recursive, found)?;
            }
        } else if path.is_file() {
            found.push(Found {
                shown,
                path,
                explicit: false,
            });
        }
    }
    Ok(())
}

fn gather(request: &Request) -> Result<Vec<Found>, Failure> {
    let mut found = Vec::new();
    let several = request.paths.len() > 1;
    for path in request.paths {
        if path.is_dir() {
            // With one directory the names are relative to it; with several
            // paths each keeps the path it was given, so two directories'
            // files cannot collide.
            let prefix = if several {
                format!("{}/", path.display())
            } else {
                String::new()
            };
            walk(path, &prefix, request.recursive, &mut found)?;
        } else if path.is_file() {
            found.push(Found {
                shown: if several {
                    path.display().to_string()
                } else {
                    base_name(path)
                },
                path: path.clone(),
                explicit: true,
            });
        } else {
            return Err(Failure::Message(format!(
                "{} is not a file or a directory",
                path.display()
            )));
        }
    }
    Ok(found)
}

fn importers(registry: &Registry) -> Vec<&Info> {
    registry
        .names()
        .into_iter()
        .filter_map(|name| registry.target(name))
        .map(|target| target.info())
        .filter(|info| !info.import_formats.is_empty())
        .collect()
}

fn list(importers: &[&Info]) -> String {
    importers
        .iter()
        .map(|i| i.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Chooses the plugin that imports the files: the one the operator named, or
/// the one whose formats claim them most specifically.
fn choose<'r>(
    registry: &'r Registry,
    request: &Request,
    found: &[Found],
) -> Result<&'r Info, Failure> {
    let importers = importers(registry);
    if let Some(name) = request.from {
        return match registry.target(name) {
            None => Err(Failure::Message(format!(
                "there is no plugin for {name:?}; the plugins that import are: {}",
                list(&importers)
            ))),
            Some(target) if target.info().import_formats.is_empty() => {
                Err(Failure::Message(format!(
                    "the {name} plugin does not import; the plugins that do are: {}",
                    list(&importers)
                )))
            }
            Some(target) => Ok(target.info()),
        };
    }
    if importers.is_empty() {
        return Err(Failure::Message(
            "none of the plugins found can import; point --plugin-dir at the plugins".to_owned(),
        ));
    }

    // A candidate has to claim every file the operator named, and at least
    // one file overall. Among candidates the most specific claim wins: a
    // file called compose.yaml says more than its being YAML does.
    let mut scores: Vec<(&Info, usize)> = Vec::new();
    for info in importers.iter().copied() {
        let mut total = 0;
        let mut all_named = true;
        for file in found {
            let score = claim(info, &base_name(&file.path));
            if score == 0 && file.explicit {
                all_named = false;
            }
            total += score;
        }
        if all_named && total > 0 {
            scores.push((info, total));
        }
    }
    let best = scores.iter().map(|(_, s)| *s).max().unwrap_or(0);
    let winners: Vec<&Info> = scores
        .iter()
        .filter(|(_, s)| *s == best)
        .map(|(i, _)| *i)
        .collect();
    match winners.as_slice() {
        [one] => Ok(one),
        [] => Err(Failure::Message(format!(
            "no plugin recognises these files by name; say which tool they belong to with \
             --from (one of: {})",
            list(&importers)
        ))),
        many => Err(Failure::Message(format!(
            "these files could belong to more than one tool; say which with --from (one of: {})",
            list(many)
        ))),
    }
}

/// The name an import gets when the operator gives none: the directory's
/// name, or the file's name up to its first dot.
fn default_name(paths: &[PathBuf]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    let full = first.canonicalize().unwrap_or_else(|_| first.clone());
    let name = base_name(&full);
    if first.is_dir() {
        name
    } else {
        // A lone file called main or compose says nothing; its directory does.
        let stem = name.split('.').next().unwrap_or_default().to_owned();
        let generic = [
            "main",
            "compose",
            "docker-compose",
            "playbook",
            "site",
            "index",
            "manifest",
            "deployment",
        ];
        match full.parent().map(base_name) {
            Some(parent) if generic.contains(&stem.as_str()) && !parent.is_empty() => parent,
            _ => stem,
        }
    }
}

/// The directory the files came from, when they came from one: the directory
/// that was named, or the one holding the file that was.
fn root(paths: &[PathBuf]) -> String {
    let [only] = paths else {
        return String::new();
    };
    let Ok(full) = only.canonicalize() else {
        return String::new();
    };
    let dir = if full.is_dir() {
        Some(full.as_path())
    } else {
        full.parent()
    };
    dir.map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) fn run(registry: &Registry, request: &Request) -> Result<(), Failure> {
    let mut found = gather(request)?;
    let info = choose(registry, request, &found)?;

    // A directory holds other things than configuration. What was found by
    // looking is kept only if the plugin's formats claim it; what was named
    // is the operator's decision and always goes.
    found.retain(|f| f.explicit || claim(info, &base_name(&f.path)) > 0);
    if found.is_empty() {
        let patterns: Vec<&str> = info
            .import_formats
            .iter()
            .flat_map(|f| f.patterns.iter().map(String::as_str))
            .collect();
        return Err(Failure::Message(format!(
            "found nothing for the {} plugin to import: no file is named like {}{}",
            info.name,
            patterns.join(", "),
            if request.recursive {
                ""
            } else {
                " (directories inside were not searched; pass --recursive to search them)"
            }
        )));
    }

    let mut files = BTreeMap::new();
    let mut bytes = 0;
    for file in &found {
        let text = std::fs::read_to_string(&file.path)
            .map_err(|e| Failure::Message(format!("reading {}: {e}", file.path.display())))?;
        bytes += text.len();
        if bytes > MAX_BYTES {
            return Err(Failure::Message(format!(
                "the files to import hold more than {} MiB of text; import them in smaller groups",
                MAX_BYTES >> 20
            )));
        }
        files.insert(file.shown.clone(), text);
    }
    let source = ImportSource {
        files,
        name: request
            .name
            .map(str::to_owned)
            .unwrap_or_else(|| default_name(request.paths)),
        root: root(request.paths),
    };

    let target = registry
        .target(&info.name)
        .expect("the chosen plugin is registered");
    let imported = target.import(&source).map_err(|e| match e {
        ImportFailure::Source(e) => Failure::Message(format!("{}: {e}", info.name)),
        ImportFailure::Plugin(e) => Failure::Message(e.to_string()),
    })?;

    let label = request
        .paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let rendered = render_all(&imported, &label);

    // Stdout is the documents, or nothing; the account of what happened goes
    // to stderr so a redirect captures a clean file.
    let written = write_documents(&rendered, request)?;
    let invalid = check(&rendered);
    report(&imported, &rendered, &written, &info.name, &label);

    if invalid {
        return Err(Failure::Reported);
    }
    if request.strict && imported.needing_attention() > 0 {
        eprintln!(
            "error: --strict: {} to look at; see the {TODO_MARKER} markers",
            count(imported.needing_attention(), "thing")
        );
        return Err(Failure::Reported);
    }
    Ok(())
}

/// Writes the documents to the output directory, or prints them. Returns
/// where each one went, in order; empty when they were printed.
fn write_documents(rendered: &[Rendered], request: &Request) -> Result<Vec<PathBuf>, Failure> {
    let Some(dir) = request.out else {
        let mut out = std::io::stdout().lock();
        for (index, document) in rendered.iter().enumerate() {
            if index > 0 {
                let _ = writeln!(out, "---");
            }
            let _ = out.write_all(document.text.as_bytes());
        }
        return Ok(Vec::new());
    };
    let paths: Vec<PathBuf> = rendered.iter().map(|d| dir.join(&d.file_name)).collect();
    // Nothing is written unless everything can be: half an import is worse
    // than none.
    if !request.force {
        if let Some(existing) = paths.iter().find(|p| p.exists()) {
            return Err(Failure::Message(format!(
                "{} already exists; pass --force to replace it, or choose another --out",
                existing.display()
            )));
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| Failure::Message(format!("creating {}: {e}", dir.display())))?;
    for (path, document) in paths.iter().zip(rendered) {
        std::fs::write(path, &document.text)
            .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;
    }
    Ok(paths)
}

/// Reads each written document back the way `meridian validate` would. An
/// importer that produced something the language refuses has a bug, and the
/// operator should hear about it now, not at their first compile.
fn check(rendered: &[Rendered]) -> bool {
    let mut invalid = false;
    for document in rendered {
        let problems = match meridian_ir::parse(&document.file_name, document.text.as_bytes()) {
            Ok(documents) => meridian_ir::Bundle::new(documents).validate().err(),
            Err(e) => Some(e),
        };
        if let Some(problems) = problems {
            invalid = true;
            eprintln!(
                "error: the imported document {} is not valid, so it will not compile as \
                 written:\n{problems}",
                document.file_name
            );
        }
    }
    invalid
}

fn report(
    imported: &Imported,
    rendered: &[Rendered],
    written: &[PathBuf],
    plugin: &str,
    label: &str,
) {
    let resources: usize = imported.documents.iter().map(|d| d.resources.len()).sum();
    eprintln!(
        "imported {label} with the {plugin} plugin: {}, {}",
        count(imported.documents.len(), "document"),
        count(resources, "resource")
    );
    for (index, document) in imported.documents.iter().enumerate() {
        let to = written
            .get(index)
            .map(|p| format!(" -> {}", p.display()))
            .unwrap_or_default();
        let todos = rendered.get(index).map(|r| r.todos).unwrap_or(0);
        eprintln!(
            "  {} ({}, {}{}){to}",
            document.name,
            document.kind.as_str(),
            count(document.resources.len(), "resource"),
            if todos > 0 {
                format!(", {todos} TODO")
            } else {
                String::new()
            }
        );
    }
    for (level, heading) in [
        (
            NoteLevel::Untranslated,
            "not translated: the documents do less than the source until these are written",
        ),
        (
            NoteLevel::Changed,
            "changed: carried over, but not as the source wrote it",
        ),
        (NoteLevel::Info, "notes"),
    ] {
        let notes: Vec<&ImportNote> = imported.notes.iter().filter(|n| n.level == level).collect();
        if notes.is_empty() {
            continue;
        }
        eprintln!("{heading} ({}):", notes.len());
        for note in notes {
            eprintln!("  {note}");
        }
    }
    if imported.needing_attention() == 0 {
        eprintln!("everything in the source was carried over");
    } else {
        eprintln!(
            "{} to look at before applying; each is marked {TODO_MARKER} in the documents",
            count(imported.needing_attention(), "thing")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_that_spells_more_out_is_more_specific() {
        assert!(specificity("compose*.y*ml") > specificity("*.yaml"));
        assert_eq!(specificity("*.tf"), 3);
        assert_eq!(specificity("*"), 0);
    }
}
