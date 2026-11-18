# A stop is asked of the plugin, and an interrupted run is not rolled back

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the Ctrl-C and time-limit work for issue 96 |
| Where it shows | [Stopping a run](../concepts/run-safety.md#stopping-a-run), [Being stopped](../contributing/writing-a-plugin.md#being-stopped) |

## Context

A run of a tool could not be stopped: Ctrl-C ended `meridian` with the lock held and the run recorded as `running`, while the plugin caught the same signal and died under its tool. A tool waiting on a prompt or a dead connection held the run forever. Issue 96 asked for both to be fixed, and left open how the host should stop a plugin, and what an interrupted `--on-failure rollback` apply should do.

## The choice

The host asks. A `Cancel` call was added to the `Runner` service; the SDK sets a flag the runner's log reports and `Tool::run` polls, stops the tool's process group (`SIGTERM`, then `SIGKILL` after ten seconds), and the run's stream ends with the plugin's own account of what the tool had done. The host keeps reading the stream until that account arrives, for up to twenty seconds. Plugins are started in a process group of their own, so a Ctrl-C meant for `meridian` reaches them only as this request.

An interrupted apply is not rolled back, even when `--on-failure rollback` was given: the stage is recorded as `interrupted`, nothing more runs, and the operator is told that `meridian rollback` puts the run back. A stage stopped by `--stage-timeout` is an ordinary failure and is rolled back.

The exit status after Ctrl-C is `130`, the shell's own for a command ended that way; a time limit exits `1`.

## Alternatives

Hanging up on the plugin's stream instead of asking. Simpler and needing no protocol change, but then the plugin's account of what the tool had done can never reach the host, and the SDK has to guess from a broken stream whether the host meant it or died. Hanging up stays as the fallback for a plugin too old to be asked.

Rolling an interrupted apply back when `--on-failure rollback` was given. It is what the flag says, but an operator who pressed Ctrl-C asked for the run to stop, and a rollback starts more tools that the same interrupt would then have to stop too.
