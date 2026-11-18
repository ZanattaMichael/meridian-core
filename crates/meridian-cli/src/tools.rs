//! The commands for authors and editors: `fmt`, `lint`, `schema` and `lsp`.
//!
//! Each is a thin shell over `meridian-tooling`, the same library the
//! language server uses, so the command line and the editor always agree.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use meridian_lsp::Discovery;
use meridian_plugin_host::{Policy as SignaturePolicy, Registry, StartOptions, Verification};
use meridian_tooling::catalog::Catalog;
use meridian_tooling::check::{self, Diagnostic, Env, Severity, Source, Workspace};
use meridian_tooling::schemas::{self, Cached, Freshness, Store};
use meridian_tooling::{fmt, jsonschema, lint};

use crate::{apply_sets, count, read_data, registry, trusted_keys, Failure, PluginDirs};

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
    /// Where fetched modules and synced tool schemas are kept, for a
    /// `--data` file written `@<module>:<path>` and for checking native
    /// params against what `meridian schema sync` cached
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
    #[command(subcommand)]
    command: Option<SchemaCommand>,
    #[command(flatten)]
    plugins: PluginDirs,
    /// Write rules for this target only, instead of every loaded target
    #[arg(long, value_name = "TARGET")]
    target: Option<String>,
    /// Write the schema to this file instead of standard output
    #[arg(short, long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Where the tool schemas `meridian schema sync` cached are read from
    #[arg(
        long,
        value_name = "DIR",
        env = "MERIDIAN_STATE_DIR",
        default_value = ".meridian"
    )]
    state_dir: PathBuf,
}

#[derive(Subcommand)]
pub enum SchemaCommand {
    /// Ask each target's installed tool for the schemas of the resource
    /// types the documents use, and cache them under the state directory
    /// for editors, lint and the JSON Schema
    Sync {
        /// Files to read, or directories to search for Meridian documents
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        plugins: PluginDirs,
        /// Sync this target only
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// A YAML or JSON file of values that `when` conditions read
        #[arg(long, value_name = "FILE")]
        data: Vec<PathBuf>,
        /// One value for `when` conditions, as in `--set env=prod`
        #[arg(long, value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Where the cache is written: `<DIR>/schemas/<target>.json`
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
        /// Change nothing; fail if any cache is missing or is not what
        /// meridian.lock records. For CI
        #[arg(long)]
        check: bool,
        /// Leave meridian.lock alone
        #[arg(long)]
        no_lock: bool,
    },
    /// Show what is cached for each target and how it stands against
    /// meridian.lock
    Status {
        #[command(flatten)]
        plugins: PluginDirs,
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
        /// One JSON object per line instead of text
        #[arg(long)]
        json: bool,
    },
    /// Remove the cache of the named targets, or of every target, and
    /// their records in meridian.lock
    Clean {
        /// The targets to forget; every target when none is named
        targets: Vec<String>,
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
}

#[derive(Args)]
pub struct LspArgs {
    #[command(flatten)]
    plugins: PluginDirs,
    /// Load plugins from the directories the editor names in its settings
    /// under the same --plugin-signatures policy as the rest. Without this,
    /// a plugin in such a directory is started only when it is signed by a
    /// trusted key, since an editor's settings can come with a project
    #[arg(long, env = "MERIDIAN_LSP_TRUST_EDITOR_PLUGIN_DIRS")]
    trust_editor_plugin_dirs: bool,
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
        let _ = crate::Stdout.write_all(formatted.as_bytes());
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
                    outln!("{name}");
                } else {
                    std::fs::write(path, formatted)
                        .map_err(|e| Failure::Message(format!("writing {name}: {e}")))?;
                    outln!("formatted {name}");
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
            outln!("{} formatted", count(checked, "file"));
        }
    } else if failed == 0 {
        outln!(
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
        .map(|(path, text)| Source::new(path.to_string_lossy(), text))
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
    let mut catalog = Catalog::from_registry(&registry);
    // The tool schemas `meridian schema sync` cached check native params
    // where the plugin's own description leaves them open.
    let store = Store::new(&args.state_dir);
    for problem in catalog.read_cache(&store, schemas::lock_in(Path::new(".")).as_ref()) {
        eprintln!("warning: {problem}");
    }
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

    let mut out = crate::Stdout;
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
    match &args.command {
        Some(SchemaCommand::Sync {
            paths,
            plugins,
            target,
            data,
            set,
            state_dir,
            check,
            no_lock,
        }) => {
            return schema_sync(&SyncArgs {
                paths,
                plugins,
                target: target.as_deref(),
                data,
                set,
                state_dir,
                check: *check,
                lock: !*no_lock,
            })
        }
        Some(SchemaCommand::Status {
            plugins,
            state_dir,
            json,
        }) => return schema_status(plugins, state_dir, *json),
        Some(SchemaCommand::Clean { targets, state_dir }) => {
            return schema_clean(targets, state_dir)
        }
        None => {}
    }
    let registry = registry(&args.plugins)?;
    let mut catalog = Catalog::from_registry(&registry);
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    // The tool schemas that were synced belong in the export too, so an
    // editor that only knows YAML checks native types as well.
    let store = Store::new(&args.state_dir);
    for problem in catalog.read_cache(&store, schemas::lock_in(Path::new(".")).as_ref()) {
        eprintln!("warning: {problem}");
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
            let _ = crate::Stdout.write_all(text.as_bytes());
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
    let options = StartOptions {
        verification: args.plugins.verification()?,
        ..StartOptions::default()
    };
    let editor_options = if args.trust_editor_plugin_dirs {
        options.clone()
    } else {
        StartOptions {
            verification: Verification {
                policy: SignaturePolicy::Require,
                trusted: trusted_keys()?,
            },
            ..StartOptions::default()
        }
    };
    meridian_lsp::run_stdio(Discovery {
        dirs,
        options,
        editor_options,
    })
    .map_err(|e| Failure::Message(e.to_string()))
}

struct SyncArgs<'a> {
    paths: &'a [PathBuf],
    plugins: &'a PluginDirs,
    target: Option<&'a str>,
    data: &'a [PathBuf],
    set: &'a [String],
    state_dir: &'a Path,
    check: bool,
    lock: bool,
}

/// Compiles every document in the files to the tree its target would get,
/// as far as that can be done without running anything, and groups the
/// trees by target. A document whose conditions need data that was not
/// given is read as written: a type name is almost always literal.
fn graphs_by_target(
    files: &[(PathBuf, String)],
    data: Option<&meridian_sdk::Data>,
) -> Result<std::collections::BTreeMap<String, Vec<meridian_sdk::ResourceGraph>>, Failure> {
    use meridian_ast::ResourceGraph;
    use meridian_ir::{Document, Resolution};
    use meridian_plugin_host::to_sdk;

    let workspace = workspace(files);
    let mut by_target: std::collections::BTreeMap<String, Vec<meridian_sdk::ResourceGraph>> =
        Default::default();
    let empty = meridian_sdk::Data::new();
    for file in &workspace.files {
        for problem in &file.errors {
            if problem.severity == Severity::Error {
                return Err(Failure::Message(format!(
                    "{}: {}; fix the document before syncing",
                    file.name, problem.message
                )));
            }
        }
        let bundle = workspace.bundle_for(&file.name);
        for parsed in &file.docs {
            let doc = match meridian_ir::resolve(&parsed.doc, data.unwrap_or(&empty)) {
                Ok(Resolution::Kept { document, .. }) => *document,
                Ok(Resolution::Skipped { .. }) => continue,
                Err(_) => parsed.doc.clone(),
            };
            let target = doc.target().to_owned();
            if meridian_expr::has_interpolation(&target) {
                eprintln!(
                    "note: {}: document {:?} works its target out from data, so it is left out; \
                     give the data with --data or --set",
                    file.name,
                    doc.name()
                );
                continue;
            }
            let graph = match &doc {
                Document::Infrastructure(infra) => ResourceGraph::build_infrastructure(infra),
                Document::ResourceSet(rs) => {
                    let host = bundle
                        .resolve_target_host(rs)
                        .map(|(_, output)| output.value.clone())
                        .ok()
                        .filter(|h| !h.is_empty())
                        .unwrap_or_else(|| "host".to_owned());
                    ResourceGraph::build(rs, &host)
                }
            };
            match graph {
                Ok(graph) => by_target.entry(target).or_default().push(to_sdk(&graph)),
                Err(e) => {
                    return Err(Failure::Message(format!(
                        "{}: document {:?}: {e}; fix the document before syncing",
                        file.name,
                        doc.name()
                    )))
                }
            }
        }
    }
    Ok(by_target)
}

fn schema_sync(args: &SyncArgs<'_>) -> Result<(), Failure> {
    let files = gather(args.paths)?;
    if files.is_empty() {
        return Err(Failure::Message(
            "no Meridian documents were found in the paths given".to_owned(),
        ));
    }
    let data = if args.data.is_empty() && args.set.is_empty() {
        None
    } else {
        let mut data = read_data(args.data, args.state_dir)?;
        apply_sets(&mut data, args.set)?;
        Some(data)
    };
    let by_target = graphs_by_target(&files, data.as_ref())?;
    let registry: Registry = registry(args.plugins)?;
    let store = Store::new(args.state_dir);
    let project = Path::new(".");
    let mut lock = if args.lock || args.check {
        meridian_module::Lock::load(project).map_err(|e| Failure::Message(e.to_string()))?
    } else {
        None
    };

    let mut failed = 0;
    let mut synced = 0;
    let mut stale = 0;
    let mut wanted: Vec<&String> = by_target.keys().collect();
    if let Some(only) = args.target {
        if !by_target.contains_key(only) {
            return Err(Failure::Message(format!(
                "no document in the paths given uses target {only:?} (they use: {})",
                if by_target.is_empty() {
                    "none".to_owned()
                } else {
                    wanted
                        .iter()
                        .map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )));
        }
        wanted.retain(|t| t.as_str() == only);
    }
    for name in wanted {
        let graphs = &by_target[name];
        let Some(target) = registry.target(name) else {
            eprintln!(
                "warning: no plugin provides target {name:?}, so its documents cannot be synced"
            );
            continue;
        };
        if !target.info().exports_schemas {
            // Not a failure: the plugin describes what it describes.
            println!("{name}: the plugin does not export its tool's schemas");
            continue;
        }
        let types: usize = graphs.iter().map(|g| g.resources.len()).sum();
        if args.check {
            let cached = store.load(name).map_err(Failure::Message)?;
            let integrity = store.integrity_of(name);
            let locked = lock.as_ref().and_then(|l| l.schemas_for(name));
            let freshness = Freshness::of(cached.as_ref(), integrity.as_deref(), locked);
            let ok = matches!(freshness, Freshness::Current) && cached.is_some();
            println!(
                "{name}: {}",
                if cached.is_none() && locked.is_none() {
                    "not synced"
                } else {
                    freshness.describe()
                }
            );
            if !ok {
                stale += 1;
            }
            continue;
        }
        println!(
            "{name}: asking the tool about {} in {}",
            count(types, "resource"),
            count(graphs.len(), "document")
        );
        let work_dir = store.work_dir(name);
        if let Err(e) = std::fs::create_dir_all(&work_dir) {
            return Err(Failure::Message(format!("{}: {e}", work_dir.display())));
        }
        let work_dir = std::fs::canonicalize(&work_dir).unwrap_or(work_dir);
        let request = meridian_sdk::SchemaRequest::new(work_dir, graphs.clone());
        match target.export_schemas(&request) {
            Ok(export) => {
                let cached = Cached::from_export(name, export);
                for note in &cached.notes {
                    eprintln!("  note: {note}");
                }
                let integrity = store.save(&cached).map_err(Failure::Message)?;
                println!(
                    "{name}: cached {} from {} in {}",
                    count(cached.schemas.len(), "resource type"),
                    cached.versions(),
                    store.path(name).display()
                );
                if let Some(lock) = &mut lock {
                    lock.set_schemas(cached.lock_entry(&integrity));
                } else if args.lock {
                    let mut fresh = meridian_module::Lock::default();
                    fresh.set_schemas(cached.lock_entry(&integrity));
                    lock = Some(fresh);
                }
                synced += 1;
            }
            // A schema export is given no time limit, so it is never stopped;
            // if it ever were, that is a failure of this sync like any other.
            Err(meridian_plugin_host::RunFailure::Tool(e)) => {
                eprintln!("error: {name}: {e}");
                failed += 1;
            }
            Err(meridian_plugin_host::RunFailure::Stopped(e)) => {
                eprintln!("error: {name}: {e}");
                failed += 1;
            }
            Err(meridian_plugin_host::RunFailure::Plugin(e)) => {
                eprintln!("error: {name}: {e}");
                failed += 1;
            }
        }
    }
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    if args.check {
        if stale > 0 {
            eprintln!(
                "error: {} not current; run `meridian schema sync`",
                count(stale, "cache")
            );
            return Err(Failure::Reported);
        }
        return Ok(());
    }
    if args.lock && synced > 0 {
        if let Some(lock) = &lock {
            let path = project.join(meridian_module::LOCK_FILE);
            std::fs::write(&path, lock.render())
                .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;
            println!("recorded in {}", meridian_module::LOCK_FILE);
        }
    }
    if failed > 0 {
        Err(Failure::Reported)
    } else {
        Ok(())
    }
}

fn schema_status(plugins: &PluginDirs, state_dir: &Path, json: bool) -> Result<(), Failure> {
    let registry = registry(plugins)?;
    let store = Store::new(state_dir);
    let lock = schemas::lock_in(Path::new("."));
    let mut names: Vec<String> = registry.names().into_iter().map(str::to_owned).collect();
    for name in store.load_all().keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    if let Some(lock) = &lock {
        for s in &lock.schemas {
            if !names.contains(&s.target) {
                names.push(s.target.clone());
            }
        }
    }
    names.sort();
    let mut out = std::io::stdout().lock();
    for name in &names {
        let exports = registry
            .target(name)
            .is_some_and(|t| t.info().exports_schemas);
        let has_plugin = registry.target(name).is_some();
        let (cached, problem) = match store.load(name) {
            Ok(found) => (found, None),
            Err(e) => (None, Some(e)),
        };
        let integrity = store.integrity_of(name);
        let locked = lock.as_ref().and_then(|l| l.schemas_for(name));
        let freshness = Freshness::of(cached.as_ref(), integrity.as_deref(), locked);
        let state = if let Some(problem) = &problem {
            format!("unreadable: {problem}")
        } else if cached.is_none() && locked.is_none() {
            if exports {
                "not synced".to_owned()
            } else if has_plugin {
                "the plugin does not export its tool's schemas".to_owned()
            } else {
                "no plugin".to_owned()
            }
        } else {
            freshness.describe().to_owned()
        };
        if json {
            let _ = writeln!(
                out,
                "{}",
                serde_json::json!({
                    "target": name,
                    "exportsSchemas": exports,
                    "cached": cached.as_ref().map(|c| serde_json::json!({
                        "tool": c.tool,
                        "toolVersion": c.tool_version,
                        "pins": c.pins,
                        "syncedAt": c.synced_at,
                        "types": c.schemas.len(),
                        "file": store.path(name).display().to_string(),
                    })),
                    "locked": locked.map(|l| serde_json::json!({
                        "tool": l.tool,
                        "toolVersion": l.tool_version,
                        "pins": l.pins,
                        "integrity": l.integrity,
                    })),
                    "state": state,
                })
            );
            continue;
        }
        match &cached {
            Some(c) => {
                let _ = writeln!(
                    out,
                    "{name}: {} from {}, synced {}: {state}",
                    count(c.schemas.len(), "resource type"),
                    c.versions(),
                    c.synced_at
                );
            }
            None => {
                let _ = writeln!(out, "{name}: {state}");
            }
        }
    }
    if names.is_empty() {
        let _ = writeln!(out, "no targets");
    }
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    Ok(())
}

fn schema_clean(targets: &[String], state_dir: &Path) -> Result<(), Failure> {
    let store = Store::new(state_dir);
    let project = Path::new(".");
    let mut lock =
        meridian_module::Lock::load(project).map_err(|e| Failure::Message(e.to_string()))?;
    let mut names: Vec<String> = if targets.is_empty() {
        let mut all: Vec<String> = store.load_all().keys().cloned().collect();
        if let Some(lock) = &lock {
            all.extend(lock.schemas.iter().map(|s| s.target.clone()));
        }
        all
    } else {
        targets.to_vec()
    };
    names.sort();
    names.dedup();
    for name in &names {
        store.remove(name).map_err(Failure::Message)?;
        let _ = std::fs::remove_dir_all(store.work_dir(name));
        if let Some(lock) = &mut lock {
            lock.remove_schemas(name);
        }
        println!("{name}: forgotten");
    }
    if let Some(lock) = &lock {
        let path = project.join(meridian_module::LOCK_FILE);
        if lock.is_empty() {
            // A lockfile that now says nothing is not worth keeping.
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::write(&path, lock.render())
                .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;
        }
    }
    if names.is_empty() {
        println!("nothing is cached");
    }
    Ok(())
}
