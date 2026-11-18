# With no data given, editors treat every `when` as true

| | |
|---|---|
| Area | Language |
| Status | **accepted** 2026-11-18 |
| Made in | PR 73 |
| Where it shows | [Language server](../tooling/language-server.md) |

## Context

`fmt`, `lint` and the language server check a document without the data a run would supply, so a `when` cannot be decided.

## The choice

Every `when` is treated as true, so every resource and edge is checked as if present. A `when` that interpolates values is checked against the data given, when any is.

## Alternatives

Treat an undecidable `when` as false, or refuse to check without data.
