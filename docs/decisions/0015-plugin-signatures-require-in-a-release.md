# Plugin signatures default to `require` in a release and `off` from source

| | |
|---|---|
| Area | Releases |
| Status | **accepted** 2026-11-18 |
| Made in | PR 61 |
| Where it shows | [CLI reference](../reference/cli.md), [plugin signatures](../reference/plugin-signatures.md) |

## Context

A released `meridian` and one built from source have different users: one runs plugins it downloaded, the other plugins it just built.

## The choice

`--plugin-signatures` defaults to `require` in a released build and `off` in one built from source. Either can be set the other way.

## Alternatives

`warn` everywhere, or `require` everywhere with a signing step in the developer loop.
