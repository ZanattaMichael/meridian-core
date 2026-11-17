# Registry logins are read only from Docker's `auths`

| | |
|---|---|
| Area | Modules |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 83 and PR 131 |
| Where it shows | [Modules: registries](../reference/modules.md) |

## Context

`docker login` stores a login in `config.json` under `auths`, or hands it to a credential helper.

## The choice

Only logins in the file's `auths` section are read; credential helpers are not called. A project can name a credential of its own in `meridian.yaml`.

## Alternatives

Call Docker's credential helpers as `docker` does.
