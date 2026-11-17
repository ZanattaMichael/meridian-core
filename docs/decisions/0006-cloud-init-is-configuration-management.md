# cloud-init sits under configuration management

| | |
|---|---|
| Area | Categories |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 33 |
| Where it shows | [cloud-init](../targets/cloud-init.md), [capability model](../concepts/capability-model.md) |

## Context

cloud-init configures a machine on its first boot, which is bootstrap, not quite configuration management.

## The choice

cloud-init takes a ResourceSet and is listed under configuration management, so the same six resource types compile for it.

## Alternatives

Add a bootstrap category.
