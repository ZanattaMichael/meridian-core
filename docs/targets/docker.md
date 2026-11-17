# Target: `docker`

<img src="../branding/logos/meridian-docker.svg" width="72" alt="Meridian Docker plugin logo">

Compiles a document into a Docker Compose file and brings it up with
`docker compose`. An Infrastructure document runs on the local Docker daemon,
or the one its `project` names. A ResourceSet runs on its target host's daemon.

This is Meridian's Docker Compose support. There is no separate `compose`
target.

On this page:
[support at a glance](#what-it-does-and-does-not-support) ·
[before you start](#before-you-start) ·
[a first document](#a-first-document) ·
[resource types](#resource-types) ·
[the project](#the-project) ·
[fields every resource has](#fields-every-resource-has) ·
[compile-time checks](#what-is-checked-at-compile-time) ·
[warnings](#warnings) ·
[dependencies](#dependencies) ·
[notifications](#notifications) ·
[relative paths](#relative-paths) ·
[references](#references) ·
[outputs](#outputs) ·
[compiled files](#what-is-compiled) ·
[running](#running) ·
[examples](#examples) ·
[troubleshooting](#troubleshooting) ·
[tests](#how-it-is-tested)

## What it does and does not support

| | |
|---|---|
| Resource types | the things a Compose file declares: `service`, `network`, `volume`, `config`, `secret`, plus `project` for where it runs |
| Params | every attribute of the Compose specification for that element, and extension fields (`x-`) |
| `state` | `present` (the default) and `running`, which mean the same here. `absent` is refused |
| `dependsOn` | Between two services, Compose's own `depends_on`, waiting for the other service to be healthy when it declares a healthcheck. Anything else Compose already creates before it starts a service |
| `notifies` | The action `restart`, aimed at a service. Native from a service, synthetic from anything else. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused |
| `state: absent` | No. Remove the resource from the document; bringing the project up removes services the document no longer declares |
| Plan | Yes: the file is compared with what Docker is running. See [Running](#running) |
| Outputs | Yes: a published port, or a path into `docker inspect` |
| Compose variables (`${VAR}` from the shell or a `.env` file) | No. A `$` is written literally. See [References](#references) |
| Several Compose files, `include`, top-level `models` | No. One document is one Compose file |

## Before you start

- `docker` with the Compose plugin (`docker compose version` works) on the
  machine that runs `meridian`. Compose v2.23 or later is needed for inline
  config `content`; the plugin is tested with Compose 5.
- A daemon to talk to. With no `project` and no target host that is the local
  one, or whatever `DOCKER_HOST` and your current Docker context select.
- For another machine, SSH access that works without a prompt
  (`docker --host ssh://deploy@web01 ps` succeeds). Docker is needed on that
  machine; Meridian is not.

Meridian stores no credentials. Registry logins are the ones `docker login`
already saved.

## A first document

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: shop
spec:
  target: docker
  resources:
    - id: backend
      type: network
      params: {}

    - id: db_data
      type: volume
      params: {}

    - id: db
      type: service
      params:
        image: postgres:17
        environment:
          POSTGRES_PASSWORD: example
        healthcheck:
          test: [CMD-SHELL, pg_isready -U postgres]
          interval: 5s
        networks: ["${backend}"]
        volumes: ["${db_data}:/var/lib/postgresql/data"]

    - id: web
      type: service
      params:
        image: nginx:1.27
        ports: ["8080:80"]
        networks: ["${backend}"]
      dependsOn:
        - resource: db

  outputs:
    - name: url
      value: http://localhost:${web.port.80}
```

```console
$ meridian compile shop.yaml --out build    # writes build/shop/compose.yaml and project.json
$ meridian plan shop.yaml                   # compares the file with what Docker is running
$ meridian apply shop.yaml                  # docker compose up, then reads the outputs
```

The compiled `compose.yaml`:

```yaml
name: shop
services:
  db:
    image: postgres:17
    environment:
      POSTGRES_PASSWORD: example
    healthcheck:
      interval: 5s
      test:
        - CMD-SHELL
        - pg_isready -U postgres
    networks:
      - backend
    volumes:
      - db_data:/var/lib/postgresql/data
  web:
    image: nginx:1.27
    depends_on:
      db:
        condition: service_healthy
    networks:
      - backend
    ports:
      - 8080:80
networks:
  backend: {}
volumes:
  db_data: {}
```

A ResourceSet looks the same, with `kind: ResourceSet` and the host it runs on.
Meridian reaches that host's daemon over SSH; `localhost` is the machine
Meridian runs on:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: monitoring
spec:
  target: docker
  targetHost:
    from: inline
    host: deploy@mon01.example.com
  resources:
    - id: prometheus
      type: service
      params:
        image: prom/prometheus:v3.0.0
```

## Resource types

A resource's `type` is the section of the Compose file it belongs to and its
`params` are that element's fields, exactly as the
[Compose specification](https://docs.docker.com/reference/compose-file/)
documents them. The resource's id is the element's name in the file, and the
name other containers resolve a service by.

Params are passed through as written: Meridian does not rename them, fill in
defaults or change their form. It checks their names, and the few values listed
under [compile-time checks](#what-is-checked-at-compile-time). Whether a value
is one Compose accepts (a port syntax, a duration) is checked by Compose itself
when the project is planned.

Any param whose name starts with `x-` is an extension field. It is yours to
name and is written to the file untouched.

### `service`

A container, or several from one image. It needs one of `image`, `build`,
`extends` or `provider`.

Accepted params:

`annotations`, `attach`, `blkio_config`, `build`, `cap_add`, `cap_drop`,
`cgroup`, `cgroup_parent`, `command`, `configs`, `container_name`, `cpu_count`,
`cpu_percent`, `cpu_period`, `cpu_quota`, `cpu_rt_period`, `cpu_rt_runtime`,
`cpu_shares`, `cpus`, `cpuset`, `credential_spec`, `depends_on`, `deploy`,
`develop`, `device_cgroup_rules`, `devices`, `dns`, `dns_opt`, `dns_search`,
`domainname`, `driver_opts`, `entrypoint`, `env_file`, `environment`, `expose`,
`extends`, `external_links`, `extra_hosts`, `gpus`, `group_add`, `healthcheck`,
`hostname`, `image`, `init`, `ipc`, `isolation`, `label_file`, `labels`,
`links`, `logging`, `mac_address`, `mem_limit`, `mem_reservation`,
`mem_swappiness`, `memswap_limit`, `models`, `network_mode`, `networks`,
`oom_kill_disable`, `oom_score_adj`, `pid`, `pids_limit`, `platform`, `ports`,
`post_start`, `pre_stop`, `privileged`, `profiles`, `provider`, `pull_policy`,
`read_only`, `restart`, `runtime`, `scale`, `secrets`, `security_opt`,
`shm_size`, `stdin_open`, `stop_grace_period`, `stop_signal`, `storage_opt`,
`sysctls`, `tmpfs`, `tty`, `ulimits`, `use_api_socket`, `user`, `userns_mode`,
`uts`, `volumes`, `volumes_from`, `working_dir`.

The ones Meridian reads as well as passes through:

| Param | What Meridian does with it |
|---|---|
| `image`, `build`, `extends`, `provider` | one of them must be present |
| `build` | a relative context is warned about; an apply adds `--build`; a plan cannot be certain. See [Running](#running) |
| `healthcheck` | decides what a dependency on this service waits for. `disable: true` counts as none |
| `depends_on` | checked, and added to by `dependsOn` and `notifies`. See [Dependencies](#dependencies) |
| `networks`, `volumes`, `configs`, `secrets` | every name used must be declared in the document |
| `env_file`, `volumes` | a relative host path is warned about |
| `labels` | a synthetic notify adds one label. Must be a map or a list when the service is notified that way |
| `scale`, `deploy.replicas` | how many containers a plan expects |
| `ports` | what `${service.port.N}` in an output reads |

```yaml
- id: api
  type: service
  params:
    image: ghcr.io/example/api:2.3.0
    command: [serve, --port, "8000"]
    environment:
      LOG_LEVEL: info
    ports: ["8000"]
    restart: unless-stopped
    healthcheck:
      test: [CMD, curl, -f, http://localhost:8000/health]
      interval: 10s
      retries: 5
    deploy:
      replicas: 2
    x-owner: platform-team
```

### `network`

Accepted params: `attachable`, `driver`, `driver_opts`, `enable_ipv4`,
`enable_ipv6`, `external`, `internal`, `ipam`, `labels`, `name`.

```yaml
- id: backend
  type: network
  params:
    internal: true

- id: shared
  type: network
  params:
    external: true
    name: company-shared     # a network that already exists, under its real name
```

The network `default` exists in every Compose project. A service may name it
without the document declaring it; declare a `network` with the id `default` to
change its settings.

### `volume`

Accepted params: `driver`, `driver_opts`, `external`, `labels`, `name`.

```yaml
- id: db_data
  type: volume
  params: {}

- id: media
  type: volume
  params:
    driver: local
    driver_opts:
      type: nfs
      o: addr=10.0.0.9,rw
      device: ":/exports/media"
```

Only a named volume is a resource. A bind mount (`/srv/data:/data`) and an
anonymous volume (`/data`) are written in the service's `volumes` and need no
declaration.

### `config`

Accepted params: `content`, `environment`, `external`, `file`, `labels`,
`name`, `template_driver`.

```yaml
- id: site
  type: config
  params:
    content: |
      server { listen 80; }
  notifies:
    - resource: proxy
      action: restart
```

`content` keeps the configuration in the document. `file` reads it from the
host the daemon runs on; a relative `file` is
[warned about](#relative-paths).

### `secret`

Accepted params: `driver`, `driver_opts`, `environment`, `external`, `file`,
`labels`, `name`, `template_driver`.

```yaml
- id: db_password
  type: secret
  params:
    environment: DB_PASSWORD     # read from the environment meridian runs in

- id: tls_key
  type: secret
  params:
    file: /etc/ssl/private/site.key
```

A secret's value never appears in the document or the compiled file, only where
Compose should read it from.

## The project

One resource may have the type `project`. It is not part of the Compose file:
it says what the project is called, where it runs and how it is brought up. A
document with none uses the defaults in the table.

| Param | Value | Default | Meaning |
|---|---|---|---|
| `name` | string | the document's `metadata.name` | the Compose project name. Lower-case letters, digits, hyphens and underscores, starting with a letter or digit |
| `host` | string | the document's target host, or the local daemon | the Docker daemon to use. A bare host such as `deploy@web01` is reached over SSH (`ssh://deploy@web01`); an address with a scheme, such as `tcp://10.0.0.5:2376`, is used as written; `localhost`, `127.0.0.1` and `::1` mean the local daemon |
| `context` | string | none | a Docker context to use instead of a host. Naming both is refused |
| `directory` | string, an absolute path starting with `/` or `~` | none | the directory Compose resolves the file's relative paths against. See [Relative paths](#relative-paths) |
| `profiles` | list of strings | none | Compose profiles to bring up with the project. Services with a `profiles` param run only when one of theirs is listed |
| `wait` | `true` or `false` | `true` | whether an apply waits for every service to be running, or healthy where it declares a healthcheck |
| `wait_timeout` | whole number of seconds, greater than zero | Compose's own limit | how long to wait before the apply fails |

```yaml
- id: target
  type: project
  params:
    name: shop-staging
    host: deploy@staging01.example.com
    directory: /srv/shop
    profiles: [metrics]
    wait_timeout: 120
```

Rules:

- A document has at most one `project`. A second is refused
  (`duplicate-project`).
- Any other param is refused (`unsupported-param`), and a value of the wrong
  kind is refused (`invalid-param`).
- A `project` cannot depend on anything or be depended on
  (`unsupported-dependency`): it is not something Docker creates.
- The document's name is used as the project name when `name` is not set. If
  it is not a valid Compose project name, such as `Web Shop`, the compile is
  refused (`invalid-name`) and says to set `name`.

## Fields every resource has

| Field | On this target |
|---|---|
| `id` | the element's name in the Compose file |
| `type` | one of the six above. Anything else is refused (`unsupported-resource-type`) |
| `params` | the element's Compose attributes |
| `state` | `present` or `running`, or left out. Both mean the element is in the file. `absent` and anything else are refused (`unsupported-state`) |
| `dependsOn` | see [Dependencies](#dependencies) |
| `notifies` | see [Notifications](#notifications) |
| `runtimeWhen` | refused (`unsupported-runtime-condition`): a Compose file is declared state, with no condition for Docker to evaluate |

A document must declare at least one service, or there is nothing to run
(`empty-project`).

## What is checked at compile time

Compose would refuse most of these itself, but only when the project is planned
or brought up, and in terms of the compiled file. Meridian refuses them when the
document compiles, against the line you wrote. A refusal stops the compile;
nothing is written and nothing runs.

| The document | Is refused with |
|---|---|
| has a resource of a type other than the six | `unsupported-resource-type` |
| has two resources of type `project` | `duplicate-project` |
| gives the project a param it does not take | `unsupported-param`, listing the ones it does |
| gives a project param a value of the wrong kind, a relative `directory`, or a `wait_timeout` of zero | `invalid-param` |
| names both a `host` and a `context` | `invalid-param` |
| has a project name Compose would not accept | `invalid-name` |
| declares no service | `empty-project` |
| has a service with none of `image`, `build`, `extends`, `provider` | `missing-param` |
| uses an attribute Compose does not know, such as `enviroment` | `unsupported-param`, naming the attribute that was probably meant. An extension field, which starts with `x-`, is yours to name |
| has a service that uses a network, named volume, config or secret it does not declare | `invalid-reference`. The network `default` needs no declaration, and neither does a bind mount or an anonymous volume |
| has a service that uses a name as the wrong kind, such as a volume's id under `networks` | `invalid-reference` |
| has a `${...}` in a param that names nothing in the document, or reads a path such as `${db.State.Status}` | `invalid-reference`. A path can only be read in an output |
| has a `depends_on` naming something that is not a service, or the service itself | `invalid-reference` |
| has a `depends_on` condition other than `service_started`, `service_healthy`, `service_completed_successfully` | `invalid-param` |
| waits for `service_healthy` on a service with no healthcheck | `invalid-param`, because Compose would wait for ever |
| has services that wait for each other in a circle | `dependency-cycle`, showing the circle. A `restart` notify counts, because the notified service starts after the one that notifies it |
| has a `dependsOn` to or from the `project` | `unsupported-dependency` |
| has a `notifies` with an action other than `restart`, or aimed at something that is not a service | `unsupported-action` |
| has a `dependsOn` or `notifies` on a service whose `depends_on` is neither a list of names nor a map, or a synthetic notify on a service whose `labels` is neither a map nor a list | `invalid-param` |
| uses `state: absent`, or a state other than `present` and `running` | `unsupported-state` |
| uses `runtimeWhen` | `unsupported-runtime-condition` |
| has an output that names something that is not a service, or is not a port or an inspect path | `invalid-reference` |

```yaml
- id: db
  type: service
  params:
    image: postgres:17
    volumes: ["dbdata:/var/lib/postgresql/data"]   # the document declares db_data
```

```
stack.yaml:11:7: docker: resource "db" [invalid-reference]: uses the volume "dbdata", which the document does not declare. Add a resource of type "volume" with that id
```

```yaml
- id: web
  type: service
  params:
    image: nginx:1.27
    enviroment: {MODE: production}
```

```
stack.yaml:28:7: docker: resource "web" [unsupported-param]: "enviroment" is not an attribute of a Compose service; did you mean "environment"?
```

What is not checked: the values of attributes Meridian only passes through, and
anything that needs the daemon, such as whether an image exists or an external
network is there. Compose reports those at plan time, and the plan fails with
what Compose said.

## Warnings

A warning does not stop the compile. `meridian compile`, `plan` and `apply`
print each one.

| Warning | Why |
|---|---|
| a relative build context, `env_file`, bind mount, or config or secret `file`, when the project has no `directory` | Compose would resolve it against Meridian's work directory. See [Relative paths](#relative-paths) |
| a service restarts by being recreated when a config, secret, network or volume changes | a synthetic notify was compiled to a label. See [Notifications](#notifications) |
| a `dependsOn` that orders nothing | it is on a network, volume, config or secret. Compose creates all of those before it starts any service, in no order among themselves |

## Dependencies

| `dependsOn` | Compiles to |
|---|---|
| a service on a service | `depends_on` on the dependent service, in Compose's long form |
| a service on a network, volume, config or secret | nothing: Compose creates those first anyway |
| a network, volume, config or secret on anything | nothing, with a [warning](#warnings) |
| anything on the `project`, or the `project` on anything | refused |

Between two services the condition is chosen from the service being waited
for:

| The other service | Condition written |
|---|---|
| declares a `healthcheck` | `service_healthy` |
| declares none, or disables it | `service_started` |

```yaml
- id: db
  type: service
  params:
    image: postgres:17
    healthcheck:
      test: [CMD-SHELL, pg_isready -U postgres]
      interval: 5s

- id: app
  type: service
  params:
    image: ghcr.io/example/app:1.4.2
  dependsOn:
    - resource: db
```

```yaml
app:
  image: ghcr.io/example/app:1.4.2
  depends_on:
    db:
      condition: service_healthy
```

For anything else, write `depends_on` in the service's `params` as Compose
documents it. An entry you write for a service is kept as written, and a
`dependsOn` on that same service adds nothing to it. A `dependsOn` on another
service adds its own entry beside yours. A `depends_on` written as a list of
names is rewritten in the long form, each waiting for `service_started`. This is
how a service waits for a job that runs once:

```yaml
- id: migrate
  type: service
  params:
    image: ghcr.io/example/app:1.4.2
    command: [migrate]
    restart: "no"

- id: app
  type: service
  params:
    image: ghcr.io/example/app:1.4.2
    depends_on:
      migrate:
        condition: service_completed_successfully
```

A `depends_on` you write is checked like one Meridian writes: it must name
services of the document, wait on a condition Compose has, not wait for health
that will never be reported, and not close a circle.

## Notifications

The one action is `restart`, and the one thing that can be restarted is a
service.

A service notifying a service is Compose's own mechanism: the notified service
gets `depends_on` on the notifier with `restart: true`, and Compose restarts it
after the other is updated. The condition is chosen as for a
[dependency](#dependencies).

```yaml
- id: api
  type: service
  params:
    image: ghcr.io/example/api:2.3.0
  notifies:
    - resource: web
      action: restart
```

```yaml
web:
  depends_on:
    api:
      condition: service_started
      restart: true
```

Because the notified service now starts after the notifier, a notify from `api`
to `web` together with a `dependsOn` from `api` on `web` is a circle, and is
refused.

A config, secret, network or volume notifying a service is synthetic. Compose
does not restart a service because a config it mounts changed. Meridian writes a
checksum of the notifier's params into a label on the service,
`meridian.dev/restart-on-<id>`. When the notifier's params change the label
changes, and Compose recreates the container. The compile warns that this is how
it works, and `docker inspect` will show the label. The checksum covers what the
document says, so a config read from a `file` restarts the service when the
param changes and not when the file's content does.

## Relative paths

Compose resolves a relative host path against the project directory, which is
the directory Meridian keeps the compiled file in, under its state directory.
Nothing you keep beside the document is there. A build context, an `env_file`, a
bind mount, or the `file` of a config or secret that is a relative path compiles
with a warning for each:

```
docker: web: the build context "./web" is a relative path, which Compose resolves against the directory Meridian keeps the compiled file in and not the document's. Write an absolute path, or name the directory with "directory" on a resource of type "project"
```

What counts as a path:

| Written | Is |
|---|---|
| `/srv/web`, `~/web`, `C:\web` | absolute; no warning |
| `./web`, `../web`, `web` as a build context, `web.env` as an `env_file` | relative; warned about |
| `https://github.com/example/app.git`, `git@github.com:example/app.git` as a build context | not a path; no warning |
| `./public:/usr/share/nginx/html` in `volumes` | a relative bind mount; warned about |
| `data:/var/lib/data` in `volumes` | the named volume `data`, which must be declared |
| `/var/lib/data` alone in `volumes` | an anonymous volume |

Write absolute paths, or say where the relative ones start:

```yaml
- id: target
  type: project
  params:
    directory: /srv/shop

- id: web
  type: service
  params:
    build: ./web                      # /srv/shop/web
    env_file: web.env                 # /srv/shop/web.env
    volumes: ["./public:/usr/share/nginx/html:ro"]
```

With a `directory` set there are no warnings. The directory is on the machine
that runs `docker compose` for build contexts and `env_file`, and on the
daemon's machine for bind mounts.

## References

Inside `params`, `${id}` is the name of another element of the document, checked
at compile time. Use it wherever Compose expects that name: under `networks`,
as the source of a volume mount, config or secret, or as a host name in an
environment variable.

```yaml
environment:
  DATABASE_HOST: ${db}                # db
volumes:
  - ${db_data}:/var/lib/postgresql/data
```

Nothing else can be read in a param, because nothing else is known until the
project is up. `${db.State.Status}` in a param is refused; read it in an
[output](#outputs). Writing the bare name (`db_data:/var/lib/...`) works too
and is checked the same way.

A `$` in a param is written to the Compose file as `$$`, so Compose does not
take it for a variable of its own. A command such as `sh -c 'echo $HOME'`
reaches the container as written. Values belong in the document, not in the
shell environment of whoever runs it, so Compose's own `${VAR}` interpolation
is not available.

## Outputs

An output is read from Docker after an apply. Each `${...}` in its value must
start with the id of a service.

| You write | You get |
|---|---|
| `${proxy.port.80}` | the host port that the service's TCP port 80 is published on |
| `${dns.port.53/udp}` | the same for a UDP port. `/tcp` may be written too |
| `${db.State.Status}`, `${db.NetworkSettings.Networks.web_backend.IPAddress}` | that path in `docker inspect` of the service's container |
| `${db.Mounts[0].Source}` | a path may index into a list |

A value may mix text and several references:

```yaml
outputs:
  - name: url
    value: http://localhost:${proxy.port.80}/
  - name: db_state
    value: ${db.State.Status}
```

An inspect path is fields and indexes only: letters, digits, `.`, `_`, `-` and
`[n]`. A network's key under `Networks` is its name as Docker knows it, which
is the project name, an underscore and the network's id (`web_backend`).

When a service has several containers the first one Compose lists is read.

An output with nothing behind it is an error and not an empty value: a port the
service does not publish, a service with no container, or a path with nothing
at it.

## What is compiled

| File | Holds |
|---|---|
| `compose.yaml` | the Compose file: `name`, then `services`, `networks`, `volumes`, `configs`, `secrets`, each element in the order the document declares it. A service's `image`, `build`, `command` and `depends_on` come first and the rest alphabetically |
| `project.json` | what the runner needs that is not Compose's: the project's name, host, context, directory, profiles, wait settings, and the outputs to read |

The same document always compiles to the same bytes. `meridian compile --out
build` writes both files to `build/<document name>/`, and the Compose file there
can be read, or run by hand with `docker compose -f`.

## Running

Every call is `docker compose --project-name <name> --file compose.yaml`, with
`--host` or `--context` before `compose` when the project names one.

A plan changes nothing. It does not parse what `docker compose up --dry-run`
prints. It asks for data and compares it the way Compose itself does: Compose
stamps every container it creates with a hash of its service's configuration,
and recreates the container when the hash of the file's service differs.

| A plan runs | To learn |
|---|---|
| `docker compose config --format json` | what the file asks for, as Compose reads it. A file Compose rejects fails here, with what Compose said |
| `docker compose config --hash '*'` | the configuration hash of each service in the file |
| `docker ps --all`, `docker network ls`, `docker volume ls`, filtered to the project | what exists now, and the hash each container was created from |

| The plan reports | When |
|---|---|
| `create` | a service has no container, or a network or volume of the file does not exist |
| `replace` | a service's configuration hash differs from its container's, so the container is recreated. What was in the container, outside its volumes, is lost |
| `update` | a container is not running and would be started, or the number of containers differs from the service's `scale` or `deploy.replicas` |
| `delete` | containers belong to a service the document no longer declares, or a service is scaled to none |

Containers started with `docker compose run` are not counted. Networks and
volumes the document no longer declares are left alone, as `docker compose up`
leaves them; remove them with `docker network rm` or `docker volume rm`.

A plan with no changes means the project is as the document describes. The one
exception is a service built from source (`build`): whether building again would
give a different image cannot be known without building it, so the plan says it
cannot be sure.

An apply makes the same comparison, so it reports what the plan would have, and
then runs `docker compose up --detach --remove-orphans`, with:

| Flag | When |
|---|---|
| `--wait` | unless the project sets `wait: false`. The apply succeeds once every service is running, or healthy where it declares a healthcheck, and fails if one does not get there |
| `--wait-timeout <seconds>` | when the project sets `wait_timeout` |
| `--build` | when a service is built from source, so a change to the source is applied and not only a change to the file |
| `--project-directory`, `--profile` | when the project sets `directory` or `profiles`; these go on every call |

Set `wait: false` for a project whose services are meant to exit, such as a job
that runs once with nothing depending on it: Compose's wait treats a service
that has stopped as one that failed to stay up.

The outputs are then read with `docker compose port`, `docker compose ps` and
`docker inspect`.

Nothing here takes a project down. To remove one, run
`docker compose --project-name <name> down` yourself, with `--volumes` to
delete its data too.

| Variable | Meaning |
|---|---|
| `MERIDIAN_DOCKER` | the program to run, when it is not `docker` on `PATH` |
| `MERIDIAN_DOCKER_ARGS` | arguments added to `docker compose up`, separated by spaces: `--pull always`, `--force-recreate` |

Docker's own variables, such as `DOCKER_HOST` and `DOCKER_CONFIG`, apply as
they do to any `docker` command when the project names no host or context.

## Examples

[`examples/docker/monitoring.yaml`](../../examples/docker/monitoring.yaml) is a
ResourceSet: Prometheus, a node exporter and Grafana on the host the document
names, with Grafana waiting for Prometheus to be healthy. It compiles to
[`monitoring.compose_yaml`](../../plugins/docker/testdata/monitoring.compose_yaml).

[`examples/docker/web.yaml`](../../examples/docker/web.yaml) declares a database,
an application and a proxy on one network, and compiles to
[`web.compose_yaml`](../../plugins/docker/testdata/web.compose_yaml). The proxy:

```yaml
- id: site
  type: config
  params:
    content: |
      server { ... }
  notifies:
    - resource: proxy
      action: restart

- id: proxy
  type: service
  params:
    image: nginx:1.27
    ports:
      - "80"
    configs:
      - source: ${site}
        target: /etc/nginx/conf.d/default.conf
    networks:
      - ${backend}
  dependsOn:
    - resource: app
```

```yaml
proxy:
  image: nginx:1.27
  depends_on:
    app:
      condition: service_started
  configs:
    - source: site
      target: /etc/nginx/conf.d/default.conf
  labels:
    meridian.dev/restart-on-site: 185ed24724f46635
  networks:
    - backend
  ports:
    - "80"
```

## Importing

`meridian import compose.yaml` reads one Compose file and writes one Infrastructure
document. [Importing existing configuration](../operations/importing.md) is the guide. A file named `compose.yaml`, `compose.yml`,
`docker-compose.yaml`, `docker-compose.yml`, or one of those with a middle part such as
`compose.prod.yaml`, is recognised without `--from`.

| In the Compose file | In the document |
|---|---|
| Each service, network, volume, config and secret | a resource of that type, whose id is the element's name and whose params are its fields |
| `name` | the document's name |
| The directory the file is in | `directory` on a `project` resource, so a relative build context, bind mount or `env_file` keeps meaning what it meant |
| `depends_on` that waits the way `dependsOn` does: `service_healthy` on a service with a healthcheck, `service_started` (or the short form, a list of names) on one without | `dependsOn` |
| `depends_on` with `restart: true` | `notifies` with the action `restart`, on the service that is waited for |
| Any other `depends_on`: `service_completed_successfully`, `service_started` on a service that has a healthcheck, `required: false` | kept as the `depends_on` param, so the condition is exactly the one written |
| `$$` | `$`. The target writes every dollar sign literally, so the compiled file has `$$` again |
| A YAML anchor and its aliases | written out in full wherever it was used |

What is reported:

| In the Compose file | What happens | Level |
|---|---|---|
| `${VARIABLE}` and `$VARIABLE` | The text is carried literally and marked. A compiled file never asks Compose to substitute, so write the value in, or declare it in the service's `environment` | not translated |
| `include` | Left out. Merge the files with `docker compose config` and import what that prints | not translated |
| Two elements with the same name, such as a service and a volume both called `db` | The second is left out: ids are unique in a document. Rename one in the Compose file and import again | not translated |
| A name that cannot be an id, such as `my.service` | Left out, for the same reason | not translated |
| `version`, and top-level `x-` extension fields | Left out: neither is part of the project | note |

More than one Compose file is refused: Compose merges several files into one project by
rules of its own. Merge them first with `docker compose -f a.yaml -f b.yaml config`.

The round-trip test imports
[`shop/compose.yaml`](../../plugins/docker/testdata/import/shop/compose.yaml), compiles
the document, and compares the Compose file it compiles to with the original as Compose
reads them. The imported document is
[`shop.meridian.yaml`](../../plugins/docker/testdata/import/shop.meridian.yaml).

## Troubleshooting

| What you see | What to do |
|---|---|
| `"..." is not an attribute of a Compose service` | Check the spelling against the list under [`service`](#service). For a field of your own, start its name with `x-` |
| `uses the volume "...", which the document does not declare` | Add a resource of type `volume` with that id. If you meant a host directory, write it as a path: `./data:/data` or `/srv/data:/data` |
| `cannot be the name of a Compose project` | Set `name` on a `project` resource, in lower case |
| the apply fails after a long wait | A service did not become running or healthy. Read `docker compose --project-name <name> logs <service>`, fix the healthcheck, raise `wait_timeout`, or set `wait: false` |
| the apply fails for a job that runs once and exits | Set `wait: false`, or have another service depend on the job with `service_completed_successfully` |
| a file or build context is "not found" at plan or apply | It is a relative path. Set `directory` on the project or write an absolute path |
| the plan always says it cannot be sure | A service has `build`. Build and push the image elsewhere and name it with `image` for a plan that can be certain |
| the plan shows `replace` when only another service changed | The service's own configuration changed too, usually through a `restart` notify's label or a `depends_on` entry |
| `service "..." does not publish port N` | Add the port to the service's `ports`. An exposed port (`expose`) is not published |
| the plan cannot reach the daemon | Run `docker --host ssh://<host> ps` by hand. The SSH user must be able to run `docker` without a password prompt |

## How it is tested

Beside the compile and runner tests, the plugin is driven as a real process over
the plugin protocol, and end to end against a real Docker daemon: a project is
brought up, its outputs are read and the page it serves is fetched, a service is
changed and another removed, and a container stopped behind Compose's back is
found by the next plan. Those tests run wherever `docker compose` and a daemon
are available, which includes CI, and are skipped with a note where they are
not. `MERIDIAN_REAL_DOCKER=1` makes a missing Docker a failure, and
`MERIDIAN_REAL_DOCKER=0` skips them.

```console
$ cargo test -p meridian-target-docker                              # everything; real Docker when present
$ MERIDIAN_REAL_DOCKER=0 cargo test -p meridian-target-docker       # without Docker
$ UPDATE_GOLDEN=1 cargo test -p meridian-target-docker              # record the golden files again
```
