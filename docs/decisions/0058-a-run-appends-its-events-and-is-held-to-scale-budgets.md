# A run appends its events, and is held to scale budgets

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | issue 240, scale tests (part 1) |
| Where it shows | [Running a lab of environments](../operations/lab.md#scale), [State, runs and backends](../reference/state.md#events) |

## The choice

- **`events.jsonl` is appended, not rewritten**, at each flush, and each event is serialised
  once. Rewriting the whole log at every stage start and end made plan and apply grow with the
  square of the stage count: 96 s to plan 200 stages and 1,000 endpoints, 8 s after.
- **The local backend appends to the file and syncs it.** A crash can leave a partial last line,
  which a reader of the log already skips.
- **A backend program keeps its protocol.** Meridian appends to it by reading the blob and putting
  it back whole, so it still works, only more slowly.
- **`run.json` is still written whole**, as it is small beside the events and a reader must never
  see half of it.
- **Tier 1 scale is a test with budgets** (`tests/scale/budgets.toml`): wall time per command and
  peak memory, failing at 20% over. Budgets move only in the pull request that changes them.

## Alternatives

- Throttle flushes by time. Rejected: a crash would lose the record of the steps that ran.
- One blob per step's events. Rejected: thousands of keys per run, and every reader changes.
