# A notify's `on` accepts only `changed`

| | |
|---|---|
| Area | Language |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 39 |
| Where it shows | [DSL reference: notifies](../reference/dsl/index.md#notifies) |

## Context

`notifies` carries an optional `on`, the event that fires the notification. The design left room for more than one event.

## The choice

Only `changed` is accepted, and leaving `on` out means the same. Any other word is refused at validation with the accepted list.

## Alternatives

Accept `always` as well, or accept any word and let each target decide.
