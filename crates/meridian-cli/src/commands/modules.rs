//! `meridian module`: install, verify, sign, pack and push.

use crate::*;

/// The project in the current directory, as the module commands see it.
pub(crate) fn project(state_dir: &Path) -> meridian_module::Project<'_> {
    meridian_module::Project {
        dir: Path::new("."),
        state_dir,
        core_version: env!("CARGO_PKG_VERSION"),
        signatures: MODULE_SIGNATURES
            .get()
            .unwrap_or(&meridian_module::NO_SIGNATURES),
    }
}

/// Says, once a run, which modules are in use although their signature did
/// not pass. Only the warn policy lets that happen.
pub(crate) fn warn_about_modules(warnings: &[String]) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if !warnings.is_empty() && !SAID.swap(true, Ordering::Relaxed) {
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
    }
}

pub(crate) fn module_failure(e: meridian_module::Error) -> Failure {
    Failure::Message(e.to_string())
}

/// Turns each file written `@<module>:<path>` into the file it names in a
/// pinned module. Nothing is fetched: a module that is not installed, or
/// does not match its pin, stops the command here.
pub(crate) fn in_modules(files: &[PathBuf], state_dir: &Path) -> Result<Vec<PathBuf>, Failure> {
    use meridian_module::Modules;
    let is_reference = |file: &PathBuf| file.to_str().is_some_and(Modules::is_reference);
    if !files.iter().any(is_reference) {
        return Ok(files.to_vec());
    }
    let modules = Modules::load(&project(state_dir)).map_err(module_failure)?;
    warn_about_modules(modules.warnings());
    files
        .iter()
        .map(|file| match file.to_str() {
            Some(text) if Modules::is_reference(text) => {
                modules.resolve(text).map_err(module_failure)
            }
            _ => Ok(file.clone()),
        })
        .collect()
}

pub(crate) fn module(command: ModuleCommand, state_dir: &Path) -> Result<(), Failure> {
    use meridian_module::{Change, Mode, LOCK_FILE};
    let project = project(state_dir);
    let named = |m: &meridian_module::Locked| {
        if m.version.is_empty() {
            m.name.clone()
        } else {
            format!("{} {}", m.name, m.version)
        }
    };
    let mode = match command {
        ModuleCommand::Install { locked: true } => Mode::Locked,
        ModuleCommand::Install { locked: false } => Mode::Install,
        ModuleCommand::Update { names } => Mode::Update(names),
        ModuleCommand::Verify => {
            let checks = meridian_module::verify(&project).map_err(module_failure)?;
            if checks.is_empty() {
                outln!("no modules are pinned");
            }
            let mut failed = 0;
            for check in &checks {
                match &check.problem {
                    None => match &check.signature {
                        Signature::NotChecked => outln!("ok      {}", named(&check.module)),
                        Signature::Verified { key_id } => {
                            outln!("ok      {}  signed by key {key_id}", named(&check.module))
                        }
                        Signature::Unsigned => {
                            outln!("ok      {}  not signed", named(&check.module))
                        }
                        Signature::Invalid(why) => outln!(
                            "ok      {}  signature does not pass: {why}",
                            named(&check.module)
                        ),
                    },
                    Some(problem) => {
                        failed += 1;
                        outln!("FAILED  {}", named(&check.module));
                        for line in problem.lines() {
                            outln!("    {line}");
                        }
                    }
                }
            }
            return if failed == 0 {
                Ok(())
            } else {
                Err(Failure::Message(format!(
                    "{} failed verification",
                    count(failed, "module")
                )))
            };
        }
        ModuleCommand::List => {
            let lock = meridian_module::Lock::load(project.dir)
                .map_err(module_failure)?
                .unwrap_or_default();
            if lock.modules.is_empty() {
                outln!("no modules are pinned");
            }
            let width = |f: fn(&meridian_module::Locked) -> usize| {
                lock.modules.iter().map(f).max().unwrap_or(0)
            };
            let (names, versions) = (width(|m| m.name.len()), width(|m| m.version.len()));
            for m in &lock.modules {
                let version = if m.version.is_empty() {
                    "-"
                } else {
                    &m.version
                };
                outln!("{:<names$}  {version:<versions$}  {}", m.name, m.source);
            }
            return Ok(());
        }
        ModuleCommand::Sign { key } => {
            let key = SecretKey::read(&key).map_err(|e| Failure::Message(e.into()))?;
            let signed = meridian_module::sign(project.dir, &key).map_err(module_failure)?;
            outln!(
                "signed {}{} with key {}",
                signed.name,
                if signed.version.is_empty() {
                    String::new()
                } else {
                    format!(" {}", signed.version)
                },
                signed.key_id
            );
            outln!("  integrity: {}", signed.integrity);
            outln!("  wrote:     {}", meridian_module::SIGNATURE_FILE);
            return Ok(());
        }
        ModuleCommand::Pack { out } => return pack_module(&project, out),
        ModuleCommand::Push {
            repository,
            insecure,
            credential,
        } => return push_module(&project, &repository, insecure, credential.as_deref()),
    };

    let report = meridian_module::install(&project, mode).map_err(module_failure)?;
    warn_about_modules(&report.warnings());
    let mut unchanged = 0;
    for (m, change) in &report.changes {
        match change {
            Change::Added => outln!("+ {} ({})", named(m), m.source),
            Change::Updated { from } if *from != m.version && !m.version.is_empty() => {
                outln!("~ {} {from} -> {}", m.name, m.version)
            }
            Change::Updated { from } => outln!("~ {} (was {from})", named(m)),
            Change::Removed => outln!("- {}", named(m)),
            Change::Unchanged => unchanged += 1,
        }
    }
    if unchanged > 0 {
        outln!("{} unchanged", count(unchanged, "module"));
    }
    if report.wrote_lock {
        outln!("wrote {LOCK_FILE}");
    } else if report.lock.modules.is_empty() {
        outln!("the project requires no modules");
    } else {
        outln!("{LOCK_FILE} is up to date");
    }
    Ok(())
}

pub(crate) fn pack_module(
    project: &meridian_module::Project<'_>,
    out: Option<PathBuf>,
) -> Result<(), Failure> {
    let manifest = meridian_module::Manifest::load(project.dir).map_err(module_failure)?;
    let Some(version) = &manifest.version else {
        return Err(Failure::Message(format!(
            "{} has no metadata.version, and a module is published as a version",
            manifest.name
        )));
    };
    let archive = meridian_module::pack(project.dir).map_err(module_failure)?;
    let integrity = meridian_module::hash_dir(project.dir).map_err(module_failure)?;
    let out = out.unwrap_or_else(|| {
        project.state_dir.join("packages").join(format!(
            "{}-{version}.tar.gz",
            manifest.name.replace('/', "-")
        ))
    });
    let written = (|| {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &archive)
    })();
    if let Err(e) = written {
        return Err(Failure::Message(format!("writing {}: {e}", out.display())));
    }
    outln!("packed {} {version} into {}", manifest.name, out.display());
    outln!("  size:       {} bytes", archive.len());
    outln!("  integrity:  {integrity}");
    outln!("  media type: {}", meridian_module::LAYER_MEDIA_TYPE);
    Ok(())
}

pub(crate) fn push_module(
    project: &meridian_module::Project<'_>,
    repository: &str,
    insecure: bool,
    credential: Option<&str>,
) -> Result<(), Failure> {
    let manifest = meridian_module::Manifest::load(project.dir).map_err(module_failure)?;
    let login = credential
        .map(|name| meridian_module::resolve_login(&manifest, name))
        .transpose()
        .map_err(module_failure)?;
    let Some(version) = &manifest.version else {
        return Err(Failure::Message(format!(
            "{} has no metadata.version, and a module is published as a version",
            manifest.name
        )));
    };
    let archive = meridian_module::pack(project.dir).map_err(module_failure)?;
    let pushed = meridian_module::push(
        repository,
        insecure,
        login.as_ref(),
        &manifest.name,
        version,
        &manifest.description,
        &archive,
    )
    .map_err(module_failure)?;
    if pushed.already_published {
        outln!(
            "{} {version} is already published in {repository}, with this content",
            manifest.name
        );
    } else {
        outln!(
            "published {} {version} to {repository}:{}",
            manifest.name,
            pushed.tag
        );
    }
    outln!("  digest:     {}", pushed.digest);
    let integrity = meridian_module::hash_dir(project.dir).map_err(module_failure)?;
    outln!("  integrity:  {integrity}");
    Ok(())
}
