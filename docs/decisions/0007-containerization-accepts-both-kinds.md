# Containerization accepts both document kinds

| | |
|---|---|
| Area | Categories |
| Status | **accepted** 2026-11-18 |
| Made in | PR 30 |
| Where it shows | [Containerization](../reference/dsl/containerization.md), [capability model](../concepts/capability-model.md) |

## Context

A container target such as Docker or Podman can be asked to run a service, which reads like a ResourceSet, or to declare a stack, which reads like Infrastructure.

## The choice

The containerization category accepts both kinds, and each target says which it compiles.

## Alternatives

Fix one kind per category, as the other categories do.
