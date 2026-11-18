//! `meridian rules check`.

use crate::*;

/// `meridian rules check`: loads the layers, checks every rule, and says
/// which layer belongs to whom. Every violation is printed, not only the
/// first, and none of them shows a value.
pub(crate) fn check_rules(
    files: meridian_rules::Files,
    set: &[String],
    data_files: &[PathBuf],
    state_dir: &Path,
) -> Result<(), Failure> {
    // Each --data file is a layer of its own, so a refusal can say which.
    let mut command_line = Vec::new();
    for file in data_files {
        let data = read_data(std::slice::from_ref(file), state_dir)?;
        command_line.push(meridian_rules::Layer::command_line(
            format!("--data {}", file.display()),
            data,
        ));
    }
    // The --set values are what they are on compile: data, merged last. They
    // are also what a layer path's %{KEY} reads.
    let mut values = Data::new();
    apply_sets(&mut values, set)?;
    let scope: meridian_resolve::Scope = set
        .iter()
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    if !values.is_empty() {
        command_line.push(meridian_rules::Layer::set("--set", values));
    }
    let checked = match meridian_rules::check_files(&files, &scope, &command_line) {
        Ok(checked) => checked,
        Err(refusal) => {
            if json_output() {
                // The violations are the result, each as data; a hierarchy
                // that does not load is an error like any other.
                let violations = match &refusal {
                    meridian_rules::Refusal::Violations(found) => {
                        found.iter().map(output::violation).collect()
                    }
                    meridian_rules::Refusal::Error(_) => {
                        return Err(Failure::Message(refusal.to_string()))
                    }
                };
                emit(
                    false,
                    body([
                        ("rulesFile", serde_json::Value::Null),
                        ("rules", serde_json::Value::Null),
                        ("layers", serde_json::json!([])),
                        ("violations", violations),
                    ]),
                );
                eprintln!("error: {refusal}");
                return Err(Failure::Reported);
            }
            return Err(Failure::Message(refusal.to_string()));
        }
    };
    if json_output() {
        let layers: Vec<serde_json::Value> = checked
            .groups
            .iter()
            .map(|(layer, group)| serde_json::json!({ "path": layer, "group": group }))
            .collect();
        emit(
            true,
            body([
                (
                    "rulesFile",
                    checked
                        .rules
                        .as_ref()
                        .map_or(serde_json::Value::Null, |(path, _)| {
                            path.display().to_string().into()
                        }),
                ),
                (
                    "rules",
                    checked
                        .rules
                        .as_ref()
                        .map_or(serde_json::Value::Null, |(_, rules)| {
                            rules.rules.len().into()
                        }),
                ),
                ("layers", layers.into()),
                ("violations", serde_json::json!([])),
            ]),
        );
        return Ok(());
    }
    let Some((path, rules)) = &checked.rules else {
        outln!(
            "no rules file ({} beside {}): nothing was checked",
            meridian_rules::RULES_FILE,
            files.hierarchy.display()
        );
        return Ok(());
    };
    outln!(
        "rules: {} ({})",
        path.display(),
        count(rules.rules.len(), "rule")
    );
    outln!("layers, least specific first:");
    let width = checked
        .groups
        .iter()
        .map(|(p, _)| p.len())
        .max()
        .unwrap_or(0);
    for (layer, group) in &checked.groups {
        if rules.groups.is_empty() {
            outln!("  {layer}");
        } else {
            outln!("  {layer:width$}  {group}");
        }
    }
    outln!("no violations");
    Ok(())
}
