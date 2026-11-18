# `compile`, `plan` and `apply` do not run compile rules

| | |
|---|---|
| Area | Compile rules |
| Status | **accepted** 2026-11-18 |
| Made in | PR 64 |
| Where it shows | [Compile rules reference](../reference/compile-rules.md) |

## Context

Compile rules check the layers of a hierarchy. `compile`, `plan` and `apply` read documents, not a hierarchy.

## The choice

The rules run only in `meridian rules check`. A pipeline calls it before compiling. The three commands will run the rules once they read a hierarchy themselves, in the data phase of the language expansion.

## Alternatives

Have every command read the hierarchy and run the rules now.
