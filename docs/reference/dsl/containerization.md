# The containerization flavour

How the [Meridian language](index.md) is written for tools that run containers on one
host.

| | |
|---|---|
| Category, as `meridian plugins` prints it | `containerization` |
| Kind of document | `Infrastructure`. A `ResourceSet` is also taken: the project then runs on the daemon of the host its `targetHost` names |
| `type` and `params` | the tool's own, passed through |
| Outputs | yes |

## Targets

| Target | `type` is | Settings resource | Page |
|---|---|---|---|
| `docker` | an element of a Compose file: `service`, `network`, `volume`, `config`, `secret` | `project` (optional) | [docker](../../targets/docker.md) |
| `podman` | a kind of Quadlet unit: `container`, `pod`, `kube`, `volume`, `network`, `image`, `build` | none; the runner is set through the environment | [podman](../../targets/podman.md) |

## Two tools, one shape

A resource is one element, `params` are its fields in the tool's spelling, and elements
find each other by `id`.

| | `docker` | `podman` |
|---|---|---|
| Compiles to | one Compose file | one Quadlet unit file per resource |
| Run by | `docker compose`, talking to a daemon | systemd, with no daemon |
| Params are spelt | as the Compose specification has them: `image`, `ports` | as Quadlet has them: `Image`, `PublishPort` |
| `dependsOn` | `depends_on` between services | `Requires=` and `After=` |
| `notifies` | `restart`, native or synthetic | `restart`, native: systemd's `PartOf=` |
| Another machine | the `project` resource, or a ResourceSet's `targetHost` | a ResourceSet's `targetHost`, over `ssh` |

Examples use Docker; see Podman's page for Quadlet.

## A document is a project

A document is one project. Its name is the project name unless the settings resource
gives another. A resource's `id` is the element's name in the compiled file.

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web
spec:
  target: docker
  resources:
    - id: backend
      type: network

    - id: db_data
      type: volume

    - id: db
      type: service
      params:
        image: postgres:16
        networks: ["${backend}"]
        volumes: ["${db_data}:/var/lib/postgresql/data"]

    - id: app
      type: service
      params:
        image: ghcr.io/example/app:1.4
        networks: ["${backend}"]
        ports: ["8080:8080"]
      dependsOn:
        - resource: db

  outputs:
    - name: app_port
      value: ${app.port.8080}
```

[`examples/flavours/containerization.yaml`](../../../examples/flavours/containerization.yaml).
Longer: [`examples/docker/web.yaml`](../../../examples/docker/web.yaml).

## The settings resource

An optional `project` resource says where and how the project runs.

| Param | Meaning |
|---|---|
| `name` | the project's name; the document's name when left out |
| `host`, `context` | the daemon to use: a host reached over SSH, an address, or a Docker context |
| `directory` | an absolute path that Compose resolves relative paths against |
| `profiles` | a list of Compose profiles to enable |
| `wait` | whether an apply waits for services to be running or healthy. On by default |
| `wait_timeout` | how long that wait may take, in seconds |

A ResourceSet's `targetHost` also selects the daemon, over SSH.

## `state`

Left out. Undeclared elements are removed; `state: absent` is refused.

## References

In `params`, `${id}` is another element's name, and nothing more.

In an output, a reference reads the running project:

| You write | You get |
|---|---|
| `${app.port.8080}` | the host port the service's port 8080 is published on |
| `${db.State.Status}` | that path in the inspection of the service's container |

A `$` in a param is escaped, so the tool does not read it as a variable.

## Order

Between services, `dependsOn` becomes `depends_on`: wait for healthy if the other has a
healthcheck, otherwise for started. A dependency on a network, volume, config or secret
adds nothing and warns.

## What is checked at compile time

Refused: an attribute Compose does not know, an undeclared network, volume, config or
secret, and dependency cycles.

## `notifies`

One action, `restart`, aimed at a service.

| Notifier | How |
|---|---|
| a service | native: `depends_on` with `restart: true` |
| a config, secret, network or volume | synthetic: a checksum of the notifier in a label on the service, `meridian.dev/restart-on-<id>`. The content changes, the label changes, the container is recreated. The compile warns |

## `runtimeWhen`

Refused.

## `adopt`

Refused. Compose and Quadlet units both refuse a container of the same name
that they did not create.

## Plan and apply

On `docker`, `plan` compares each service's Compose configuration hash with the running
containers' (uncertain for services built from source). `apply` brings the project up,
removes undeclared services, waits, and reads outputs. On `podman`, `plan` compares unit
files.

## How this differs from container orchestration

Here the tool brings one project up on one host while Meridian waits. In
[container orchestration](container-orchestration.md), a cluster converges objects on its
own schedule.
