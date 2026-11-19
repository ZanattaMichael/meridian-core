# `meridian diagnose` reads the record and runs nothing

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "`meridian diagnose`" (issue 215) |
| Where it shows | [Troubleshooting a failed run](../operations/troubleshooting.md), [the `meridian` command](../reference/cli.md#diagnose), [`--output json`](../reference/cli-json.md#diagnose), [the MCP server](../tooling/mcp-server.md) |

## The choice

- **Read only**: `run.json`, `events.jsonl` and this machine's `failures.jsonl`. No tool,
  no target plugin, no lock; it works while another run holds the state.
- **Default run**: the newest with a failed step, not the newest run.
- **Failed steps first**, then the steps not run, named by the step that stopped them
  (`becauseOf` from `step.skipped`).
- **Grouped**: three or more machines failing one step with one cause are one entry.
- **Without `failures.jsonl`** (another machine, `--no-logs`): resources and causes come
  from the `resource.failed` events, without the tool's words.
- **`next` names only commands that exist today**: `logs`, `validate`, and the run's own
  command with `--tool-debug` (and `--resume` after an apply). `doctor` (issue 218) and
  rerunning one resource (issue 216) are added when they exist.
- **The MCP tool runs `meridian diagnose --output json`** and is always offered, as it
  reads only.
- **No `explain` LAST RUN block**: there is no `meridian explain <resource>` command yet.

## Status

Proposed.
