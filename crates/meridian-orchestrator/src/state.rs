//! The orchestrator's own run-state: which document was last applied, from
//! which artifact, by which run, and whether it worked.
//!
//! It exists because the tools disagree about state. Terraform keeps its own
//! and knows nothing of what was configured afterwards; Ansible keeps none.
//! This file records only what Meridian itself did. It is never the source of
//! truth about infrastructure, and nothing in it is secret: outputs are not
//! stored here, they are read back from the tool that owns them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use meridian_sdk::{canonical_map, decode_map, Artifact, MeridianRun, Value};
use sha2::{Digest, Sha256};

use crate::backend::Backend;
use crate::{StageId, StageKind};

/// The key the run-state is kept under.
pub const STATE_KEY: &str = "state.json";

/// What happened the last time a stage was applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageRecord {
    pub target: String,
    /// The hash of the artifact that was applied, from [`artifact_hash`].
    pub artifact_hash: String,
    /// Whether the apply succeeded.
    pub applied: bool,
    /// The run that applied it; empty in a record written before runs had
    /// ids.
    pub run: String,
    /// The hash of the artifact of the last apply that succeeded. It equals
    /// `artifact_hash` after a success, and after a failure it is what a
    /// rollback goes back to. Empty when no apply ever succeeded.
    pub last_good: String,
    /// Meridian-run actions an apply triggered and that have not run yet;
    /// the next apply of the stage runs them (decision 0055).
    pub pending: Vec<MeridianRun>,
}

/// Every stage's last apply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunState {
    stages: BTreeMap<String, StageRecord>,
}

/// The name a stage has in the run-state and in a directory: its kind, in
/// lower case, and its document's name.
pub fn stage_key(id: &StageId) -> String {
    let kind = match id.kind {
        StageKind::Infrastructure => "infrastructure",
        StageKind::ResourceSet => "resourceset",
    };
    format!("{kind}/{}", id.name)
}

impl RunState {
    /// Reads the file. A missing or unreadable one is an empty state: the
    /// file is a record, and losing it loses history, not infrastructure.
    pub fn load(path: &Path) -> RunState {
        std::fs::read(path)
            .map(|bytes| RunState::from_json(&bytes))
            .unwrap_or_default()
    }

    /// Reads the run-state from a store. Unlike [`RunState::load`], a store
    /// that cannot be asked is an error: a run that cannot read the state
    /// should not go on to change it.
    pub fn read(backend: &dyn Backend) -> Result<RunState, String> {
        Ok(backend
            .get(STATE_KEY)?
            .map(|bytes| RunState::from_json(&bytes))
            .unwrap_or_default())
    }

    /// Reads what [`RunState::to_json`] wrote. Anything unreadable is left
    /// out.
    pub fn from_json(bytes: &[u8]) -> RunState {
        let Ok(map) = decode_map(bytes) else {
            return RunState::default();
        };
        let mut state = RunState::default();
        for (key, value) in map {
            let Value::Map(fields) = value else { continue };
            let text = |name: &str| match fields.get(name) {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let applied = text("status") == "applied";
            let artifact_hash = text("artifactHash");
            let mut last_good = text("lastGood");
            // A record written before `lastGood` existed says the same thing
            // with its status.
            if last_good.is_empty() && applied && !fields.contains_key("lastGood") {
                last_good = artifact_hash.clone();
            }
            let pending = match fields.get("pending") {
                Some(Value::List(items)) => items
                    .iter()
                    .filter_map(|item| match item {
                        Value::Map(m) => {
                            let field = |name: &str| match m.get(name) {
                                Some(Value::String(s)) => s.clone(),
                                _ => String::new(),
                            };
                            Some(MeridianRun {
                                notifier: field("notifier"),
                                resource: field("resource"),
                                action: field("action"),
                            })
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            state.stages.insert(
                key,
                StageRecord {
                    target: text("target"),
                    artifact_hash,
                    applied,
                    run: text("run"),
                    last_good,
                    pending,
                },
            );
        }
        state
    }

    pub fn get(&self, id: &StageId) -> Option<&StageRecord> {
        self.stages.get(&stage_key(id))
    }

    pub fn set(&mut self, id: &StageId, record: StageRecord) {
        self.stages.insert(stage_key(id), record);
    }

    /// Forgets a stage, as after a destroy: it reads as never applied.
    pub fn remove(&mut self, id: &StageId) {
        self.stages.remove(&stage_key(id));
    }

    pub fn to_json(&self) -> String {
        let map: BTreeMap<String, Value> = self
            .stages
            .iter()
            .map(|(key, record)| {
                let mut fields: BTreeMap<String, Value> = [
                    ("target", record.target.as_str()),
                    ("artifactHash", record.artifact_hash.as_str()),
                    ("status", if record.applied { "applied" } else { "failed" }),
                    ("run", record.run.as_str()),
                    ("lastGood", record.last_good.as_str()),
                ]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), Value::from(value)))
                .collect();
                if !record.pending.is_empty() {
                    let pending = record
                        .pending
                        .iter()
                        .map(|p| {
                            Value::Map(
                                [
                                    ("notifier", &p.notifier),
                                    ("resource", &p.resource),
                                    ("action", &p.action),
                                ]
                                .into_iter()
                                .map(|(k, v)| (k.to_owned(), Value::from(v.as_str())))
                                .collect(),
                            )
                        })
                        .collect();
                    fields.insert("pending".to_owned(), Value::List(pending));
                }
                (key.clone(), Value::Map(fields))
            })
            .collect();
        canonical_map(&map) + "\n"
    }

    /// Writes the run-state to a store, replacing it whole.
    pub fn write(&self, backend: &dyn Backend) -> Result<(), String> {
        backend.put(STATE_KEY, self.to_json().as_bytes())
    }
}

/// A stable fingerprint of an artifact: the same files with the same content
/// always hash the same, whatever order they were produced in.
pub fn artifact_hash(artifact: &Artifact) -> String {
    let mut hasher = Sha256::new();
    for (path, content) in &artifact.files {
        // Lengths go in, so no two different sets of files can run together
        // into the same bytes.
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(content.as_bytes());
    }
    let mut out = String::from("sha256:");
    for byte in hasher.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemoryBackend;

    fn id(name: &str) -> StageId {
        StageId {
            kind: StageKind::Infrastructure,
            name: name.to_owned(),
        }
    }

    #[test]
    fn the_run_state_reads_back_from_a_store() {
        let backend = MemoryBackend::new();
        assert_eq!(RunState::read(&backend).unwrap(), RunState::default());
        let mut state = RunState::default();
        state.set(
            &id("network"),
            StageRecord {
                target: "terraform".to_owned(),
                artifact_hash: "sha256:new".to_owned(),
                applied: false,
                run: "2026-11-17T09-14-03Z-7f3a".to_owned(),
                last_good: "sha256:old".to_owned(),
                pending: vec![MeridianRun {
                    notifier: "settings".to_owned(),
                    resource: "web".to_owned(),
                    action: "restart".to_owned(),
                }],
            },
        );
        state.write(&backend).unwrap();
        assert_eq!(RunState::read(&backend).unwrap(), state);
    }

    #[test]
    fn a_record_from_before_runs_had_ids_still_reads() {
        let old = br#"{"infrastructure/network":{"artifactHash":"sha256:a","status":"applied","target":"terraform"},
                       "resourceset/web":{"artifactHash":"sha256:b","status":"failed","target":"ansible"}}"#;
        let state = RunState::from_json(old);
        let network = state.get(&id("network")).unwrap();
        assert!(network.applied);
        assert_eq!(network.last_good, "sha256:a");
        assert_eq!(network.run, "");
        let web = state
            .get(&StageId {
                kind: StageKind::ResourceSet,
                name: "web".to_owned(),
            })
            .unwrap();
        assert!(!web.applied);
        assert_eq!(web.last_good, "");
    }
}
