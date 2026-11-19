# Tool output is kept, masked, on the machine that ran it, and never in the record

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "Keep each step's tool output; `meridian logs` and `meridian bug-report`" (issue 214) |
| Where it shows | [State, runs and backends](../reference/state.md#runsidlogs), [the `meridian` command](../reference/cli.md#logs), [running safely](../concepts/run-safety.md#the-record) |

## Context

What a tool printed was shown on the terminal and then dropped, because tool
text can quote a secret and the record must hold none. A run in CI, or one
whose terminal is gone, left nothing to read about why a step failed.
Issue 214 asks to keep it.

## The choice

- **Kept apart from the record.** `runs/<id>/logs/<kind>/<document>[@<endpoint>].log`
  in the state directory. The record still holds no tool text, so a shared
  backend never receives any.
- **Masked before it is written**, with the same filter the terminal uses:
  every secret the stage fetched and every sensitive output of an earlier
  stage. A line is written as `<time> <stream> <text>`.
- **Local only, owner-readable** (mode `0600`), as `history/` is.
- **On by default**, capped per step at `--log-limit` (10 MiB): the first
  half and the last half are kept, and a `meridian` line marks the cut. The
  tail is held in memory until the step ends. `--no-logs` turns it off.
- **Kept as long as the run's record.** `--keep-runs` removes both.
- **Meridian's own lines** use the stream `meridian`: a step's failure with
  its reason word and message, a failed rollback, a cut.
- **`meridian bug-report` copies, never collects anew.** The archive holds the
  record, the events, the kept logs, `meridian version` and the run's
  plugins as `meridian plugins --output json` lists them. It sends nothing.

## Alternatives

- Putting the logs in the backend. Rejected: masking catches only the
  secrets Meridian knows of, and a shared store is the wrong place to find
  that one was missed.
- Writing every line straight to the file and truncating at the cap. Rejected:
  the end of the output is where a tool says why it failed.
- Off by default. Rejected: the run worth reading is the one nobody expected
  to fail.

## Status

Proposed. Open: tool versions in the bundle (recorded by issue 217), and the
per-resource failures of issue 213, which the bundle should carry once they
exist.
