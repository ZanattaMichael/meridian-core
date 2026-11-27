//! `meridian retarget`: moving a document from one tool to another without
//! creating what already exists again.

use crate::*;
use meridian_ir::Document;
use meridian_orchestrator::StageKind;
use meridian_sdk::Adoption;
use serde_json::Value as Json;

pub(crate) fn retarget(
    file: &Path,
    to: &str,
    out: Option<&Path>,
    no_adopt: bool,
    state_dir: &Path,
    plugins: &PluginDirs,
) -> Result<(), Failure> {
    let name = file.display().to_string();
    let text = std::fs::read_to_string(file)
        .map_err(|e| Failure::Message(format!("{name}: cannot read file: {e}")))?;
    // Only a document that reads is moved.
    let bundle = load(&[file.to_path_buf()], state_dir)?;
    let registry = registry(plugins)?;
    let Some(new) = registry.target(to) else {
        return Err(Failure::Message(format!(
            "no plugin is installed for the target {to:?}; `meridian plugins` lists them"
        )));
    };
    let adoption = new.info().capabilities.adopt;
    let adopt = adoption == Adoption::Import && !no_adopt;

    // What the old tool manages, by document.
    let mut identifiers = BTreeMap::new();
    if adopt {
        for document in &bundle.documents {
            if document.target() == to {
                continue;
            }
            let id = StageId {
                kind: match document {
                    Document::Infrastructure(_) => StageKind::Infrastructure,
                    Document::ResourceSet(_) => StageKind::ResourceSet,
                },
                name: document.name().to_owned(),
            };
            let found =
                meridian_orchestrator::identifiers(&registry, state_dir, &id, document.target())
                    .map_err(|e| {
                        Failure::Message(format!(
                            "{e}\n\nGive --no-adopt to move the document without `adopt:` lines; \
                             {to} would then create every resource again"
                        ))
                    })?;
            identifiers.insert(id.name, found);
        }
    }

    let moved = meridian_ir::retarget(
        &name,
        &text,
        &meridian_ir::Retarget {
            to,
            adopt,
            identifiers: &identifiers,
        },
    )
    .map_err(|e| Failure::Message(format!("{name}: {e}")))?;

    // `--out`, else the project's `spec.retarget.out`, else beside the file.
    let (out, out_from) = match out {
        Some(path) => (path.to_path_buf(), "cli"),
        None => match manifest_pattern()? {
            Some(pattern) => (from_pattern(file, to, &pattern), "manifest"),
            None => (from_pattern(file, to, DEFAULT_PATTERN), "default"),
        },
    };
    if out.exists() {
        return Err(Failure::Message(format!(
            "{} already exists; remove it or give --out",
            out.display()
        )));
    }
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| {
            Failure::Message(format!(
                "{}: cannot create directory: {e}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(&out, &moved.text)
        .map_err(|e| Failure::Message(format!("{}: cannot write file: {e}", out.display())))?;

    let why = match adoption {
        Adoption::Import if no_adopt => {
            Some(format!("--no-adopt: {to} will create every resource again"))
        }
        Adoption::Import => None,
        Adoption::Converge => None,
        Adoption::Replace => Some(format!(
            "{to} takes over only by replacing: its first apply removes and makes each resource \
             again"
        )),
        Adoption::None => Some(format!(
            "{to} cannot adopt what exists: its first apply creates every resource again"
        )),
    };
    if json_output() {
        let documents: Vec<Json> = moved
            .moved
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "from": m.from,
                    "adopted": m.adopted,
                    "notes": m.notes,
                })
            })
            .collect();
        emit(
            true,
            body([
                ("file", name.clone().into()),
                ("out", out.display().to_string().into()),
                ("outFrom", out_from.into()),
                ("to", to.into()),
                ("adoption", adoption.as_str().into()),
                ("documents", documents.into()),
                ("warning", why.map_or(Json::Null, Json::from)),
            ]),
        );
        return Ok(());
    }
    for m in &moved.moved {
        let adopted = match adoption {
            Adoption::Import if !no_adopt => {
                format!(", {} adopted", count(m.adopted.len(), "resource"))
            }
            Adoption::Converge => ", which converges on what exists".to_owned(),
            Adoption::Replace => ", which replaces what exists".to_owned(),
            _ => String::new(),
        };
        outln!("{}: {} -> {to}{adopted}", m.name, m.from);
        for note in &m.notes {
            eprintln!("note: {}: {note}", m.name);
        }
    }
    if let Some(why) = why {
        eprintln!("warning: {why}");
    }
    outln!(
        "wrote {}; check it with `meridian plan {}`",
        out.display(),
        out.display()
    );
    Ok(())
}

/// Where a moved file goes when neither `--out` nor the project says.
const DEFAULT_PATTERN: &str = "{name}.{target}.{ext}";

/// The project's `spec.retarget.out`, from `meridian.yaml` in the current
/// directory, when it has one.
fn manifest_pattern() -> Result<Option<String>, Failure> {
    let dir = Path::new(".");
    if !dir.join(meridian_module::MANIFEST_FILE).is_file() {
        return Ok(None);
    }
    meridian_module::Manifest::load(dir)
        .map(|manifest| manifest.retarget_out)
        .map_err(module_failure)
}

/// A pattern of `{name}`, `{target}` and `{ext}`, relative to the file moved:
/// `site.yaml` moved to `opentofu` by the default is `site.opentofu.yaml`,
/// beside it.
fn from_pattern(file: &Path, to: &str, pattern: &str) -> PathBuf {
    let name = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "yaml".to_owned());
    let relative = pattern
        .replace("{name}", &name)
        .replace("{target}", to)
        .replace("{ext}", &ext);
    file.parent().unwrap_or(Path::new("")).join(relative)
}
