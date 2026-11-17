# There is no central module index

| | |
|---|---|
| Area | Modules |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 62 |
| Where it shows | [Modules](../reference/modules.md), [modules and versioning plan](../plans/modules-and-versioning.md#5-open-questions) |

## Context

A module could be found by name in a hosted index, or only by the source a requirement names.

## The choice

A requirement says where the module is: a path, a git repository or an OCI registry. There is no index and no hosted registry. If one is ever wanted it is a hosted service and a decision for the owner.

## Alternatives

A hosted index, as crates.io or the Terraform registry.
