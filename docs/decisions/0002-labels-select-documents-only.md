# `metadata.labels` select documents and are not passed to plugins

| | |
|---|---|
| Area | Language |
| Status | **accepted** 2026-11-18 |
| Made in | PR 39 |
| Where it shows | [Labels and selectors](../reference/dsl/index.md#labels-and-selectors) |

## Context

Labels could be a way of choosing which documents a command runs, or a way of tagging what a plugin writes, or both.

## The choice

Labels are for selection only: `-l` picks the documents whose labels match. A plugin never sees them, so no target writes them into tags or metadata.

## Alternatives

Pass labels to plugins and let a target map them to its own tags.
