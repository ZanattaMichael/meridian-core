# Meridian runs a notify the tool cannot, after the stage applies

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull requests for issue 261: part 1 (core), part 2 (Azure actions), part 3 (AWS actions) |
| Where it shows | [Dependencies and notifications](../concepts/dependencies.md#meridian-run-notifies), [`--output json`](../reference/cli-json.md), [Writing a plugin](../contributing/writing-a-plugin.md), [the capability matrix](../reference/capability-matrix.md) |

## The choice

- **An optional `Runner.Act` call** (protocol 2, streamed and cancellable as `Apply`),
  taking the context, the artifact, the resource id and the action, and returning the
  native `command`. `Describe.acts` lists the `(type, action)` pairs, a type being a glob
  with `*`. No new capability field: a non-empty `acts` is the capability.
- **Native, then synthetic, then Meridian-run.** A plugin lists only what its tool cannot
  express. The host takes each covered notify out of the tree before `Emit`, with a
  compile warning, so the emitter is unchanged; a pair not listed stays
  `unsupported-action`.
- **Trigger**: the notifier's `create`, `update` or `replace` in the apply's changes, as
  the tool traced them to resource ids. Not `adopt` or `delete`.
- **Timing**: after the stage's apply returns, in apply order, each `(resource, action)`
  once per stage. No immediate mode: the tool applies the stage as one unit.
- **Pending actions live in the run-state** (`pending` on the stage), written as soon as
  `Apply` returns and before any action runs, and removed one by one as each succeeds. The
  next apply of the stage, resumed or not, runs what is pending while the document still
  declares the notify. A resumed run does not skip a stage with pending actions.
- **A failed apply** records what its reported changes trigger as pending and runs none.
- **A failed action** fails the stage (its reason from the failure, `resource-failed` on
  the resource when the plugin names none). Later actions still run.
- **Plan**: `would` for each action the planned changes trigger and each pending one;
  `may` for every action when the tool cannot preview. A plan with only actions has
  changes.
- **Saved plan**: the plan file lists the stage's actions; `apply --plan` runs only those,
  and an action it triggers that the plan did not list stays pending and fails the stage
  with `plan-differs`.
- **Rollback runs no action.** A restart is not undone.
- **Credentials**: the action runs with the stage's context, so with the credential from
  its provider (decision 0047). Nothing new is stored.

## The fake target

`touch` on a `file` is Meridian-run; acting appends to `acts.log`; `NOACT` in the applied
file fails it; `FAKE_ACTS=off` turns it off. `FAKE_CHANGES=ids` traces its changes to
resource ids, which a notify needs.
