# Terraform import renames colliding names

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 72 |
| Where it shows | [Importing existing configuration](../operations/importing.md) |

## Context

Two Terraform resources of different types may share a name, which a document cannot.

## The choice

The importer renames one and says so, with the `terraform state mv` command that keeps the state in step.

## Alternatives

Refuse to import, or prefix every id with its type.
