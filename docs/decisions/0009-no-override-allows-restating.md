# `no-override` allows restating a value and `final` does not

| | |
|---|---|
| Area | Compile rules |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 64 |
| Where it shows | [Compile rules reference: rules](../reference/compile-rules.md#rules) |

## Context

Two rules stop a lower layer from changing a value set by a higher one. A layer that sets the same value again could be a violation or not.

## The choice

`no-override` allows it: the value is unchanged. `final` refuses it: nothing below may mention the key at all.

## Alternatives

One rule, with a flag.
