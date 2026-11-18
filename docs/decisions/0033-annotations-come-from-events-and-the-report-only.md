# A pipeline engine is shown the events and the report, and nothing formatted a second time

| | |
|---|---|
| Area | Reporting |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Run Meridian inside any pipeline engine" (issue 184) |
| Where it shows | [Running Meridian in a pipeline](../operations/running-in-a-pipeline.md#what-meridian-does-inside-an-engine), [`--annotations`](../reference/cli.md#inside-a-pipeline-engine), `meridian_report::engine` |

## Context

Inside a pipeline engine, a refused document or a failed step is worth
showing where the engine shows such things: on the pull request at the line
of the document, and on the job's summary page. GitHub Actions and Azure
Pipelines read a command syntax from the job's output for that, Buildkite
takes Markdown through its agent, and the others have a log and artifacts.

The obvious way to write those is a second formatter beside the one that
prints for a person: one that knows about errors, stages, outcomes and
tools, and writes them in the engine's syntax. Two formatters drift, and a
second one that reads a tool's output or a plan is a second place a value
could leak from.

## The choice

What an engine is shown is derived from two things only:

- a document's diagnostics, as `validate` reports them, each with its file,
  line and column; and
- the [report](../reference/reporting.md) folded from the run's
  [events](../reference/state.md#events): a step that failed, could not be
  put back or drifted is a problem, with the fixed reason word the record
  keeps, pointed at the document's position; and the summary is the
  Markdown report that `--report markdown=FILE` already writes, with where
  the record is kept added at its end.

The rendering lives in the `meridian-report` crate, which cannot see a
document, an artifact or a tool's output, so it cannot format anything else.
The command only detects the engine and writes what the crate hands it. An
event never holds a value, so neither does an annotation or a summary, and
nothing needs masking: the page says so rather than relying on the engine's
masking.

Annotations are written to standard error, beside the error for a person.
Standard output stays the command's result, so `--events json` and
`--output json` are not disturbed; GitHub Actions reads its workflow commands
from either stream, which the smoke test proves, and Azure Pipelines' agent
is documented to do the same, which it has not been run against here.

Writing for an engine is on when the engine's variable is set and standard
output is not a terminal (`--annotations auto`); `on` ignores the terminal,
for Buildkite's pseudo-terminal; `off` turns it off. The engine's name is
not an option: one variable per engine is detection enough, and a wrong
guess costs a few lines in a log.

## Alternatives

- A second formatter in the command, reading the stage reports. Rejected:
  the stage report holds the tool's own error text, which can quote a value
  the tool was handed, and the two formatters would disagree the first time
  one of them changed.
- Writing annotations to standard output, where the engines document their
  commands. Rejected: standard output is the command's result, and
  `--events json` and `--output json` both own it.
- An `--engine NAME` option instead of detection. Rejected as the primary
  switch: every engine already says which it is; `--annotations on|off` is
  enough to override.

## Status

Proposed. Open to the owner: whether an engine with no annotation syntax of
its own (GitLab CI, Jenkins) should get the problems as a plain
`meridian:` line block in the log instead of nothing, and whether the
Azure summary should go through `task.addattachment` as well, so it shows
on the run's summary tab under its own name.
