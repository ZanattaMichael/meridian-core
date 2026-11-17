//! What the tooling tests share: a registry of real plugins, compiled in.
#![allow(dead_code)]

use meridian_plugin_host::Registry;
use meridian_tooling::catalog::Catalog;
use meridian_tooling::check::{Diagnostic, Env, Source, Workspace};
use meridian_tooling::syntax::Pos;

/// The plugins that describe their types, plus two that pass theirs
/// through, registered in-process exactly as the CLI's own tests do.
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    for name in [
        "ansible",
        "puppet",
        "openvox",
        "chef",
        "salt",
        "cloud-init",
        "terraform",
        "dsc",
    ] {
        let target = meridian_all_targets::target(name).expect("a shipped target");
        registry.register(target.emitter()).unwrap();
    }
    registry
}

pub fn workspace(files: &[(&str, &str)]) -> Workspace {
    let sources: Vec<Source> = files
        .iter()
        .map(|(name, text)| Source {
            name: (*name).to_owned(),
            text: (*text).to_owned(),
        })
        .collect();
    Workspace::new(&sources)
}

/// Checks one file against the real plugins.
pub fn check(text: &str) -> Vec<Diagnostic> {
    let registry = registry();
    let catalog = Catalog::from_registry(&registry);
    let ws = workspace(&[("doc.yaml", text)]);
    meridian_tooling::check::check(
        &ws,
        &Env {
            registry: Some(&registry),
            catalog: &catalog,
            data: None,
        },
    )
}

/// Splits a text at its `|` marker into the text without it and the
/// marker's position, so a test can show where the cursor is.
pub fn cursor(marked: &str) -> (String, Pos) {
    let at = marked.find('|').expect("the text marks a cursor with |");
    let before = &marked[..at];
    let line = before.matches('\n').count();
    let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1);
    (
        format!("{before}{}", &marked[at + 1..]),
        Pos::new(line, col),
    )
}

/// The text a diagnostic's span covers.
pub fn marked<'a>(text: &'a str, d: &Diagnostic) -> &'a str {
    let line = text.lines().nth(d.span.start.line).unwrap_or("");
    line.get(d.span.start.col..d.span.end.col).unwrap_or("")
}
