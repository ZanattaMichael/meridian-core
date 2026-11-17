# Group and layer paths are case-sensitive

| | |
|---|---|
| Area | Compile rules |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 64 |
| Where it shows | [Compile rules reference](../reference/compile-rules.md), [compared with DscPipelineRunner](../reference/compile-rules.md#from-dscpipelinerunner) |

## Context

DscPipelineRunner, which the rules are modelled on, compares paths without regard to case. Meridian runs on Linux, where file names are case-sensitive.

## The choice

Paths and keys are compared exactly, including case. A rule that must ignore case says `ignoreCase: true`.

## Alternatives

Compare without regard to case everywhere, as DscPipelineRunner does.
