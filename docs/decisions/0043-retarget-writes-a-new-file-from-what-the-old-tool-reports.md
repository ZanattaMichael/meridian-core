# `meridian retarget` writes a new file from what the old tool reports

| | |
|---|---|
| Area | Core |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "Move a document to another tool with `meridian retarget`" (issue 118, second part) |
| Where it shows | [Moving a document to another tool](../operations/retargeting.md), [the `meridian` command](../reference/cli.md#retarget), the Terraform, OpenTofu and Pulumi target pages, [Writing a plugin](../contributing/writing-a-plugin.md) |

## Context

Issue 118 asks for `meridian retarget`: move a document from one tool to
another without the new tool creating again what the old one made. The first
part added [`adopt`](0042-adopt-is-a-resource-field-and-a-target-says-how-it-adopts.md);
this part fills it in.

## The choice

- **The old tool is asked, through a new optional plugin operation.**
  `Runner::identifiers` (protocol: `Identifiers`, `DescribeResponse.reads_identifiers`)
  returns each managed resource's identifier by resource id, read from the
  tool's own state. Terraform and OpenTofu read `show -json`, Pulumi reads
  `stack export`. It is read only, and a plugin that does not have it says so.
- **A new file, never the old one rewritten.** The old file still describes
  what the old tool manages until its state is cleaned up; overwriting it
  would leave nothing to compare with. An existing output file is refused.
- **Rewritten as text.** `spec.target` changes in place and each `adopt:` line
  goes under the resource's `id`, so comments and layout survive, as
  `meridian migrate` does. JSON is refused; a flow-style resource is a note.
- **What the new target does decides what is written.** `import`: `adopt:` per
  resource. `converge`: the target only. `none`: the target only, and a
  warning that the first apply creates everything again.
- **Refuse rather than move silently.** With `import`, a document the old tool
  never applied, or an old tool that cannot report, is refused unless
  `--no-adopt` is given.
- **Where the new file goes: three sources, the first set wins** (the owner's
  answer on the pull request): `--out`, then `spec.retarget.out` in the
  project's `meridian.yaml` (a pattern of `{name}`, `{target}` and `{ext}`
  that must hold `{target}`), then `{name}.{target}.{ext}` beside the file.
  A setting for every project of a user or an organisation waits on a global
  settings file, issue 219.
- **No tool changes anything.** `retarget` does not clean the old tool's state
  and does not plan; the output says to run `meridian plan` on the new file.

## Alternatives

- Rewriting the file in place. Rejected for the reason above.
- Reading the old tool's state files directly in the core. Rejected: the core
  knows no tool's format; the plugin does.
- Removing the resources from the old tool's state as part of the move.
  Left out: it cannot be undone, and it should follow a successful apply by
  the new tool, not precede it.

## Status

Proposed. The output name was settled by the owner. Still open: whether `retarget` should also run a compile of the new file, and whether a
later command should remove adopted resources from the old tool's state.
