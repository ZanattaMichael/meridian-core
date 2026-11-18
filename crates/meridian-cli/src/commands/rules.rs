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
    let checked = meridian_rules::check_files(&files, &scope, &command_line)
        .map_err(|refusal| Failure::Message(refusal.to_string()))?;
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
