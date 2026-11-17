# Podman output is Quadlet units only

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 36 |
| Where it shows | [Podman](../targets/podman.md) |

## Context

Podman has no daemon and no `apply` command. Its declarative form is Quadlet unit files, and its imperative form is `podman run`.

## The choice

A document compiles to Quadlet unit files, and apply installs them for systemd. There is no `podman run` path.

## Alternatives

Drive `podman` directly, as the Docker target drives the Docker daemon.
