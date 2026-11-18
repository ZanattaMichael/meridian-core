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

    let out = match out {
        Some(path) => path.to_path_buf(),
        None => beside(file, to),
    };
    if out.exists() {
        return Err(Failure::Message(format!(
            "{} already exists; remove it or give --out",
            out.display()
        )));
    }
    std::fs::write(&out, &moved.text)
        .map_err(|e| Failure::Message(format!("{}: cannot write file: {e}", out.display())))?;

    let why = match adoption {
        Adoption::Import if no_adopt => {
            Some(format!("--no-adopt: {to} will create every resource again"))
        }
        Adoption::Import => None,
        Adoption::Converge => None,
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

/// `site.yaml` moved to `opentofu` is `site.opentofu.yaml`, beside it.
fn beside(file: &Path, to: &str) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = file
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "yaml".to_owned());
    file.with_file_name(format!("{stem}.{to}.{extension}"))
}
