# One version of each module per project

| | |
|---|---|
| Area | Modules |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 62 |
| Where it shows | [Modules: version resolution](../reference/modules.md) |

## Context

Two modules may require the same module at different versions.

## The choice

One version serves the whole project, chosen to satisfy every requirement. Two requirements that cannot be satisfied by one version fail the install.

## Alternatives

Several versions side by side, as Cargo allows for different major versions.
