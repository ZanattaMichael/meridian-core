//! `meridian secret`: checking and setting what a document reads.

use crate::*;

/// Stores a value for a declared secret in the first place it names that
/// meridian can write. The value goes from standard input to protected
/// memory to the store, and is in nothing this prints.
pub(crate) fn set_secret(name: &str, files: &[PathBuf], state_dir: &Path) -> Result<(), Failure> {
    let bundle = load(files, state_dir)?;
    let Some(places) = meridian_orchestrator::places_declared(&bundle.documents, name) else {
        let known = meridian_orchestrator::names_declared(&bundle.documents);
        return Err(Failure::Message(if known.is_empty() {
            format!("{name:?} is not declared: these documents declare no secret")
        } else {
            format!("{name:?} is not declared (declared: {})", known.join(", "))
        }));
    };
    if !places.iter().any(|place| place.writable()) {
        let named: Vec<String> = places.iter().map(|place| place.place()).collect();
        return Err(Failure::Message(format!(
            "{name:?} names no place meridian can write. It is looked for in {}. meridian \
             writes the kernel keyring (from: keyring) and the Secret Service (from: \
             secret-service); every other place is written with its own tool",
            named.join(", then in ")
        )));
    }
    let value = meridian_secret::read_from_stdin(&format!("Value for {name} (not shown): "))
        .map_err(|e| Failure::Message(format!("reading the value: {e}")))?;
    if value.is_empty() {
        return Err(Failure::Message(
            "no value was given, so nothing was stored".to_owned(),
        ));
    }
    let at = meridian_secret::store_first(&places, &format!("Meridian: {name}"), &value)
        .map_err(|e| Failure::Message(format!("{name:?} was not stored: {e}")))?;
    let place = &places[at];
    if json_output() {
        emit(
            true,
            body([
                ("name", name.into()),
                ("from", place.from.as_str().into()),
                ("storedIn", place.place().into()),
            ]),
        );
    } else {
        outln!("stored {name} in {}", place.place());
    }
    Ok(())
}

/// Says where each declared secret comes from and whether that place would
/// answer. Nothing is fetched: a file is looked at and not read, a command is
/// found and not run. Fails when a secret that is not optional has no answer.
pub(crate) fn check_secrets(files: &[PathBuf], state_dir: &Path) -> Result<(), Failure> {
    use meridian_orchestrator::check_declared;
    use meridian_secret::Probe;
    let bundle = load(files, state_dir)?;
    let (mut declared, mut unanswered) = (0, 0);
    let mut documents = Vec::new();
    for doc in &bundle.documents {
        let rows = check_declared(doc);
        if rows.is_empty() {
            continue;
        }
        if json_output() {
            let secrets: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    declared += 1;
                    if row.unanswered() {
                        unanswered += 1;
                    }
                    let state = match (&row.probe, row.optional) {
                        (Probe::Present(_), _) => "ok",
                        (Probe::Unknown(_), _) => "not-checked",
                        (Probe::Absent(_), true) => "absent-optional",
                        (Probe::Absent(_), false) => "missing",
                        (Probe::Refused(_), _) => "refused",
                        (Probe::Unavailable(_), _) => "unavailable",
                    };
                    serde_json::json!({
                        "name": row.name,
                        "from": row.from,
                        "optional": row.optional,
                        "state": state,
                        "answered": !row.unanswered(),
                        "detail": row.probe.words(),
                    })
                })
                .collect();
            documents.push(serde_json::json!({
                "name": doc.name(),
                "file": doc.position().file,
                "secrets": secrets,
            }));
            continue;
        }
        outln!("{} ({})", doc.name(), doc.position().file);
        let width = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
        let provider = rows
            .iter()
            .map(|row| row.from.len())
            .max()
            .unwrap_or(0)
            .max(5);
        // Each row is a declaration and what a probe found. No value is
        // read by this command.
        for row in &rows {
            declared += 1;
            let state = match (&row.probe, row.optional) {
                (Probe::Present(_), _) => "ok",
                (Probe::Unknown(_), _) => "not checked",
                (Probe::Absent(_), true) => "absent, optional",
                (Probe::Absent(_), false) => "MISSING",
                (Probe::Refused(_), _) => "REFUSED",
                (Probe::Unavailable(_), _) => "UNAVAILABLE",
            };
            if row.unanswered() {
                unanswered += 1;
            }
            outln!(
                "  {:width$}  {:provider$}  {state}: {}",
                row.name,
                row.from,
                row.probe.words()
            );
        }
    }
    if json_output() {
        emit(
            unanswered == 0,
            body([
                ("documents", documents.into()),
                ("declared", declared.into()),
                ("unanswered", unanswered.into()),
            ]),
        );
    } else if declared == 0 {
        outln!("no secret is declared");
    }
    if unanswered == 0 {
        Ok(())
    } else {
        let message = format!(
            "{} would not be found by plan or apply",
            count(unanswered, "secret")
        );
        if json_output() {
            eprintln!("error: {message}");
            return Err(Failure::Reported);
        }
        Err(Failure::Message(message))
    }
}
