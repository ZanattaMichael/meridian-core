//! A saved plan: what `meridian plan --out` writes and `meridian apply
//! --plan` carries out.
//!
//! The file ties a reviewed plan to the apply that follows it. It records,
//! per stage, the hash of the artifact the document compiled to, what the
//! tool said would change, and the tool's own saved plan where the tool has
//! one. An apply from the file compiles the documents again and refuses when
//! any stage no longer compiles to the planned artifact, or when a stage was
//! applied by another run since the plan was taken; then it hands each tool
//! the plan it saved, so what runs is what was reviewed.
//!
//! The file holds no secret value: an artifact never does, a tool's saved
//! plan is kept out of it when it would, and a plan's changes are the tool's
//! one-line summaries.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use meridian_sdk::{canonical_map, decode_map, Change, ChangeAction, SavedPlan, Value};

use crate::{
    time, Decisions, Outcome, Refusal, RunReport, RunState, StageId, StageKind, StageReport,
};

/// The version of the file this build writes and reads.
pub const PLAN_FILE_VERSION: i64 = 1;

/// What `plan --out` was asked, as the apply must be asked the same.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asked {
    /// The documents, as they were typed.
    pub files: Vec<String>,
    /// Every `--data` file, as typed.
    pub data: Vec<String>,
    /// The hash of each `--data` file's bytes, in the order of `data`, as
    /// [`file_hash`] gives it. Never a refusal on its own: a data file can
    /// change where no stage reads it. It names the file when a stage no
    /// longer compiles to what was planned.
    pub data_hashes: Vec<String>,
    /// Every `--set KEY=VALUE`, as typed.
    pub set: Vec<String>,
    /// The selector, as [`crate::Selector`] prints it.
    pub selector: String,
    /// What was typed, for the record; never a secret.
    pub command: String,
}

/// How far the plan got with a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlannedStatus {
    /// The tool previewed the change.
    Planned,
    /// The document compiled, and the target cannot preview.
    Compiled,
    /// The stage waits on an output that did not exist when the plan was
    /// taken, so it was neither compiled nor previewed. An apply from the
    /// plan compiles it then, and cannot check it against the plan.
    Deferred,
    /// The document's own `spec.when` left it out.
    LeftOut,
}

impl PlannedStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlannedStatus::Planned => "planned",
            PlannedStatus::Compiled => "compiled",
            PlannedStatus::Deferred => "deferred",
            PlannedStatus::LeftOut => "left-out",
        }
    }

    fn parse(text: &str) -> Result<PlannedStatus, String> {
        Ok(match text {
            "planned" => PlannedStatus::Planned,
            "compiled" => PlannedStatus::Compiled,
            "deferred" => PlannedStatus::Deferred,
            "left-out" => PlannedStatus::LeftOut,
            other => return Err(format!("a stage has the status {other:?}")),
        })
    }
}

/// One stage of a saved plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedStage {
    pub id: StageId,
    pub target: String,
    pub status: PlannedStatus,
    /// The hash of the artifact the document compiled to; empty when it
    /// was not compiled.
    pub artifact_hash: String,
    /// What the tool said an apply would do.
    pub changes: Vec<Change>,
    /// Whether the tool could not preview, so [`PlannedStage::changes`]
    /// says nothing.
    pub unknown: bool,
    /// The tool's own saved plan, when its plugin saved one. Its `changes`
    /// are [`PlannedStage::changes`], filled in when it is handed back.
    pub saved: Option<SavedPlan>,
    /// The hash of the artifact last applied to this stage when the plan
    /// was taken, by any run, succeeded or failed; empty when none was.
    /// An apply from the plan refuses when it has changed since.
    pub applied_before: String,
    /// What the plan's policy said, when one was asked.
    pub policy: Option<Decisions>,
}

/// A saved plan: every stage of one `meridian plan`, with what it takes to
/// apply exactly that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanFile {
    pub version: i64,
    /// When the plan was taken.
    pub created: String,
    /// The id of the run that took it.
    pub run: String,
    pub asked: Asked,
    pub stages: Vec<PlannedStage>,
    /// The `--data` files whose bytes are not what the plan was taken with,
    /// as the apply found them. Not part of the file: it is filled in by
    /// whoever reads the plan to apply it, and only explains a
    /// `plan-differs` failure.
    pub changed_data: Vec<String>,
}

/// The hash of a file's bytes, `sha256:<hex>`, as a plan records a data file.
pub fn file_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut out = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

impl PlanFile {
    /// The plan a finished `plan` run amounts to. `state` is the run-state
    /// as the plan read it, which says what was last applied to each stage.
    ///
    /// A plan with a stage that failed, or that its policy denied, is not a
    /// plan anyone can apply, so it is an error rather than a file.
    pub fn from_report(
        report: &RunReport,
        state: &RunState,
        asked: Asked,
    ) -> Result<PlanFile, String> {
        if let Some(refusal) = &report.refused {
            return Err(format!("the plan did not run: {refusal}"));
        }
        let mut stages = Vec::with_capacity(report.stages.len());
        for stage in &report.stages {
            stages.push(PlannedStage::from_report(stage, state)?);
        }
        Ok(PlanFile {
            version: PLAN_FILE_VERSION,
            created: time::now(),
            run: report.run_id.clone(),
            asked,
            stages,
            changed_data: Vec::new(),
        })
    }

    /// The planned stage with this id.
    pub fn stage(&self, id: &StageId) -> Option<&PlannedStage> {
        self.stages.iter().find(|stage| stage.id == *id)
    }

    /// Whether any stage leaves an apply something to do. A tool that could
    /// not preview may, and a deferred stage waits on an apply, so both
    /// count.
    pub fn has_changes(&self) -> bool {
        self.stages.iter().any(|stage| match stage.status {
            PlannedStatus::Planned => stage.unknown || !stage.changes.is_empty(),
            PlannedStatus::Deferred => true,
            PlannedStatus::Compiled | PlannedStatus::LeftOut => false,
        })
    }

    /// Writes the file, making the directory it goes in.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let failed =
            |e: std::io::Error| format!("the plan could not be written to {}: {e}", path.display());
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        std::fs::write(path, self.to_json()).map_err(failed)
    }

    /// Reads a file [`PlanFile::write`] wrote.
    pub fn read(path: &Path) -> Result<PlanFile, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("the plan {} could not be read: {e}", path.display()))?;
        PlanFile::from_json(&bytes).map_err(|why| {
            format!(
                "{} is not a plan `meridian plan --out` wrote: {why}",
                path.display()
            )
        })
    }

    pub fn to_json(&self) -> String {
        let mut map: BTreeMap<String, Value> = BTreeMap::new();
        map.insert("meridianPlan".to_owned(), Value::Int(self.version));
        map.insert("created".to_owned(), Value::from(self.created.as_str()));
        map.insert("run".to_owned(), Value::from(self.run.as_str()));
        map.insert("files".to_owned(), strings(&self.asked.files));
        map.insert("data".to_owned(), strings(&self.asked.data));
        map.insert("dataHashes".to_owned(), strings(&self.asked.data_hashes));
        map.insert("set".to_owned(), strings(&self.asked.set));
        map.insert(
            "selector".to_owned(),
            Value::from(self.asked.selector.as_str()),
        );
        map.insert(
            "command".to_owned(),
            Value::from(self.asked.command.as_str()),
        );
        map.insert(
            "stages".to_owned(),
            Value::List(self.stages.iter().map(PlannedStage::to_value).collect()),
        );
        canonical_map(&map) + "\n"
    }

    pub fn from_json(bytes: &[u8]) -> Result<PlanFile, String> {
        let map = decode_map(bytes)?;
        let version = match map.get("meridianPlan") {
            Some(Value::Int(version)) => *version,
            _ => return Err("it has no meridianPlan version".to_owned()),
        };
        if version != PLAN_FILE_VERSION {
            return Err(format!(
                "it is a version {version} plan, and this build reads version {PLAN_FILE_VERSION}"
            ));
        }
        let text = |name: &str| {
            map.get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let list = |name: &str| -> Vec<String> {
            map.get(name)
                .and_then(Value::as_list)
                .unwrap_or(&[])
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        let mut stages = Vec::new();
        for stage in map.get("stages").and_then(Value::as_list).unwrap_or(&[]) {
            stages.push(PlannedStage::from_value(stage)?);
        }
        Ok(PlanFile {
            version,
            created: text("created"),
            run: text("run"),
            asked: Asked {
                files: list("files"),
                data: list("data"),
                data_hashes: list("dataHashes"),
                set: list("set"),
                selector: text("selector"),
                command: text("command"),
            },
            stages,
            changed_data: Vec::new(),
        })
    }
}

impl PlannedStage {
    fn from_report(stage: &StageReport, state: &RunState) -> Result<PlannedStage, String> {
        let (status, changes, unknown, saved) = match &stage.outcome {
            Outcome::Planned(plan) => (
                PlannedStatus::Planned,
                plan.changes.clone(),
                plan.unknown,
                plan.saved.clone(),
            ),
            Outcome::Compiled => (PlannedStatus::Compiled, Vec::new(), false, None),
            Outcome::Deferred { .. } => (PlannedStatus::Deferred, Vec::new(), false, None),
            Outcome::LeftOut { .. } => (PlannedStatus::LeftOut, Vec::new(), false, None),
            Outcome::Failed(why) => {
                return Err(format!(
                    "{} failed, so there is nothing to apply: {why}",
                    stage.id
                ))
            }
            Outcome::NotRun { because_of } => {
                return Err(format!(
                    "{} was not planned because {because_of} failed",
                    stage.id
                ))
            }
            Outcome::Applied { .. } | Outcome::Skipped { .. } | Outcome::Drift(_) => {
                return Err(format!(
                    "{} was not planned; only a plan is saved",
                    stage.id
                ))
            }
        };
        Ok(PlannedStage {
            id: stage.id.clone(),
            target: stage.target.clone(),
            status,
            artifact_hash: stage
                .artifact
                .as_ref()
                .map(crate::artifact_hash)
                .unwrap_or_default(),
            changes,
            unknown,
            saved: saved.map(|saved| SavedPlan {
                data: saved.data,
                changes: Vec::new(),
            }),
            applied_before: state
                .get(&stage.id)
                .map(|record| record.artifact_hash.clone())
                .unwrap_or_default(),
            policy: stage.policy.clone(),
        })
    }

    /// The saved plan as the tool's plugin is handed it: its data with the
    /// planned changes.
    pub fn saved_for_apply(&self) -> Option<SavedPlan> {
        self.saved.as_ref().map(|saved| SavedPlan {
            data: saved.data.clone(),
            changes: self.changes.clone(),
        })
    }

    fn to_value(&self) -> Value {
        let mut map: BTreeMap<String, Value> = BTreeMap::new();
        map.insert("name".to_owned(), Value::from(self.id.name.as_str()));
        map.insert("kind".to_owned(), Value::from(self.id.kind.as_str()));
        map.insert("target".to_owned(), Value::from(self.target.as_str()));
        map.insert("status".to_owned(), Value::from(self.status.as_str()));
        map.insert(
            "artifactHash".to_owned(),
            Value::from(self.artifact_hash.as_str()),
        );
        map.insert(
            "changes".to_owned(),
            Value::List(self.changes.iter().map(change_to_value).collect()),
        );
        map.insert("unknown".to_owned(), Value::Bool(self.unknown));
        map.insert(
            "saved".to_owned(),
            match &self.saved {
                None => Value::Null,
                Some(saved) => Value::from(
                    base64::engine::general_purpose::STANDARD
                        .encode(&saved.data)
                        .as_str(),
                ),
            },
        );
        map.insert(
            "appliedBefore".to_owned(),
            Value::from(self.applied_before.as_str()),
        );
        map.insert(
            "policy".to_owned(),
            match &self.policy {
                None => Value::Null,
                Some(decisions) => {
                    let mut policy: BTreeMap<String, Value> = BTreeMap::new();
                    policy.insert("allowed".to_owned(), Value::Bool(decisions.allowed()));
                    policy.insert("asked".to_owned(), Value::Bool(decisions.asked));
                    policy.insert("deny".to_owned(), strings(&decisions.deny));
                    policy.insert("warn".to_owned(), strings(&decisions.warn));
                    Value::Map(policy)
                }
            },
        );
        Value::Map(map)
    }

    fn from_value(value: &Value) -> Result<PlannedStage, String> {
        let map = value.as_map().ok_or("a stage is not an object")?;
        let text = |name: &str| {
            map.get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let name = text("name");
        if name.is_empty() {
            return Err("a stage has no name".to_owned());
        }
        let kind = match text("kind").as_str() {
            "Infrastructure" => StageKind::Infrastructure,
            "ResourceSet" => StageKind::ResourceSet,
            other => return Err(format!("stage {name:?} has the kind {other:?}")),
        };
        let mut changes = Vec::new();
        for change in map.get("changes").and_then(Value::as_list).unwrap_or(&[]) {
            changes.push(change_from_value(change)?);
        }
        let saved = match map.get("saved") {
            None | Some(Value::Null) => None,
            Some(Value::String(encoded)) => Some(SavedPlan {
                data: base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|e| format!("stage {name:?}'s saved plan is not base64: {e}"))?,
                changes: Vec::new(),
            }),
            Some(_) => return Err(format!("stage {name:?}'s saved plan is not text")),
        };
        let policy = map.get("policy").and_then(Value::as_map).map(|policy| {
            let list = |name: &str| -> Vec<String> {
                policy
                    .get(name)
                    .and_then(Value::as_list)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            };
            Decisions {
                deny: list("deny"),
                warn: list("warn"),
                asked: matches!(policy.get("asked"), Some(Value::Bool(true))),
            }
        });
        Ok(PlannedStage {
            id: StageId { kind, name },
            target: text("target"),
            status: PlannedStatus::parse(&text("status"))?,
            artifact_hash: text("artifactHash"),
            changes,
            unknown: matches!(map.get("unknown"), Some(Value::Bool(true))),
            saved,
            applied_before: text("appliedBefore"),
            policy,
        })
    }
}

/// Checks a plan against the run about to apply it: the same stages, none
/// applied by anyone since. Done before any stage runs, so a plan that does
/// not fit stops the run while there is still nothing to undo.
pub(crate) fn check(plan: &PlanFile, stages: &[StageId], state: &RunState) -> Result<(), Refusal> {
    let refuse = |why: String| Refusal::Plan(format!("{why}; take a new plan"));
    for id in stages {
        let Some(planned) = plan.stage(id) else {
            return Err(refuse(format!(
                "the plan has no stage for {id}: it was taken for other documents, or \
                 another selection, than this run's"
            )));
        };
        let applied_since = state
            .get(id)
            .map(|record| (record.artifact_hash.clone(), record.run.clone()))
            .unwrap_or_default();
        if applied_since.0 != planned.applied_before {
            return Err(refuse(format!(
                "{id} was applied since the plan was taken{}, so the plan no longer says what \
                 would change",
                if applied_since.1.is_empty() {
                    String::new()
                } else {
                    format!(" (by run {})", applied_since.1)
                }
            )));
        }
    }
    for planned in &plan.stages {
        if !stages.contains(&planned.id) {
            return Err(refuse(format!(
                "the plan has a stage this run does not, {}: it was taken for other documents, \
                 or another selection, than this run's",
                planned.id
            )));
        }
    }
    Ok(())
}

fn strings(values: &[String]) -> Value {
    Value::List(values.iter().map(|v| Value::from(v.as_str())).collect())
}

fn change_to_value(change: &Change) -> Value {
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    map.insert("resource".to_owned(), Value::from(change.resource.as_str()));
    map.insert("action".to_owned(), Value::from(change.action.as_str()));
    map.insert("summary".to_owned(), Value::from(change.summary.as_str()));
    Value::Map(map)
}

fn change_from_value(value: &Value) -> Result<Change, String> {
    let map = value.as_map().ok_or("a change is not an object")?;
    let text = |name: &str| {
        map.get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let action = match text("action").as_str() {
        "create" => ChangeAction::Create,
        "update" => ChangeAction::Update,
        "delete" => ChangeAction::Delete,
        "replace" => ChangeAction::Replace,
        "adopt" => ChangeAction::Adopt,
        other => return Err(format!("a change has the action {other:?}")),
    };
    Ok(Change {
        resource: text("resource"),
        action,
        summary: text("summary"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StageRecord;

    fn id(name: &str) -> StageId {
        StageId {
            kind: StageKind::Infrastructure,
            name: name.to_owned(),
        }
    }

    fn plan() -> PlanFile {
        PlanFile {
            version: PLAN_FILE_VERSION,
            created: "2026-11-18T12:00:00Z".to_owned(),
            run: "2026-11-18T12-00-00Z-ab12".to_owned(),
            asked: Asked {
                files: vec!["site.yaml".to_owned()],
                data: vec!["prod.yaml".to_owned()],
                data_hashes: vec![file_hash(b"region: eu\n")],
                set: vec!["region=eu".to_owned()],
                selector: "env=prod".to_owned(),
                command: "plan site.yaml".to_owned(),
            },
            stages: vec![
                PlannedStage {
                    id: id("network"),
                    target: "terraform".to_owned(),
                    status: PlannedStatus::Planned,
                    artifact_hash: "sha256:new".to_owned(),
                    changes: vec![Change {
                        resource: "vm".to_owned(),
                        action: ChangeAction::Create,
                        summary: "a machine".to_owned(),
                    }],
                    unknown: false,
                    saved: Some(SavedPlan::new(b"\x00\x01binary".to_vec())),
                    applied_before: "sha256:old".to_owned(),
                    policy: Some(Decisions {
                        deny: Vec::new(),
                        warn: vec!["late".to_owned()],
                        asked: true,
                    }),
                },
                PlannedStage {
                    id: StageId {
                        kind: StageKind::ResourceSet,
                        name: "web".to_owned(),
                    },
                    target: "ansible".to_owned(),
                    status: PlannedStatus::Deferred,
                    artifact_hash: String::new(),
                    changes: Vec::new(),
                    unknown: false,
                    saved: None,
                    applied_before: String::new(),
                    policy: None,
                },
            ],
            changed_data: Vec::new(),
        }
    }

    #[test]
    fn a_plan_file_reads_back_as_it_was_written() {
        let plan = plan();
        let back = PlanFile::from_json(plan.to_json().as_bytes()).unwrap();
        assert_eq!(back, plan);
        assert!(plan.has_changes());
        assert_eq!(
            plan.stage(&id("network"))
                .unwrap()
                .saved_for_apply()
                .unwrap()
                .changes
                .len(),
            1
        );
    }

    #[test]
    fn a_plan_of_nothing_has_no_changes() {
        let mut plan = plan();
        plan.stages[0].changes.clear();
        plan.stages.pop();
        assert!(!plan.has_changes());
        plan.stages[0].unknown = true;
        assert!(plan.has_changes());
    }

    #[test]
    fn what_is_not_a_plan_says_why() {
        assert!(PlanFile::from_json(b"{}")
            .unwrap_err()
            .contains("meridianPlan"));
        assert!(PlanFile::from_json(b"{\"meridianPlan\":2}")
            .unwrap_err()
            .contains("version 2"));
        let bad = r#"{"meridianPlan":1,"stages":[{"name":"a","kind":"Nope"}]}"#;
        assert!(PlanFile::from_json(bad.as_bytes())
            .unwrap_err()
            .contains("kind"));
    }

    #[test]
    fn the_check_wants_the_same_stages_and_nothing_applied_since() {
        let plan = plan();
        let web = StageId {
            kind: StageKind::ResourceSet,
            name: "web".to_owned(),
        };
        let mut state = RunState::default();
        state.set(
            &id("network"),
            StageRecord {
                target: "terraform".to_owned(),
                artifact_hash: "sha256:old".to_owned(),
                applied: true,
                run: "r1".to_owned(),
                last_good: "sha256:old".to_owned(),
            },
        );
        check(&plan, &[id("network"), web.clone()], &state).unwrap();

        let missing = check(&plan, &[id("network")], &state).unwrap_err();
        assert!(missing
            .to_string()
            .contains("has a stage this run does not"));
        let extra = check(&plan, &[id("network"), web.clone(), id("dns")], &state).unwrap_err();
        assert!(extra.to_string().contains("no stage for"));

        state.set(
            &id("network"),
            StageRecord {
                target: "terraform".to_owned(),
                artifact_hash: "sha256:newer".to_owned(),
                applied: false,
                run: "r2".to_owned(),
                last_good: "sha256:old".to_owned(),
            },
        );
        let moved = check(&plan, &[id("network"), web], &state).unwrap_err();
        assert!(moved
            .to_string()
            .contains("applied since the plan was taken (by run r2)"));
    }
}
