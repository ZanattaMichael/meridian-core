# `meridian inspect` reads one resource through an optional plugin call

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull requests for issue 222: part 1 (core), part 2 (infrastructure targets) |
| Where it shows | [the `meridian` command](../reference/cli.md#inspect), [`--output json`](../reference/cli-json.md#inspect), [Troubleshooting a failed run](../operations/troubleshooting.md#meridian-inspect), [the MCP server](../tooling/mcp-server.md), [Writing a plugin](../contributing/writing-a-plugin.md), [the capability matrix](../reference/capability-matrix.md) |

## The choice

- **An optional `Runner.Inspect` call**, declared by `Describe.inspects` (protocol 2,
  ignored by a side that predates it). It takes the context, the artifact, the resource id
  and the endpoint, and returns `found`, `actual`, `in_desired_state` (yes, no, unknown)
  and the native `command`. Read only by contract.
- **The core compares, the plugin reads.** `desired` is `state` and the params as compiled;
  the core lists the properties that differ. Text read back equals a number or boolean
  (`"80"` is `80`). A property the tool did not report is `not-reported`, never a
  difference.
- **Secrets are never compared or shown.** A param holding a secret is `sensitive`: desired
  prints `(sensitive)`, and `actual` and `command` are masked as tool output is.
- **One step, compiled as drift compiles it**: imports from earlier steps come from their
  tools' outputs. No lock, no run record, no apply. The plugin is sent the step's secrets
  and credential, because reading a machine needs them.
- **The endpoint is the step's own.** `--endpoint` must name it; reading another machine
  waits for a step that configures more than one.
- **A target that cannot inspect is refused** with the general rule `unsupported-inspect`.
- **`diagnose` suggests it** for a failure that names a resource.
- **The MCP tool is opt-in** (`--allow-inspect`), as it runs the real tool.
- **Exit 0 whatever the tool reads**: a difference is the answer, not a failure.

## Infrastructure targets (part 2)

| Target | Actual | As declared |
|---|---|---|
| Terraform, OpenTofu | `show -json`, the root-module resource named by the id | `plan -target=ADDR -detailed-exitcode -lock=false` of the compiled configuration |
| Pulumi | `stack export`, the resource's `outputs` | not given: the export is the last update's record |
| CloudFormation | `detect-stack-resource-drift`, the actual properties | not given: drift is against the last template |
| Bicep, ARM | `az resource show`; location, tags, sku, kind, identity, properties | not given: Azure keeps no record |

A name that is an expression, and an ARM deployment to a subscription, are refused with
`unsupported-inspect`.

## Status

Proposed.
