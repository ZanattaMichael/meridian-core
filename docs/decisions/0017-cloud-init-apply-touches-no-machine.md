# cloud-init's `apply` does not touch a machine

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 33 |
| Where it shows | [cloud-init](../targets/cloud-init.md) |

## Context

A cloud-init document is read by a machine the first time it boots, so there is nothing to apply to until the machine exists.

## The choice

`apply` writes the user-data and a NoCloud seed on the machine running Meridian. The new machine applies it by itself on its first boot; `apply` can wait for that boot over SSH.

## Alternatives

Refuse `apply`, and have only `compile` and `plan`.
