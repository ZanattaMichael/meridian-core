# A failed resource is named with a fixed cause word, and the tool's words stay on the machine that ran

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "Name the resource, machine and cause of a failure" (issue 213, part 1) |
| Where it shows | [resource failures](../reference/state.md#resource-failures), [failure causes](../reference/failure-causes.md), [the `apply` output](../reference/cli.md#apply), each target's "Reading a failure" |

## The choice

- **The plugin reads its tool's structured output** (Terraform's `-json`
  diagnostics, Ansible's `fatal:` results) and returns one `Failure` per
  resource: resource, endpoint, cause, tool code, summary, detail.
- **Failures travel in `RunError.failures`**, not in `PlanResult` or
  `ApplyResult` as the issue sketched: a failure exists only when the call
  failed, and every operation's error carries them the same way.
- **Causes are ten fixed words** from `meridian_sdk::failure::Cause`. A word
  never changes or is reused; an unknown word reads as `unknown`.
- **The record gets names and the cause only**: one `resource.failed` event
  per failure. The tool's code, summary and detail go to the terminal, the
  `--output json` stage, and `runs/<id>/failures.jsonl`: masked, local only,
  mode `0600`, kept and removed with the logs, packed by `bug-report`.
- **The host finds `file:line`** from the document's resource id, so a
  plugin need not know where the resource was written.
- **Terraform and OpenTofu run `plan` and `apply` with `-json`**; a person is
  shown each line's message and a diagnostic's detail.
- **Part 1 covers the SDK, protocol, Terraform, OpenTofu and Ansible; part 2
  the configuration management targets** (Salt, Puppet, OpenVox, Chef,
  DSC); **part 3 the other infrastructure targets** (CloudFormation, Bicep,
  ARM, Pulumi, Packer, cloud-init). Containers and orchestration follow.
- **A target whose failing command says too little asks the tool again**:
  CloudFormation's stack events, Azure's deployment operations.
- **Where a tool has no structured output, its fixed lines are read**:
  Pulumi's `Diagnostics:` section, Puppet's `Error:` and `Skipping because of failed dependencies` lines,
  chef-client's `Error executing action` block. `chef-run` reports none.
- **Shared causes come from the message** (`Cause::from_message`): a refused
  login, a failed connection, a timeout, a reboot.

## Status

Proposed.
