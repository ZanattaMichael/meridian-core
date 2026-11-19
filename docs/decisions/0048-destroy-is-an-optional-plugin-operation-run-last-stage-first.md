# `destroy` is an optional plugin operation, run last stage first

| | |
|---|---|
| Area | Orchestrator |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "`meridian destroy`: remove what was applied (issue 127, part 1)" |
| Where it shows | [Destroying what was applied](../operations/destroying.md), [the `meridian` command](../reference/cli.md#destroy), the Terraform and OpenTofu target pages, [Writing a plugin](../contributing/writing-a-plugin.md) |

## Context

Plugins could plan and apply but not remove what they made. The lifecycle lab (#234) ends
every environment with a teardown, and a rollback of a first apply had nothing to call.

## The choice

- **An optional runner operation.** `Runner::destroys`, `plan_destroy` and `destroy`
  (protocol: `DescribeResponse.destroys`, `PlanRequest.destroy`, `Runner.Destroy`, streamed
  like `Apply`). The host asks only a plugin that declares it.
- **The tool's destroy, not Meridian's.** The plugin runs what its tool does
  (`terraform plan -destroy` then that plan applied). Meridian keeps no resource state.
- **Reverse order.** Stages run last first, so a ResourceSet goes before the Infrastructure
  whose output names its host, and that output is still readable.
- **A target that cannot destroy is kept, not failed.** Configuration management tools keep
  no record of what they made; their host goes with the Infrastructure that made it. A
  stage never applied is kept too.
- **Forgotten on success.** The stage's run-state entry is removed, so `drift` reports it
  never applied. On failure it stays, so `destroy` can be run again.
- **Asked for.** A terminal prompt, or `--yes`; refused with no terminal or with JSON output.
- **No policy, resume or saved plan yet.** Refused rather than silently ignored.

## Alternatives

- `apply` of an empty artifact. Rejected: CM targets would converge nothing and report
  success, and IaC tools would still need their own destroy semantics (providers, backends).
- Failing a destroy on a target that cannot destroy. Rejected: a workflow with one
  Terraform and one Ansible stage could never be torn down.
