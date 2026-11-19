# `adopt` is a field of the resource, and each target says how it adopts

| | |
|---|---|
| Area | Language |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "Adopt existing resources with `adopt`" (issue 118, first part) |
| Where it shows | [Adopting what exists](../reference/dsl/resources.md#adopting-what-exists), the [capability matrix](../reference/capability-matrix.md), the Terraform, OpenTofu and Pulumi target pages, [Writing a plugin](../contributing/writing-a-plugin.md) |

## Context

Issue 118 asks for a way to say "this already exists, as this identifier", so a
stateful tool takes the thing under management instead of creating a second
one. It names the field `spec.adopt`.

## The choice

- **A field of the resource, `adopt: <identifier>`, not a list under `spec`.**
  The identifier belongs to one resource, a list would repeat the resource's id
  and could name one that does not exist, and the field reaches a plugin on the
  resource it is about, as `runtimeWhen` does. It is text, placed from data like
  `state`, with the target's literal escapes, and never a secret.
- **A capability with three values.** `import`: the emitter compiles the
  identifier into the tool's own import (Terraform and OpenTofu `import`
  blocks, Pulumi's `import` option). `converge`: the tool keeps no state and
  makes each thing what the document says whether it exists or not, so `adopt`
  is accepted and changes nothing, with a warning the SDK adds. `none`: refused.
- **The host refuses for `none` before the plugin is asked.** A plugin built
  before the field existed reports `none`, so it never receives an `adopt` it
  would drop and then create a duplicate. The SDK makes the same check in front
  of every `emit`.
- **A new change action, `adopt`.** A plan that only imports reports `adopt`,
  not `create` (Terraform reported it as nothing at all, Pulumi as `create`). An
  import that also changes the thing is reported as the change, with `adopted`
  in the summary.
- **`adopt` with `state: absent` is refused** at validation.

## Alternatives

- `spec.adopt` as a list of `{resource, id}`. Rejected for the reasons above.
  A later `meridian retarget` can still write `adopt` onto each resource.
- A boolean capability. Rejected: "accepted and changes nothing" and "compiled
  into the tool's import" are different promises, and a configuration
  management document should move between tools without editing.
- CloudFormation now. Left out: it needs a resource-import change set and
  `DeletionPolicy: Retain` in the runner, and real-AWS testing this pull
  request cannot do. It refuses `adopt` until then.

## Status

Proposed. Open to the owner: the field name and place (the issue said
`spec.adopt`), whether CloudFormation is the next target, and whether
`converge` should warn or stay silent.
