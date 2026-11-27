# A configuration management target destroys by applying the document's inverse

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work, the owner's refinement of 2026-11-25 asks for it |
| Made in | the pull request "ansible: `destroy` by compiling the document's inverse (issue 270, part 1)" |
| Where it shows | [Destroying what was applied](../operations/destroying.md), [the Ansible target](../targets/ansible.md), [capability matrix](../reference/capability-matrix.md), [errors](../reference/errors.md) |

## Context

`destroy` is an optional plugin operation (decision 0048). It was built for the
targets whose tool keeps a record of what it made (Terraform, CloudFormation,
Kubernetes, Docker, ...). The configuration management targets say `destroys()
-> false`, because their tools keep no such record, so a destroy leaves their
stages in place (the `cannot-destroy` status). Issue 270 asks Meridian to close
that gap: compile the document's inverse into the tool's own language and apply
it.

## The choice

- **The tool's own inverse, applied last stage first.** A target that destroys
  derives the inverse of its own compiled artifact and applies it with the same
  tool. For Ansible the artifact is the play Meridian wrote; the plugin inverts
  it and runs the result with `ansible-playbook` (`--check --diff` for the
  preview).
- **Reverse order within the play.** A `service` is stopped before the package
  it needs is removed, and a resource is removed after the ones that depend on
  it. The order is the reverse of the task sequence the document compiled to.
- **Per-resource inverse.** `package`, `file`, `user`, `group` and `cron` become
  the tool's `absent` state; `service` becomes stopped and disabled; a `file`
  written from literal content (a `copy`) becomes the `file` module's `absent`
  on the same destination.
- **A resource with no absent state is kept, not failed.** `exec` (a
  `command`/`shell`), or a native module with no absent state, is not inverted.
  The destroy removes what it can and **names each resource it left in place**
  with the rule `unsupported-destroy`. This follows the owner's rule for 270:
  a destroy tries to remove everything and flags what it cannot, rather than
  stopping.
- **The artifact is unchanged.** The inverse is derived at destroy time from
  the same artifact, so what `compile` writes — and what the golden files
  record — is the same whether or not a document is ever destroyed.

## Alternatives

- **Compile the inverse into the artifact.** Rejected: it changes the compiled
  output for every document, and the Ansible golden files that record the port,
  even for a document that is never destroyed. Deriving the inverse at destroy
  time keeps `compile` byte for byte as it was.
- **Meridian keeps its own record of what each stage made.** Rejected: it would
  duplicate, and then disagree with, the tool's own state — the reason #260
  keeps Meridian from holding resource state of its own.
- **A user-adjustable teardown document stored in the configuration.** The
  owner's fuller mechanism: the inverse is computed when the document is first
  parsed, stored with the configuration, and may be hand-adjusted, and a
  user-written teardown document takes precedence. That shape is deferred to a
  following issue; this record covers the target half, which the stored document
  would call.
