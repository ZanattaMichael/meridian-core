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

use crate::output::{body, emit, json_output};
use crate::schema_bundles::{self, BundleNaming, FromBundles, Resolve};
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

/// Where `sync` takes a target's schemas from first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Prefer {
    /// Signed schema bundles from the registries spec.schemas names, and the
    /// tool only for a target no bundle set describes
    Bundles,
    /// The installed tool, always
    Tool,
}

#[derive(Args)]
pub struct SyncOptions {
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
    /// Leave meridian.lock alone
    #[arg(long)]
    no_lock: bool,
    /// Where to take schemas from first: bundles (from the registries
    /// spec.schemas in meridian.yaml names, falling back to the tool) or
    /// tool. Without a registry named, bundles means the tool
    #[arg(
        long,
        value_enum,
        default_value = "bundles",
        env = "MERIDIAN_SCHEMA_PREFER"
    )]
    prefer: Prefer,
    /// What to do about schema bundle signatures: off, warn or require.
    /// Defaults to spec.schemas.signatures in meridian.yaml, else off
    #[arg(long, value_name = "POLICY", env = "MERIDIAN_SCHEMA_SIGNATURES")]
    schema_signatures: Option<SignaturePolicy>,
}

#[derive(Subcommand)]
pub enum SchemaCommand {
    /// Get the schemas of the resource types the documents use, from signed
    /// schema bundles or by asking each target's installed tool, and cache
    /// them under the state directory for editors, lint and the JSON Schema
    Sync {
        #[command(flatten)]
        options: SyncOptions,
        /// Change nothing; fail if any cache is missing or is not what
        /// meridian.lock records. For CI
        #[arg(long, conflicts_with = "locked")]
        check: bool,
        /// Use only what meridian.lock pins: the bundles it names, or for a
        /// target synced from its tool, the same tool and pins. For CI
        #[arg(long)]
        locked: bool,
        /// Describe every type of the providers, packages or cluster the
        /// documents use, not only the types they write. For building a
        /// schema bundle with `meridian schema pack`; the cache can be large
        #[arg(long)]
        all_types: bool,
    },
    /// Choose the schema bundles again, the newest that describe what the
    /// documents use, and move meridian.lock to them
    Update {
        #[command(flatten)]
        options: SyncOptions,
    },
    /// Pack one target's cache, as `schema sync` wrote it, into a schema
    /// bundle: an archive a registry stores, optionally signed
    Pack {
        /// The target whose cache to pack
        target: String,
        /// The bundle's name within the target, such as hashicorp-aws
        #[arg(long, value_name = "NAME")]
        name: String,
        /// The bundle's version, such as 5.60.0 or 2026-11-18
        #[arg(long, value_name = "VERSION")]
        version: String,
        /// The file to write. Defaults to
        /// `<DIR>/schemas/packed/<target>-<name>-<version>.tar.gz`
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Sign the bundle with this secret key, from `meridian signing
        /// keygen`
        #[arg(long, value_name = "FILE")]
        key: Option<PathBuf>,
        /// Pack only the types whose names start with this, such as
        /// community.general. for one Ansible collection; may be given more
        /// than once
        #[arg(long, value_name = "TEXT")]
        prefix: Vec<String>,
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
    },
    /// Sign a packed schema bundle, writing it again with its signature in
    /// it. For a publisher whose build and signing are separate steps
    Sign {
        /// The file `meridian schema pack` wrote
        file: PathBuf,
        /// The secret key to sign with, from `meridian signing keygen`
        #[arg(long, value_name = "FILE")]
        key: PathBuf,
        /// Where to write the signed bundle. Defaults to the file itself
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Publish a packed schema bundle to a registry, under
    /// `<registry>/<target>/<name>:<version>`, and list it in the
    /// registry's index
    Push {
        /// The file `meridian schema pack` wrote
        file: PathBuf,
        /// The registry: an OCI repository prefix such as
        /// ghcr.io/acme/meridian-schemas (oci:// optional), or file://DIR
        registry: String,
        /// Log in with this credential from spec.credentials in
        /// meridian.yaml, instead of a login from the Docker configuration
        #[arg(long, value_name = "NAME")]
        credential: Option<String>,
        /// Talk plain HTTP. For a registry on a trusted network only
        #[arg(long)]
        insecure: bool,
        /// Publish the bundle and leave the registry's index alone
        #[arg(long)]
        no_index: bool,
    },
    /// Fetch one schema bundle and write the cache from it, as sync would,
    /// without asking any tool
    Fetch {
        /// oci://REGISTRY/<target>/<name>:<version>, or a packed bundle as
        /// file://PATH or a path
        reference: String,
        /// Documents whose types to describe in full, every other type of
        /// the bundle by name. Without any, every type is described in full
        paths: Vec<PathBuf>,
        #[arg(
            long,
            value_name = "DIR",
            env = "MERIDIAN_STATE_DIR",
            default_value = ".meridian"
        )]
        state_dir: PathBuf,
        /// Leave meridian.lock alone
        #[arg(long)]
        no_lock: bool,
        /// What to do about the bundle's signature: off, warn or require.
        /// Defaults to spec.schemas.signatures in meridian.yaml, else off
        #[arg(long, value_name = "POLICY", env = "MERIDIAN_SCHEMA_SIGNATURES")]
        schema_signatures: Option<SignaturePolicy>,
        /// Log in with this credential from spec.credentials in
        /// meridian.yaml. A registry spec.schemas names uses its own
        #[arg(long, value_name = "NAME")]
        credential: Option<String>,
        /// Talk plain HTTP to a registry spec.schemas does not name. For a
        /// registry on a trusted network only
        #[arg(long)]
        insecure: bool,
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

#[derive(Args)]
pub struct McpArgs {
    #[command(flatten)]
    plugins: PluginDirs,
    /// Offer the `plan` tool, which runs the real tool and takes the state
    /// lock. Off by default: the server then changes nothing anywhere
    #[arg(long, env = "MERIDIAN_MCP_ALLOW_PLAN")]
    allow_plan: bool,
    /// The directory file arguments are confined to. Defaults to the
    /// current directory
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
    /// Where `meridian schema sync` cached each tool's own schemas, which
    /// describe native types
    #[arg(
        long,
        value_name = "DIR",
        env = "MERIDIAN_STATE_DIR",
        default_value = ".meridian"
    )]
    state_dir: PathBuf,
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
    // One row per file for the JSON result.
    let mut rows = Vec::new();
    let mut row = |path: &Path, status: &str, message: Option<String>| {
        rows.push(serde_json::json!({
            "file": path.display().to_string(),
            "status": status,
            "message": message,
        }));
    };
    for (path, text) in &files {
        let name = path.to_string_lossy();
        // Only reached for a file named outright: a directory is searched
        // for Meridian documents alone. Other people's YAML is not ours to
        // lay out.
        if !check::is_meridian(text) {
            skipped += 1;
            eprintln!("skipped {name}: not a Meridian document");
            row(path, "skipped", Some("not a Meridian document".to_owned()));
            continue;
        }
        match fmt::format(&name, text, options) {
            Ok(formatted) if formatted == *text => row(path, "ok", None),
            Ok(formatted) => {
                changed += 1;
                if args.check {
                    row(path, "not-formatted", None);
                    if !json_output() {
                        outln!("{name}");
                    }
                } else {
                    std::fs::write(path, formatted)
                        .map_err(|e| Failure::Message(format!("writing {name}: {e}")))?;
                    row(path, "formatted", None);
                    if !json_output() {
                        outln!("formatted {name}");
                    }
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("error: {name}: {e}");
                row(path, "failed", Some(e.to_string()));
            }
        }
    }
    let checked = files.len() - skipped;
    let unchanged = checked - changed - failed;
    if json_output() {
        emit(
            failed == 0 && !(args.check && changed > 0),
            body([
                ("check", args.check.into()),
                ("files", rows.into()),
                (
                    "summary",
                    serde_json::json!({
                        "checked": checked,
                        "changed": changed,
                        "unchanged": unchanged,
                        "failed": failed,
                        "skipped": skipped,
                    }),
                ),
            ]),
        );
    }
    if args.check {
        if changed > 0 {
            eprintln!(
                "{} not formatted; run `meridian fmt` to format",
                count(changed, "file")
            );
        } else if failed == 0 && !json_output() {
            outln!("{} formatted", count(checked, "file"));
        }
    } else if failed == 0 && !json_output() {
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
    let failed = errors > 0 || (args.strict && warnings > 0);
    if json_output() {
        // The whole result as one object; `--json` keeps its line per
        // finding for the tools that already read it.
        emit(
            !failed,
            body([
                (
                    "diagnostics",
                    found.iter().map(to_json).collect::<Vec<_>>().into(),
                ),
                (
                    "summary",
                    serde_json::json!({
                        "errors": errors,
                        "warnings": warnings,
                        "notes": notes,
                        "fixed": fixed,
                        "documents": documents,
                        "files": files.len(),
                        "pluginsFound": !catalog.is_empty(),
                    }),
                ),
            ]),
        );
        return if failed {
            Err(Failure::Reported)
        } else {
            Ok(())
        };
    }
    let mut out = crate::Stdout;
    for d in &found {
        let _ = if args.json {
            writeln!(out, "{}", to_json(d))
        } else {
            writeln!(out, "{d}")
        };
    }
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
            options,
            check,
            locked,
            all_types,
        }) => {
            return schema_sync(&SyncArgs {
                options,
                check: *check,
                resolve: if *locked {
                    Resolve::Locked
                } else {
                    Resolve::Keep
                },
                all_types: *all_types,
            })
        }
        Some(SchemaCommand::Update { options }) => {
            return schema_sync(&SyncArgs {
                options,
                check: false,
                resolve: Resolve::Update,
                all_types: false,
            })
        }
        Some(SchemaCommand::Pack {
            target,
            name,
            version,
            out,
            key,
            prefix,
            state_dir,
        }) => {
            return schema_bundles::pack(
                state_dir,
                target,
                &BundleNaming { name, version },
                prefix,
                out.as_deref(),
                key.as_deref(),
            )
        }
        Some(SchemaCommand::Sign { file, key, out }) => {
            return schema_bundles::sign(file, key, out.as_deref())
        }
        Some(SchemaCommand::Push {
            file,
            registry,
            credential,
            insecure,
            no_index,
        }) => {
            return schema_bundles::push(
                file,
                registry,
                credential.as_deref(),
                *insecure,
                !no_index,
            )
        }
        Some(SchemaCommand::Fetch {
            reference,
            paths,
            state_dir,
            no_lock,
            schema_signatures,
            credential,
            insecure,
        }) => {
            let wanted = if paths.is_empty() {
                None
            } else {
                let files = gather(paths)?;
                let by_target = graphs_by_target(&files, None)?;
                Some(
                    by_target
                        .values()
                        .flatten()
                        .flat_map(|g| g.resources.iter().map(|r| r.r#type.clone()))
                        .collect(),
                )
            };
            return schema_bundles::fetch(
                state_dir,
                reference,
                wanted,
                *schema_signatures,
                !no_lock,
                credential.as_deref(),
                *insecure,
            );
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

/// `meridian mcp`: the compiler and the catalogue as tools for an AI model,
/// over the Model Context Protocol on standard input and output.
pub fn mcp(args: &McpArgs) -> Result<(), Failure> {
    let root = args.root.clone().unwrap_or_else(|| PathBuf::from("."));
    let registry = registry(&args.plugins)?;
    let mut catalog = Catalog::from_registry(&registry);
    let store = Store::new(&root.join(&args.state_dir));
    let mut server_notes = Vec::new();
    for problem in catalog.read_cache(&store, schemas::lock_in(&root).as_ref()) {
        eprintln!("warning: {problem}");
        server_notes.push(format!(
            "A cached tool schema could not be read: {problem}."
        ));
    }
    let mut server = meridian_mcp::Server::new(registry, catalog, &root);
    for note in server_notes {
        server = server.note(note);
    }
    // `diagnose` only reads the record, so it is always offered.
    let program = std::env::current_exe()
        .map_err(|e| Failure::Message(format!("finding the meridian binary: {e}")))?;
    server = server.allow_diagnose(meridian_mcp::PlanCommand {
        program,
        args: vec![
            "--state-dir".to_owned(),
            root.join(&args.state_dir).to_string_lossy().into_owned(),
        ],
    });
    if args.allow_plan {
        // `plan` is the command itself, with the plugin options it was
        // started with, so the result is the object `meridian plan --output
        // json` prints.
        let program = std::env::current_exe()
            .map_err(|e| Failure::Message(format!("finding the meridian binary: {e}")))?;
        let mut plan_args = Vec::new();
        for dir in &args.plugins.dirs {
            plan_args.push("--plugin-dir".to_owned());
            plan_args.push(dir.to_string_lossy().into_owned());
        }
        if let Some(policy) = args.plugins.signatures {
            plan_args.push("--plugin-signatures".to_owned());
            plan_args.push(
                match policy {
                    SignaturePolicy::Off => "off",
                    SignaturePolicy::Warn => "warn",
                    SignaturePolicy::Require => "require",
                }
                .to_owned(),
            );
        }
        server = server.allow_plan(meridian_mcp::PlanCommand {
            program,
            args: plan_args,
        });
    }
    let registry = meridian_mcp::run_stdio(server).map_err(|e| Failure::Message(e.to_string()))?;
    // A plugin that will not exit is not worth failing the client over.
    if let Some(registry) = registry {
        let _ = registry.close();
    }
    Ok(())
}

struct SyncArgs<'a> {
    options: &'a SyncOptions,
    check: bool,
    resolve: Resolve,
    all_types: bool,
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
    let opts = args.options;
    let files = gather(&opts.paths)?;
    if files.is_empty() {
        return Err(Failure::Message(
            "no Meridian documents were found in the paths given".to_owned(),
        ));
    }
    let data = if opts.data.is_empty() && opts.set.is_empty() {
        None
    } else {
        let mut data = read_data(&opts.data, &opts.state_dir)?;
        apply_sets(&mut data, &opts.set)?;
        Some(data)
    };
    let by_target = graphs_by_target(&files, data.as_ref())?;
    let registry: Registry = registry(&opts.plugins)?;
    let catalog = Catalog::from_registry(&registry);
    let store = Store::new(&opts.state_dir);
    let project = Path::new(".");
    let lock_wanted = !opts.no_lock || args.check || args.resolve == Resolve::Locked;
    let mut lock = if lock_wanted {
        meridian_module::Lock::load(project).map_err(|e| Failure::Message(e.to_string()))?
    } else {
        None
    };
    if args.resolve == Resolve::Locked && lock.is_none() {
        return Err(Failure::Message(format!(
            "there is no {} to hold to; run `meridian schema sync` and commit it",
            meridian_module::LOCK_FILE
        )));
    }
    // Bundles are looked for only when asked and when there is somewhere to
    // look: a registry the project names, or a bundle the lock pins.
    let manifest = if args.check || opts.prefer == Prefer::Tool {
        None
    } else {
        schema_bundles::project_manifest(project)?
    };
    let bundle_registries = schema_bundles::registries(project, manifest.as_ref())?;
    let trust = if opts.prefer == Prefer::Bundles && !args.check {
        Some(schema_bundles::verification(
            opts.schema_signatures,
            manifest.as_ref(),
        )?)
    } else {
        None
    };

    let mut failed = 0;
    let mut synced = 0;
    let mut stale = 0;
    let mut wanted: Vec<&String> = by_target.keys().collect();
    if let Some(only) = &opts.target {
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
        wanted.retain(|t| *t == only);
    }
    for name in wanted {
        let graphs = &by_target[name];
        let Some(target) = registry.target(name) else {
            eprintln!(
                "warning: no plugin provides target {name:?}, so its documents cannot be synced"
            );
            continue;
        };
        let types: usize = graphs.iter().map(|g| g.resources.len()).sum();
        if args.check {
            if !target.info().exports_schemas {
                println!("{name}: the plugin does not export its tool's schemas");
                continue;
            }
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

        let locked = lock.as_ref().and_then(|l| l.schemas_for(name)).cloned();
        if let Some(trust) = &trust {
            let pinned = locked.as_ref().is_some_and(|l| !l.bundles.is_empty());
            if !bundle_registries.is_empty() || pinned || args.resolve == Resolve::Locked {
                let needed = schema_bundles::needed_types(graphs, |t| {
                    catalog.target(name).is_some_and(|c| c.plugin_describes(t))
                });
                match schema_bundles::from_bundles(
                    &store,
                    name,
                    &needed,
                    &bundle_registries,
                    locked.as_ref(),
                    args.resolve,
                    trust,
                ) {
                    Ok(FromBundles::Cached(cached, bundles, warnings)) => {
                        for warning in warnings {
                            eprintln!("warning: {warning}");
                        }
                        let integrity = store.save(&cached).map_err(Failure::Message)?;
                        println!(
                            "{name}: cached {} from {} in {}",
                            count(cached.schemas.len(), "resource type"),
                            if cached.bundles.len() == 1 {
                                format!("the schema bundle {}", cached.bundles[0])
                            } else {
                                format!("the schema bundles {}", cached.bundles.join(", "))
                            },
                            store.path(name).display()
                        );
                        if !opts.no_lock {
                            lock.get_or_insert_with(Default::default).set_schemas(
                                schema_bundles::lock_entry(&cached, &integrity, bundles),
                            );
                        }
                        synced += 1;
                        continue;
                    }
                    Ok(FromBundles::UseTool(why)) => {
                        if !target.info().exports_schemas {
                            println!(
                                "{name}: {why}, and the plugin does not export its tool's schemas"
                            );
                            continue;
                        }
                        if !bundle_registries.is_empty() || pinned {
                            println!("{name}: {why}; asking the tool");
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {name}: {e}");
                        failed += 1;
                        continue;
                    }
                }
            }
        }

        if !target.info().exports_schemas {
            // Not a failure: the plugin describes what it describes.
            println!("{name}: the plugin does not export its tool's schemas");
            continue;
        }
        if args.resolve == Resolve::Locked && locked.is_none() {
            eprintln!(
                "error: {name}: {} records nothing for it; run `meridian schema sync` without \
                 --locked and commit the lock",
                meridian_module::LOCK_FILE
            );
            failed += 1;
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
        let mut exported = target.export_schemas(&request);
        if args.all_types {
            // The first answer names every type of the parts in use; asking
            // again for all of them describes each in full.
            if let Ok(first) = &exported {
                let mut all: Vec<String> = request.types.clone();
                all.extend(first.schemas.iter().map(|s| s.r#type.clone()));
                all.sort();
                all.dedup();
                if all.len() > request.types.len() {
                    println!(
                        "{name}: asking the tool again to describe all {} in full",
                        count(all.len(), "type")
                    );
                    let mut again = request.clone();
                    again.types = all;
                    exported = target.export_schemas(&again);
                }
            }
        }
        match exported {
            Ok(export) => {
                let cached = Cached::from_export(name, export);
                for note in &cached.notes {
                    eprintln!("  note: {note}");
                }
                if args.resolve == Resolve::Locked {
                    if let Some(locked) = &locked {
                        if locked.tool_version != cached.tool_version || locked.pins != cached.pins
                        {
                            eprintln!(
                                "error: {name}: the tool answered from {}, and {} pins {}; \
                                 install those versions, or sync without --locked and commit \
                                 the lock",
                                cached.versions(),
                                meridian_module::LOCK_FILE,
                                Cached {
                                    tool: locked.tool.clone(),
                                    tool_version: locked.tool_version.clone(),
                                    pins: locked.pins.clone(),
                                    ..Cached::default()
                                }
                                .versions()
                            );
                            failed += 1;
                            continue;
                        }
                    }
                }
                let integrity = store.save(&cached).map_err(Failure::Message)?;
                println!(
                    "{name}: cached {} from {} in {}",
                    count(cached.schemas.len(), "resource type"),
                    cached.versions(),
                    store.path(name).display()
                );
                if !opts.no_lock {
                    lock.get_or_insert_with(Default::default)
                        .set_schemas(cached.lock_entry(&integrity));
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
    if !opts.no_lock && synced > 0 {
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
                        "bundles": c.bundles,
                    })),
                    "locked": locked.map(|l| serde_json::json!({
                        "tool": l.tool,
                        "toolVersion": l.tool_version,
                        "pins": l.pins,
                        "integrity": l.integrity,
                        "bundles": l.bundles.iter().map(|b| serde_json::json!({
                            "reference": b.reference,
                            "digest": b.digest,
                        })).collect::<Vec<_>>(),
                    })),
                    "state": state,
                })
            );
            continue;
        }
        match &cached {
            Some(c) if !c.bundles.is_empty() => {
                let _ = writeln!(
                    out,
                    "{name}: {} from {}, fetched from the schema {} {}: {state}",
                    count(c.schemas.len(), "resource type"),
                    c.versions(),
                    if c.bundles.len() == 1 {
                        "bundle"
                    } else {
                        "bundles"
                    },
                    c.bundles.join(", ")
                );
            }
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
