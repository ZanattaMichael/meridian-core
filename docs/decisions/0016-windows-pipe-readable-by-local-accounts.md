# The Windows named pipe can be opened for reading by any local account

| | |
|---|---|
| Area | Releases |
| Status | **accepted** 2026-11-18 |
| Made in | PR 61 |
| Where it shows | [Windows quickstart](../quickstart/windows.md) |

## Context

A plugin listens on a named pipe. Its access control decides who can send a request and who can see one.

## The choice

Only the user's own account, administrators and the system can write, so no other account can send a plugin a request. Another local account can still open the pipe for reading.

## Alternatives

Deny every other account, at the cost of a stricter descriptor that some tools cannot open.
