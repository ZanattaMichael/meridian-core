# An `adopt: replace` removes what exists and makes it again from the document

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | the pull request "docker: `adopt: replace` takes over an existing container" (issue 269, first part) |
| Where it shows | [Adopting what exists](../reference/dsl/resources.md#adopting-what-exists), [Docker](../targets/docker.md), the [capability matrix](../reference/capability-matrix.md) |

## Context

Issue 269 asks each of CloudFormation, Helm, Docker and Podman to take over what
already exists. Docker and Podman keep no record of what they manage, and
neither takes a running container under management as it is: Compose refuses a
container it did not create, and a Quadlet unit refuses one it did not make. So
Meridian can neither import such a container by an identifier (there is none) nor
pretend it manages one the tool then leaves alone.

## The choice

- **A fourth adoption value, `replace`.** An author writes `adopt: replace`, and
  the target refuses any other value on that target with `invalid-adopt`: docker
  keeps no record of what it manages and cannot import a container by an id. The
  value crosses the protocol as `ADOPTION_REPLACE`, a value a plugin built
  before it reads as `none`, so nothing is adopted by a plugin that cannot.
- **The removal is Meridian's, not the tool's.** Before `docker compose up`, the
  runner runs `docker compose rm --stop --force <service>` for each service that
  carries `adopt: replace`, so `up` makes the container again from the document
  rather than Compose deciding the file matches and leaving the old one. The
  plan reports such a service as `adopt`, so the removal is shown before it
  happens.
- **A separate value from `converge`.** `converge` accepts an identifier and
  changes nothing, because the tool makes the thing what the document says
  anyway. `replace` destroys and recreates, which is a stronger promise a user
  must ask for with the word `replace`.

## Alternatives

- Reuse `converge`. Rejected: a converging target accepts an identifier and
  warns that it changes nothing; Docker cannot accept an existing container at
  all, so a plain `adopt` must be refused, and accepting it would claim a
  management Compose does not honour (the one rule in CONTRIBUTING).
- Reuse `import`. Rejected: neither Docker nor Podman can import a container by
  an identifier.
- Write a Compose label and let `up` recreate. Rejected: `up` recreates only
  when the configuration hash changes, so an unchanged file would leave the
  existing container alone and silently adopt nothing.

## Still open

The owner settled (2026-11-25) that the first `adopt: replace` should require an
explicit confirmation, as `destroy` does. That confirmation is a run-level
feature, not a target one, and is not in this first part: today `meridian plan`
shows the `adopt` line and the Docker target page says the container is removed.
It is to be built with the CloudFormation, Helm and Podman parts of issue 269, so
that it covers every `replace` target at once.
