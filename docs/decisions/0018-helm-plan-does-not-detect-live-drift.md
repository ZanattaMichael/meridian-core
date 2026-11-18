# Helm's plan does not detect live drift

| | |
|---|---|
| Area | Targets |
| Status | **accepted** 2026-11-18 |
| Made in | PR 36 |
| Where it shows | [Helm](../targets/helm.md) |

## Context

A Helm plan can compare with what Helm last installed, or with what is in the cluster now.

## The choice

The plan compares with Helm's last release. A change made to the cluster behind Helm's back is not seen.

## Alternatives

A server-side dry run against the live objects, which is slower and needs cluster access at plan time.
