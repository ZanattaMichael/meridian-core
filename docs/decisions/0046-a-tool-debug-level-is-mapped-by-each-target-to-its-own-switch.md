# A `--tool-debug` level is mapped by each target to its tool's own switch

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "`--tool-debug`: ask each step's tool for its own debug output" (issue 221) |
| Where it shows | [the `meridian` command](../reference/cli.md#asking-a-tool-for-more-output), [the capability matrix](../reference/capability-matrix.md#--tool-debug), each [target page](../targets/ansible.md#troubleshooting) |

## Context

Each tool has its own debug switch (`-vvv`, `TF_LOG`, `--debug`, `-v=6`).
Reaching it meant knowing the switch and setting `MERIDIAN_<TOOL>_ARGS` by
hand. Issue 221 asks for one flag.

## The choice

- **Levels 0 to 3.** `--tool-debug` alone is 1; `MERIDIAN_TOOL_DEBUG` sets it.
  Level 0 changes nothing.
- **The plugin owns the mapping.** `RunContext.debug_level` carries the
  level; a runner declares one table (`Runner::debug_levels`) of arguments
  and environment per level, and applies it with `Tool::debug`. The same
  table is sent in `Describe.debug_levels`, so `meridian plugins` and the
  capability matrix show what the runner does.
- **A level above a target's last uses its last.** A target with no table
  runs as usual and `meridian` warns.
- **The output is ordinary tool output**: printed, masked, kept in the
  step's log. Nothing new reaches the record but the level, in
  `run.started` and the recorded command.
- **`--tool-debug-step NAME`** limits it to named documents, since debug
  output of every step can bury the one that matters.
- **Every target has levels.** A runner with a second tool declares its
  table too (`Runner::more_debug_levels`): Bolt beside `puppet apply`,
  `chef-run` beside `chef-client`. Where the tool has no switch, the runner
  does the work and says so in words (`DebugLevel::does`): `chef-run` gets
  a config file with a `[log]` level and its log is copied to the step;
  Podman shows more of the journal, `systemctl status` and Quadlet's units.
- **Debug output stays off any stream Meridian parses.** Bolt and the
  others that print JSON log to standard error.

## Alternatives

- One environment variable per tool, set by the user. Rejected: it is what
  existed, and it gives no level that means the same across targets.
- Meridian maps levels itself, from a table in the core. Rejected: the
  plugin knows which call takes the switch and where the output goes.
- No levels for Podman, Bolt and `chef-run`. Rejected by the owner: every
  engine needs its own way to show more.

## Status

Proposed.
