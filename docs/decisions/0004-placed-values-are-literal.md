# A value placed by interpolation is always literal text

| | |
|---|---|
| Area | Language |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 71 and issues 134 to 136 |
| Where it shows | [Values from data are literal](../reference/dsl/interpolation.md#values-from-data-are-literal) |

## Context

A value from `--data`, `--set`, a data layer or `env()` could be handed to a tool as text, or as something the tool's own language reads, such as a Jinja or Bicep expression.

## The choice

It is always text. A plugin declares the openings of the tool's own expression language, and the core escapes each one in a placed value, so a data value can never run anything on the control machine or in the tool.

## Alternatives

Let a document mark a value as an expression for the tool.
