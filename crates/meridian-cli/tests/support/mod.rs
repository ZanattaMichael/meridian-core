//! What the conformance tests share: every shipped target as an emitter, and
//! the segments they belong to.

#![allow(dead_code)]

use meridian_sdk::{Category, CompileError, Data, Emitted, Emitter};

/// Every target the workspace ships, sorted by name.
pub fn emitters() -> Vec<Box<dyn Emitter>> {
    meridian_all_targets::emitters()
        .into_iter()
        .map(|e| e as Box<dyn Emitter>)
        .collect()
}

/// One target, by name.
pub fn emitter(name: &str) -> Box<dyn Emitter> {
    emitters()
        .into_iter()
        .find(|e| e.name() == name)
        .unwrap_or_else(|| panic!("no target is called {name}"))
}

/// The targets that share the six configuration management resource types.
pub const SHARED_DIALECT: &[&str] = &["ansible", "chef", "cloud-init", "openvox", "puppet", "salt"];

/// The targets of one segment.
pub fn segment(category: Category) -> Vec<Box<dyn Emitter>> {
    emitters()
        .into_iter()
        .filter(|e| e.category() == category)
        .collect()
}

/// How a compile came out, as one cell of a table: `ok`, `ok, warns`, or the
/// rule that refused it.
pub fn outcome(result: &Result<Emitted, CompileError>) -> String {
    match result {
        Ok(emitted) if emitted.warnings.is_empty() => "ok".to_owned(),
        Ok(_) => "ok, warns".to_owned(),
        Err(refusal) => format!("`{}`", refusal.rule),
    }
}

/// No data: what a command is given when it is run without `--data`.
pub fn no_data() -> Data {
    Data::new()
}
