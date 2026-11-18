//! Reading documents: `validate`, `migrate`, `version`, and the data and
//! backend the other commands share.

use crate::*;

/// Reads and validates the documents. Every problem is reported, each at its
/// own line, before anything else happens.
pub(crate) fn load(files: &[PathBuf], state_dir: &Path) -> Result<Bundle, Failure> {
    let problems = |errors: meridian_ir::ErrorList| {
        eprintln!("error: {errors}");
        // A document that does not read is this command's whole result,
        // whichever command it is.
        if json_output() {
            emit(
                false,
                body([
                    ("documents", 0.into()),
                    (
                        "diagnostics",
                        errors.iter().map(output::document_error).collect(),
                    ),
                ]),
            );
        }
        // And the pipeline engine, where there is one: each at its line.
        report_problems(&document_problems(&errors));
        Failure::Reported
    };
    let files = in_modules(files, state_dir)?;
    let bundle = meridian_ir::load(&files).map_err(problems)?;
    bundle.validate().map_err(problems)?;
    // Once per run, however many documents: see the versioning policy.
    if let Some(warning) = bundle.deprecation_warning() {
        eprintln!("warning: {warning}");
    }
    Ok(bundle)
}

/// Prints every version stream of this build.
pub(crate) fn version() {
    if json_output() {
        use meridian_ir::Status;
        use meridian_sdk::version::{describe_protocols, supported_protocols};
        let languages: Vec<serde_json::Value> = meridian_ir::VersionTable::builtin()
            .versions()
            .iter()
            .map(|v| {
                let (status, release) = match &v.status {
                    Status::Supported => ("supported", None),
                    Status::Deprecated { since } => ("deprecated", Some(since.to_string())),
                    Status::Removed { in_release } => ("removed", Some(in_release.to_string())),
                };
                serde_json::json!({
                    "name": v.name(),
                    "status": status,
                    "release": release,
                })
            })
            .collect();
        emit(
            true,
            body([
                ("meridian", env!("CARGO_PKG_VERSION").into()),
                ("languages", languages.into()),
                ("pluginSdk", meridian_sdk::VERSION.into()),
                (
                    "pluginProtocol",
                    describe_protocols(&supported_protocols()).into(),
                ),
                ("lockfileFormat", meridian_module::LOCK_VERSION.into()),
            ]),
        );
    } else {
        out!("{}", versions());
    }
}

/// Every version stream of this build, one per line.
pub(crate) fn versions() -> String {
    use meridian_ir::Status;
    use meridian_sdk::version::{describe_protocols, supported_protocols};
    let mut out = format!("meridian {}\n", env!("CARGO_PKG_VERSION"));
    for v in meridian_ir::VersionTable::builtin().versions() {
        let status = match v.status {
            Status::Supported => "supported".to_owned(),
            Status::Deprecated { since } => format!("deprecated since meridian {since}"),
            Status::Removed { in_release } => format!("removed in meridian {in_release}"),
        };
        out.push_str(&format!("  language         {}  {status}\n", v.name()));
    }
    out.push_str(&format!("  plugin sdk       {}\n", meridian_sdk::VERSION));
    out.push_str(&format!(
        "  plugin protocol  {}\n",
        describe_protocols(&supported_protocols())
    ));
    out.push_str(&format!(
        "  lockfile format  {}\n",
        meridian_module::LOCK_VERSION
    ));
    out
}

/// Rewrites each file into the language version asked for. A file is either
/// rewritten whole or left exactly as it was.
pub(crate) fn migrate(files: &[PathBuf], to: Option<&str>, check: bool) -> Result<(), Failure> {
    use meridian_ir::{MigrateError, Migrated, VersionTable};
    let table = VersionTable::builtin();
    let target = match to {
        Some(text) => table
            .check(text)
            .map_err(|e| Failure::Message(format!("--to: {e}")))?,
        None => table
            .current()
            .ok_or_else(|| Failure::Message("this build supports no language version".into()))?,
    };
    let mut failed = false;
    let mut would_change = 0usize;
    // One row per file, for the JSON result: the status word and, when
    // there is one, the message or the decisions.
    let mut rows = Vec::new();
    let mut row = |file: &Path, status: &str, message: Option<String>, decisions: &[String]| {
        rows.push(serde_json::json!({
            "file": file.display().to_string(),
            "status": status,
            "message": message,
            "decisions": decisions,
        }));
    };
    for file in files {
        let name = file.display();
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("error: {name}: cannot read file: {e}");
                row(
                    file,
                    "unreadable",
                    Some(format!("cannot read file: {e}")),
                    &[],
                );
                failed = true;
                continue;
            }
        };
        match meridian_ir::migrate(&table, meridian_ir::STEPS, &text, target.number) {
            Ok(Migrated::Unchanged) => {
                row(file, "current", None, &[]);
                if !json_output() {
                    outln!("{name}: already at {}", target.name());
                }
            }
            Ok(Migrated::Rewritten(_)) if check => {
                would_change += 1;
                row(file, "would-migrate", None, &[]);
                if !json_output() {
                    outln!("{name}: would be migrated to {}", target.name());
                }
            }
            Ok(Migrated::Rewritten(new)) => match replace_file(file, &new) {
                Ok(()) => {
                    row(file, "migrated", None, &[]);
                    if !json_output() {
                        outln!("{name}: migrated to {}", target.name());
                    }
                }
                Err(e) => {
                    eprintln!("error: {name}: cannot write file: {e}");
                    row(
                        file,
                        "unwritable",
                        Some(format!("cannot write file: {e}")),
                        &[],
                    );
                    failed = true;
                }
            },
            Err(MigrateError::Refused(why)) => {
                eprintln!("error: {name}: {why}");
                row(file, "refused", Some(why.to_string()), &[]);
                failed = true;
            }
            Err(MigrateError::NeedsDecisions(decisions)) => {
                eprintln!(
                    "error: {name}: not migrated, because it needs {} only you can make:",
                    count(decisions.len(), "decision")
                );
                for decision in &decisions {
                    eprintln!("  {decision}");
                }
                let decisions: Vec<String> = decisions.iter().map(ToString::to_string).collect();
                row(file, "needs-decisions", None, &decisions);
                failed = true;
            }
        }
    }
    if json_output() {
        emit(
            !failed && would_change == 0,
            body([
                ("to", target.name().into()),
                ("check", check.into()),
                ("files", rows.into()),
            ]),
        );
    }
    if failed {
        return Err(Failure::Reported);
    }
    if would_change > 0 {
        let message = format!(
            "{} would change; run without --check to migrate",
            count(would_change, "file")
        );
        if json_output() {
            eprintln!("error: {message}");
            return Err(Failure::Reported);
        }
        return Err(Failure::Message(message));
    }
    Ok(())
}

/// Replaces a file's content by writing beside it and renaming, so a crash
/// leaves the old file or the new one and never half of either.
pub(crate) fn replace_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".meridian-tmp");
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, content)?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// Reads the `--data` files into one map. Each file is a mapping; a key in a
/// later file replaces the same key from an earlier one, whole.
pub(crate) fn read_data(files: &[PathBuf], state_dir: &Path) -> Result<Data, Failure> {
    let mut data = Data::new();
    for file in &in_modules(files, state_dir)? {
        let name = file.to_string_lossy();
        let raw = std::fs::read(file)
            .map_err(|e| Failure::Message(format!("reading the data file {name}: {e}")))?;
        match meridian_ir::parse_value(&name, &raw) {
            Ok(meridian_ir::Value::Map(map)) => data.extend(map),
            Ok(meridian_ir::Value::Null) => {}
            Ok(other) => {
                return Err(Failure::Message(format!(
                    "the data file {name} must hold a mapping of names to values, and it \
                     holds {}",
                    other.describe()
                )))
            }
            Err(e) => return Err(Failure::Message(e.to_string())),
        }
    }
    Ok(data)
}

/// Applies the `--set key=value` options to the data, in the order given.
/// A dotted key sets a value inside a mapping, creating the mappings on the
/// way; the other keys of those mappings are kept.
pub(crate) fn apply_sets(data: &mut Data, sets: &[String]) -> Result<(), Failure> {
    use meridian_ir::Value;
    for set in sets {
        let refuse = |why: &str| Failure::Message(format!("--set {set}: {why}"));
        let Some((key, raw)) = set.split_once('=') else {
            return Err(refuse("expected KEY=VALUE, as in --set env=prod"));
        };
        let path: Vec<&str> = key.split('.').collect();
        if path.iter().any(|part| part.trim().is_empty()) {
            return Err(refuse(
                "the key is empty, or has an empty part between dots",
            ));
        }
        // The value is one YAML scalar, list or mapping. Text that YAML
        // cannot read as a value is taken as the text it is.
        let value = match meridian_ir::parse_value("--set", raw.as_bytes()) {
            Ok(value) => value,
            Err(_) => Value::String(raw.to_owned()),
        };
        let mut holder = &mut *data;
        for (i, part) in path[..path.len() - 1].iter().enumerate() {
            let entry = holder
                .entry((*part).to_owned())
                .or_insert_with(|| Value::Map(Data::new()));
            holder = match entry {
                Value::Map(inner) => inner,
                other => {
                    return Err(refuse(&format!(
                        "{} is already set to {}, so it cannot hold the key {:?}",
                        path[..=i].join("."),
                        other.describe(),
                        path[i + 1]
                    )))
                }
            };
        }
        holder.insert(path[path.len() - 1].to_owned(), value);
    }
    Ok(())
}

/// Where Meridian's record is kept, as the command line chose it.
pub(crate) fn backend(state: &State) -> Result<Arc<dyn Backend>, Failure> {
    let mut settings = BTreeMap::new();
    for setting in &state.backend_config {
        let Some((key, value)) = setting.split_once('=').filter(|(key, _)| !key.is_empty()) else {
            return Err(Failure::Message(format!(
                "--backend-config takes key=value, and was given {setting:?}"
            )));
        };
        settings.insert(key.to_owned(), value.to_owned());
    }
    if state.backend == "local" {
        if let Some(key) = settings.keys().next() {
            return Err(Failure::Message(format!(
                "the local backend takes no settings, and was given {key:?}; it keeps the \
                 record in --state-dir"
            )));
        }
        return Ok(Arc::new(LocalBackend::new(&state.state_dir)));
    }
    ExecBackend::find(&state.backend, &plugin_dirs(&state.plugins), settings)
        .map(|found| Arc::new(found) as Arc<dyn Backend>)
        .map_err(Failure::Message)
}
