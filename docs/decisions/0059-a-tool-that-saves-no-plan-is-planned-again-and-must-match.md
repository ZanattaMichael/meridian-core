# A tool that saves no plan is planned again and must match

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request for issue 268 |
| Where it shows | [A saved plan](../operations/pull-requests.md#what-apply---plan-checks), [capability matrix](../reference/capability-matrix.md), [errors](../reference/errors.md), [`meridian plugins`](../reference/cli.md#plugins) |

- **When**: on `apply --plan`, for a stage handed no saved tool plan: a target whose
  `saves_plans` is false, or Terraform, OpenTofu and Pulumi when a secret kept the plan
  out of the file. Not for a `deferred` stage (decision 0038).
- **How**: the stage is planned again just before its apply, after every other check.
  The changes are compared with the plan's by `(resource, action)`, counted; summaries are
  ignored, so wording (ARM's `what-if`) refuses nothing.
- **A difference fails the stage** with `plan-differs`, listing what the tool would now do
  that the plan did not show and what it would no longer do. Later stages do not run.
- **Unknown cannot be guarded**: a plan or re-plan marked `unknown`, or a target that cannot
  plan, fails with `plan-differs` unless `apply --plan --accept-unknown`, which applies it
  with a warning. `--accept-unknown` needs `--plan`.
- **Shown as `guarded`**: the matrix's `saved plan` column, `meridian plugins`
  (`plan file:`) and its JSON (`savedPlan`) read `yes`, `guarded` or `no`.
- **Not atomic**: a change between the re-plan and the apply is not caught. Cost: one more
  plan per stage.
