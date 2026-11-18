# A secret has one place on each containers target

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Phase 7 part 5: secrets handed to Docker, Podman, Kubernetes and Helm" |
| Where it shows | [Docker](../targets/docker.md#secrets), [Podman](../targets/podman.md#secrets), [Kubernetes](../targets/kubernetes.md#secrets), [Helm](../targets/helm.md#secrets), [Secrets](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) |

## Context

The [language expansion plan](../plans/language-expansion.md#116-handing-a-secret-to-a-tool)
hands every target a secret by its tool's most private channel, standard input first and a
private file second, never the command line. On the configuration and infrastructure targets
a secret can go in any param, because the tool reads every param from a variables file. The
containers tools are different: a manifest, a Compose file and a unit file are *all* read by
more than the tool, and the tool keeps or shows most of what it is given.

## The choice

Each containers target takes a secret in exactly one place, the one the tool itself means
for a secret, and refuses it everywhere else at compile (`unsupported-secret-place`):

- `kubernetes`: a value of a `v1/Secret`'s `stringData`, filled in on `kubectl`'s standard input.
- `helm`: a value in the `values`, filled in on `helm`'s standard input; not when the document
  has a Kustomize overlay, because the rendered manifests then go through a file.
- `docker`: a service's params or a `secret` element's `value`, which Compose reads from a
  `0600` environment file given as `--env-file`. This is the plan's rank 2, not its rank 1:
  Compose has no standard-input channel for a value, and engine secrets belong to Swarm.
- `podman`: the whole value of a container's `Environment` variable, which becomes a Podman
  secret created on the host from the apply script's standard input and named by the unit's
  `Secret=` line.

On `kubernetes` and `helm` the compiled file holds a token the runner fills in, so a document
that writes the token's text itself is refused (`reserved-text`).

## Alternatives

- Take a secret in any param and fill it in wherever it is, as the other targets do. Refused:
  on these targets the value would land in a Deployment's environment, a Compose file
  `docker compose config` prints, or a unit file every user of the host can read.
- Refuse secrets on the containers targets altogether, as `cloud-init` and `dsc` do. Refused:
  a database password is the first thing a Kubernetes or Compose document needs.
- For Compose, create engine secrets with `docker secret create` from standard input (rank 1).
  Refused: those exist only in Swarm mode.
