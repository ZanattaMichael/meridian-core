# The run id is carried in the data and read as `meridian.run.id`

| | |
|---|---|
| Area | Language |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | issue 235, part 2 |
| Where it shows | [Expressions](../reference/dsl/expressions.md#names), [`--run-id`](../reference/cli.md), [Testing](../reference/testing.md#the-run-id) |

## The choice

- **Given, never made up by a run.** `--run-id` or `MERIDIAN_RUN_ID`; unset otherwise, and
  reading it unset is an error. A new id on every `apply` would rename resources each time.
- **`meridian test` gives one.** `test` for compile and plan cases; a new `t` + 8 hex digits
  per lifecycle run, the same for its provision, verify and destroy, written to `run-id` in
  the scratch directory.
- **Carried in the data as `meridian.run.id`.** Every resolve, condition and selector sees it
  with no new argument, and a saved plan holds it as one of its `--set` values.
  `data.meridian` is reserved: a data file, `--set` or test `set` that sets it is refused.
- **Read as `meridian.run.id`, not `data.meridian.run.id`**, and removed from the data handed
  to a plugin's compile, so the files a plugin writes do not change with it.
- **Lowercase letters, digits and `-`, up to 32, starting with a letter**, so it fits a name
  or tag on every target.

## Alternatives

- A run-id argument on every resolve function. Rejected: about twenty call sites, and a saved
  plan would need a field of its own.
- The orchestrator's own run id. Rejected: it changes on every run.
