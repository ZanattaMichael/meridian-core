//! The commands for authors and editors: `fmt`, `lint`, `schema` and `lsp`.
//!
//! Each is a thin shell over `meridian-tooling`, the same library the
//! language server uses, so the command line and the editor always agree.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use meridian_tooling::catalog::Catalog;
use meridian_tooling::check::{self, Diagnostic, Env, Severity, Source, Workspace};
use meridian_tooling::{fmt, jsonschema, lint};

use crate::{apply_sets, count, read_data, registry, Failure, PluginDirs};

#[derive(Args)]
pub struct FmtArgs {
    /// Files to format, or directories to search for Meridian documents;
    /// `-` reads standard input and writes the result to standard output
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Change nothing; list the files that are not formatted and fail if
    /// there are any
    #[arg(long)]
    check: bool,
    /// Keep the order fields are written in, instead of the language's own
    /// order (id, type, state, params, ...)
    #[arg(long)]
    keep_order: bool,
}

#[derive(Args)]
pub struct LintArgs {
    /// Files to check, or directories to search for Meridian documents
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    #[command(flatten)]
    plugins: PluginDirs,
    /// A YAML or JSON file of values that `when` conditions read; without
    /// one, every resource is checked as if its condition held
    #[arg(long, value_name = "FILE")]
    data: Vec<PathBuf>,
    /// One value for `when` conditions, as in `--set env=prod`; may be given
    /// more than once, and is applied after the `--data` files
    #[arg(long, value_name = "KEY=VALUE")]
    set: Vec<String>,
    /// Where fetched modules are kept, for a `--data` file written
    /// `@<module>:<path>`
    #[arg(
        long,
        value_name = "DIR",
        env = "MERIDIAN_STATE_DIR",
        default_value = ".meridian"
    )]
    state_dir: PathBuf,
    /// Apply the fixes that are safe to apply, such as quoting a file mode
    #[arg(long)]
    fix: bool,
    /// Fail on warnings as well as on errors
    #[arg(long)]
    strict: bool,
    /// Print one JSON object per finding instead of text
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct SchemaArgs {
    #[command(flatten)]
    plugins: PluginDirs,
    /// Write rules for this target only, instead of every loaded target
    #[arg(long, value_name = "TARGET")]
    target: Option<String>,
    /// Write the schema to this file instead of standard output
    #[arg(short, long, value_name = "FILE")]
    out: Option<PathBuf>,
}

#[derive(Args)]
pub struct LspArgs {
    #[command(flatten)]
    plugins: PluginDirs,
}

/// Whether a path names a YAML or JSON file by its extension.
fn is_document_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml" | "json")
    )
}

/// Expands the paths a command was given into files. A file named outright
/// is always taken; a directory is searched for Meridian documents.
fn gather(paths: &[PathBuf]) -> Result<Vec<(PathBuf, String)>, Failure> {
    fn visit(dir: &Path, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_dir() {
                // Hidden directories, build output and dependencies hold
                // nobody's source documents.
                if !name.starts_with('.') && !matches!(&*name, "node_modules" | "target") {
                    visit(&path, out)?;
                }
            } else if is_document_file(&path) {
                // Unreadable or binary files under a directory are not ours.
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if check::is_meridian(&text) {
                        out.push((path, text));
                    }
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    for path in paths {
        let shown = path.display();
        if path.is_dir() {
            visit(path, &mut out)
                .map_err(|e| Failure::Message(format!("reading the directory {shown}: {e}")))?;
        } else {
            let text = std::fs::read_to_string(path)
                .map_err(|e| Failure::Message(format!("reading {shown}: {e}")))?;
            out.push((path.clone(), text));
        }
    }
    Ok(out)
}

pub fn format(args: &FmtArgs) -> Result<(), Failure> {
    let options = fmt::Options {
        sort_fields: !args.keep_order,
    };
    if args.paths == [PathBuf::from("-")] {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| Failure::Message(format!("reading standard input: {e}")))?;
        let formatted =
            fmt::format("<stdin>", &text, options).map_err(|e| Failure::Message(e.to_string()))?;
        if args.check {
            return if formatted == text {
                Ok(())
            } else {
                Err(Failure::Message(
                    "standard input is not formatted".to_owned(),
                ))
            };
        }
        // A reader that stops early, such as `head`, is not an error.
        let _ = std::io::stdout().write_all(formatted.as_bytes());
        return Ok(());
    }

    let files = gather(&args.paths)?;
    let (mut changed, mut failed, mut skipped) = (0, 0, 0);
    for (path, text) in &files {
        let name = path.to_string_lossy();
        // Only reached for a file named outright: a directory is searched
        // for Meridian documents alone. Other people's YAML is not ours to
        // lay out.
        if !check::is_meridian(text) {
            skipped += 1;
            eprintln!("skipped {name}: not a Meridian document");
            continue;
        }
        match fmt::format(&name, text, options) {
            Ok(formatted) if formatted == *text => {}
            Ok(formatted) => {
                changed += 1;
                if args.check {
                    println!("{name}");
                } else {
                    std::fs::write(path, formatted)
                        .map_err(|e| Failure::Message(format!("writing {name}: {e}")))?;
                    println!("formatted {name}");
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("error: {name}: {e}");
            }
        }
    }
    let checked = files.len() - skipped;
    let unchanged = checked - changed - failed;
    if args.check {
        if changed > 0 {
            eprintln!(
                "{} not formatted; run `meridian fmt` to format",
                count(changed, "file")
            );
        } else if failed == 0 {
            println!("{} formatted", count(checked, "file"));
        }
    } else if failed == 0 {
        println!(
            "{} formatted, {} already formatted",
            count(changed, "file"),
            unchanged
        );
    }
    if failed > 0 || (args.check && changed > 0) {
        Err(Failure::Reported)
    } else {
        Ok(())
    }
}

fn workspace(files: &[(PathBuf, String)]) -> Workspace {
    let sources: Vec<Source> = files
        .iter()
        .map(|(path, text)| Source {
            name: path.to_string_lossy().into_owned(),
            text: text.clone(),
        })
        .collect();
    Workspace::new(&sources)
}

fn to_json(d: &Diagnostic) -> serde_json::Value {
    serde_json::json!({
        "file": d.file,
        "line": d.span.start.line + 1,
        "column": d.span.start.col + 1,
        "endLine": d.span.end.line + 1,
        "endColumn": d.span.end.col + 1,
        "severity": d.severity.as_str(),
        "code": d.code,
        "source": d.source,
        "message": d.message,
        "fixable": d.fix.is_some(),
    })
}

pub fn lint(args: &LintArgs) -> Result<(), Failure> {
    let mut files = gather(&args.paths)?;
    let registry = registry(&args.plugins)?;
    let catalog = Catalog::from_registry(&registry);
    let data = if args.data.is_empty() && args.set.is_empty() {
        None
    } else {
        let mut data = read_data(&args.data, &args.state_dir)?;
        apply_sets(&mut data, &args.set)?;
        Some(data)
    };
    let run = |files: &[(PathBuf, String)]| {
        let workspace = workspace(files);
        let env = Env {
            registry: Some(&registry),
            catalog: &catalog,
            data: data.as_ref(),
        };
        let mut found = check::check(&workspace, &env);
        found.extend(lint::lint(&workspace, &catalog));
        found.sort_by(|a, b| (&a.file, a.span.start).cmp(&(&b.file, b.span.start)));
        (found, workspace)
    };

    let (mut found, mut checked) = run(&files);
    let mut fixed = 0;
    if args.fix {
        for (path, text) in &mut files {
            let name = path.to_string_lossy().into_owned();
            let (new, applied) = lint::apply_fixes(&name, text, &found);
            if applied > 0 {
                std::fs::write(&*path, &new)
                    .map_err(|e| Failure::Message(format!("writing {name}: {e}")))?;
                *text = new;
                fixed += applied;
            }
        }
        if fixed > 0 {
            (found, checked) = run(&files);
        }
    }
    // A plugin that will not exit is worth a warning, not a failed run.
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }

    let mut out = std::io::stdout().lock();
    for d in &found {
        let _ = if args.json {
            writeln!(out, "{}", to_json(d))
        } else {
            writeln!(out, "{d}")
        };
    }
    let errors = found
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = found
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    let notes = found.len() - errors - warnings;
    let documents: usize = checked.files.iter().map(|f| f.docs.len()).sum();
    if !args.json {
        if fixed > 0 {
            let _ = writeln!(out, "fixed {}", count(fixed, "finding"));
        }
        if catalog.is_empty() {
            let _ = writeln!(
                out,
                "note: no target plugins were found, so documents were checked against the \
                 language only"
            );
        }
        let _ = if found.is_empty() {
            writeln!(
                out,
                "{} in {} checked, nothing to report",
                count(documents, "document"),
                count(files.len(), "file")
            )
        } else {
            writeln!(
                out,
                "{}, {}, {}",
                count(errors, "error"),
                count(warnings, "warning"),
                count(notes, "note")
            )
        };
    }
    if errors > 0 || (args.strict && warnings > 0) {
        Err(Failure::Reported)
    } else {
        Ok(())
    }
}

pub fn schema(args: &SchemaArgs) -> Result<(), Failure> {
    let registry = registry(&args.plugins)?;
    let catalog = Catalog::from_registry(&registry);
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    if let Some(target) = &args.target {
        if catalog.target(target).is_none() {
            return Err(Failure::Message(format!(
                "no plugin provides target {target:?} (loaded targets: {})",
                if catalog.is_empty() {
                    "none".to_owned()
                } else {
                    catalog.names().join(", ")
                }
            )));
        }
    }
    let schema = jsonschema::export(&catalog, args.target.as_deref());
    let mut text = serde_json::to_string_pretty(&schema)
        .map_err(|e| Failure::Message(format!("writing the schema: {e}")))?;
    text.push('\n');
    match &args.out {
        Some(path) => std::fs::write(path, text)
            .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display()))),
        None => {
            let _ = std::io::stdout().write_all(text.as_bytes());
            Ok(())
        }
    }
}

pub fn lsp(args: &LspArgs) -> Result<(), Failure> {
    let mut dirs = args.plugins.dirs.clone();
    // The directory the binary is in is searched last, so a directory given
    // on the command line or by the editor wins.
    dirs.extend(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
    );
    meridian_lsp::run_stdio(dirs).map_err(|e| Failure::Message(e.to_string()))
}
