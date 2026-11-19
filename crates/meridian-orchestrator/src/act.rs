//! Meridian-run notifies (decision 0055): which actions a stage's changes
//! trigger, and running them after the stage applies.

use meridian_sdk::notify::triggers;
use meridian_sdk::MeridianRun;

use super::*;

/// What became of one Meridian-run notify in a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionStatus {
    /// A plan: the notifier changes, so an apply runs the action.
    Would,
    /// A plan whose tool cannot preview: an apply may run the action.
    May,
    /// An apply ran it.
    Done,
    /// An apply ran it and it failed; it stays pending.
    Failed,
    /// Triggered and not run: the apply failed or was stopped first, or a
    /// saved plan did not list it. The next apply of the stage runs it.
    Pending,
}

impl ActionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionStatus::Would => "would",
            ActionStatus::May => "may",
            ActionStatus::Done => "done",
            ActionStatus::Failed => "failed",
            ActionStatus::Pending => "pending",
        }
    }

    pub fn parse(word: &str) -> Option<ActionStatus> {
        [
            ActionStatus::Would,
            ActionStatus::May,
            ActionStatus::Done,
            ActionStatus::Failed,
            ActionStatus::Pending,
        ]
        .into_iter()
        .find(|s| s.as_str() == word)
    }
}

/// One Meridian-run action of a stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionReport {
    /// The resource whose change triggered it; empty for one left pending
    /// by an earlier run.
    pub notifier: String,
    pub resource: String,
    pub action: String,
    pub status: ActionStatus,
    /// The native command run, with no secret in it; empty when none was.
    pub command: String,
}

impl ActionReport {
    fn of(run: &MeridianRun, status: ActionStatus) -> ActionReport {
        ActionReport {
            notifier: run.notifier.clone(),
            resource: run.resource.clone(),
            action: run.action.clone(),
            status,
            command: String::new(),
        }
    }
}

/// The actions `changes` trigger, with those `pending` from an earlier run,
/// each (resource, action) once, in the order the resources are declared.
/// A pending action whose notify the document no longer declares is
/// dropped.
pub(crate) fn triggered(
    declared: &[MeridianRun],
    changes: &[Change],
    pending: &[MeridianRun],
    order: &[String],
) -> Vec<MeridianRun> {
    let changed = |id: &str| {
        changes
            .iter()
            .any(|c| c.resource == id && triggers(c.action))
    };
    let mut out: Vec<MeridianRun> = Vec::new();
    let same = |a: &MeridianRun, b: &MeridianRun| a.resource == b.resource && a.action == b.action;
    for run in pending
        .iter()
        .filter(|p| declared.iter().any(|d| same(d, p)))
    {
        if !out.iter().any(|o| same(o, run)) {
            out.push(run.clone());
        }
    }
    for run in declared.iter().filter(|d| changed(&d.notifier)) {
        if !out.iter().any(|o| same(o, run)) {
            out.push(run.clone());
        }
    }
    let place = |id: &str| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    out.sort_by_key(|r| place(&r.resource));
    out
}

/// What a plan says of each Meridian-run notify: `would` for one its
/// changes trigger, `may` for every one when the tool cannot preview.
pub(crate) fn planned(
    declared: &[MeridianRun],
    plan: &Plan,
    pending: &[MeridianRun],
    order: &[String],
) -> Vec<ActionReport> {
    if plan.unknown {
        return declared
            .iter()
            .map(|d| ActionReport::of(d, ActionStatus::May))
            .collect();
    }
    triggered(declared, &plan.changes, pending, order)
        .iter()
        .map(|r| ActionReport::of(r, ActionStatus::Would))
        .collect()
}

impl Run<'_> {
    /// Runs each triggered action, in order, after the stage applied. Each
    /// one done is taken off the stage's pending list; a failed one stays.
    /// Later actions still run after a failure. A saved plan's stage runs
    /// only the actions the plan listed. Returns the actions and, when any
    /// did not run as asked, why the stage fails.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn act(
        &mut self,
        target: &meridian_plugin_host::Target,
        context: &RunContext,
        artifact: &Artifact,
        id: &StageId,
        document: &Document,
        runs: Vec<MeridianRun>,
        log: &mut dyn FnMut(LogLine),
    ) -> (
        Vec<ActionReport>,
        Vec<ResourceFailure>,
        Option<(Reason, String)>,
    ) {
        let allowed: Option<Vec<(String, String)>> = self
            .planned(id)
            .filter(|p| p.status == PlannedStatus::Planned)
            .map(|p| {
                p.actions
                    .iter()
                    .map(|a| (a.resource.clone(), a.action.clone()))
                    .collect()
            });
        let mut reports = Vec::new();
        let mut errors = Vec::new();
        let mut unplanned = Vec::new();
        for run in runs {
            if self.interrupted() {
                reports.push(ActionReport::of(&run, ActionStatus::Pending));
                continue;
            }
            if let Some(allowed) = &allowed {
                if !allowed
                    .iter()
                    .any(|(r, a)| *r == run.resource && *a == run.action)
                {
                    unplanned.push(format!("{} {}", run.action, run.resource));
                    reports.push(ActionReport::of(&run, ActionStatus::Pending));
                    continue;
                }
            }
            match target.act_with(
                context,
                artifact,
                &run.resource,
                &run.action,
                log,
                &self.options.stop,
            ) {
                Ok(acted) => {
                    self.done_pending(id, &run);
                    reports.push(ActionReport {
                        command: acted.command,
                        ..ActionReport::of(&run, ActionStatus::Done)
                    });
                }
                Err(e) => {
                    let command = match &e {
                        RunFailure::Tool(tool) => tool.command.clone(),
                        _ => String::new(),
                    };
                    reports.push(ActionReport {
                        command,
                        ..ActionReport::of(&run, ActionStatus::Failed)
                    });
                    errors.push((run, e));
                }
            }
        }
        let failed = errors.len();
        let mut failures = Vec::new();
        let mut why = None;
        for (run, e) in errors {
            let mut found = self.failures(&e, document);
            if found.is_empty() {
                found.push(ResourceFailure {
                    failure: meridian_sdk::failure::Failure::new(
                        meridian_sdk::failure::Cause::ResourceFailed,
                        run.resource.clone(),
                        format!("{} failed", run.action),
                    ),
                    at: String::new(),
                });
            }
            failures.extend(found);
            if why.is_none() {
                let more = if failed > 1 {
                    format!(" ({} more failed)", failed - 1)
                } else {
                    String::new()
                };
                let reason = reason_of(&e);
                why = Some((
                    reason,
                    format!(
                        "the stage applied, and {} on {} failed{more}: {}",
                        run.action,
                        run.resource,
                        self.describe(e)
                    ),
                ));
            }
        }
        if why.is_none() && !unplanned.is_empty() {
            why = Some((
                Reason::PlanDiffers,
                format!(
                    "the stage applied, and triggered {} the plan did not list; they are \
                     pending and were not run; take a new plan",
                    unplanned.join(", ")
                ),
            ));
        }
        if why.is_none() && reports.iter().any(|r| r.status == ActionStatus::Pending) {
            why = Some((
                Reason::Interrupted,
                "interrupted: the stage applied and its triggered actions are pending; the \
                 next apply runs them"
                    .to_owned(),
            ));
        }
        (reports, failures, why)
    }

    /// Records the stage's pending actions.
    pub(crate) fn set_pending(&mut self, id: &StageId, pending: Vec<MeridianRun>) {
        if let Some(record) = self.state.get(id).cloned() {
            self.state.set(id, StageRecord { pending, ..record });
            self.write_state();
        }
    }

    fn done_pending(&mut self, id: &StageId, run: &MeridianRun) {
        if let Some(record) = self.state.get(id).cloned() {
            let pending = record
                .pending
                .iter()
                .filter(|p| !(p.resource == run.resource && p.action == run.action))
                .cloned()
                .collect();
            self.state.set(id, StageRecord { pending, ..record });
            self.write_state();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::ChangeAction;

    fn run(notifier: &str, resource: &str, action: &str) -> MeridianRun {
        MeridianRun {
            notifier: notifier.to_owned(),
            resource: resource.to_owned(),
            action: action.to_owned(),
        }
    }

    fn change(resource: &str, action: ChangeAction) -> Change {
        Change {
            resource: resource.to_owned(),
            action,
            summary: String::new(),
        }
    }

    #[test]
    fn a_change_triggers_each_action_once_in_declared_order() {
        let declared = vec![
            run("settings", "web", "restart"),
            run("plan", "web", "restart"),
            run("settings", "api", "restart"),
            run("other", "worker", "restart"),
        ];
        let order = ["api", "web", "worker"].map(String::from).to_vec();
        let changes = vec![
            change("settings", ChangeAction::Update),
            change("plan", ChangeAction::Replace),
            change("other", ChangeAction::Adopt),
        ];
        let got = triggered(&declared, &changes, &[], &order);
        assert_eq!(
            got,
            vec![
                run("settings", "api", "restart"),
                run("settings", "web", "restart")
            ]
        );
    }

    #[test]
    fn a_pending_action_runs_while_its_notify_is_still_declared() {
        let declared = vec![run("settings", "web", "restart")];
        let pending = vec![
            run("settings", "web", "restart"),
            run("x", "gone", "restart"),
        ];
        let got = triggered(&declared, &[], &pending, &["web".to_owned()]);
        assert_eq!(got, vec![run("settings", "web", "restart")]);
    }

    #[test]
    fn a_plan_that_cannot_preview_says_may_for_every_action() {
        let declared = vec![run("settings", "web", "restart")];
        let plan = Plan {
            unknown: true,
            ..Plan::default()
        };
        let got = planned(&declared, &plan, &[], &[]);
        assert_eq!(got[0].status, ActionStatus::May);
    }
}
