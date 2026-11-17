# The lock file does not record who signed a module

| | |
|---|---|
| Area | Modules |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 85 |
| Where it shows | [Modules: signatures](../reference/modules.md#signatures) |

## Context

A lock file could pin the signer as well as the content.

## The choice

The lock file records the content hash. The signature is checked against the trusted keys when the module is fetched, and `meridian module verify` says who signed it. A changed signer with the same content is not a lock-file change.

## Alternatives

Record the key in the lock file, so a change of signer is a diff.
