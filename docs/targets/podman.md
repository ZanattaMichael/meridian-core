# Target: `podman`

<img src="../branding/logos/meridian-podman.svg" width="72" alt="Meridian Podman plugin logo">

Compiles a document into Podman
[Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html) unit files
and has systemd run them, on this machine or on another over `ssh`.

## What it does and does not support

| | |
|---|---|
| Resource types | the kinds of Quadlet unit: `container`, `pod`, `kube`, `volume`, `network`, `image`, `build` |
| `dependsOn` | `Requires=` and `After=`, left out where a reference already orders the units |
| `notifies` | The action `restart`, native: systemd's `PartOf=`. See [Notifications](#notifications) |
| `runtimeWhen` | No. Refused; use a `Condition*=` setting under `unit` |
| `adopt` | No. Refused: a Quadlet unit refuses a container of the same name it did not create |
| `state: absent` | No. Remove the resource from the document; the next apply stops and removes its unit |
| Plan | Yes: an exact comparison of unit files, plus a check by the host's own Quadlet |
| Outputs | Yes: names known at compile time, or a path into `podman inspect` |
| Documents | Infrastructure (this machine) and ResourceSet (its `targetHost`, over `ssh`) |
| Needs on the host | Linux with systemd, and Podman 4.4 or later (4.8 for image units, 5.0 for pods, 5.2 for builds) |

A plan compares the unit files on the host with the compiled ones.

## A first document

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: hello
spec:
  target: podman
  resources:
    - id: site
      type: volume

    - id: web
      type: container
      params:
        Image: docker.io/library/nginx:1.27
        PublishPort: "8080:80"
        Volume: ${site}:/usr/share/nginx/html:ro

  outputs:
    - name: url
      value: http://localhost:8080/
```

```
meridian compile hello.yaml --out build   # write site.volume and web.container; change nothing
meridian plan hello.yaml                  # what would be installed, changed, started or removed
meridian apply hello.yaml                 # install the units and start them
```

`meridian compile` writes this `web.container`:

```ini
# Written by Meridian from the document "hello". The next apply overwrites any change made here.

[Container]
Image=docker.io/library/nginx:1.27
PublishPort=8080:80
Volume=site.volume:/usr/share/nginx/html:ro

[Install]
WantedBy=default.target
```

## Resource types

Each resource becomes one unit file named after its `id`.

| `type` | Unit file | systemd service | Name in Podman |
|---|---|---|---|
| `container` | `<id>.container` | `<id>.service` | `systemd-<id>`, or `ContainerName` |
| `pod` | `<id>.pod` | `<id>-pod.service` | `systemd-<id>`, or `PodName` |
| `kube` | `<id>.kube` | `<id>.service` | set by the YAML it plays |
| `volume` | `<id>.volume` | `<id>-volume.service` | `systemd-<id>`, or `VolumeName` |
| `network` | `<id>.network` | `<id>-network.service` | `systemd-<id>`, or `NetworkName` |
| `image` | `<id>.image` | `<id>-image.service` | the `Image` it pulls |
| `build` | `<id>.build` | `<id>-build.service` | its `ImageTag` |

An `id` starts with a letter or digit, holds only letters, digits, `_` and `-`, and is at
most 200 characters.

| `type` | What it is | Required params |
|---|---|---|
| `container` | one container, run with `podman run` | `Image` or `Rootfs` |
| `pod` | a pod that containers join with `Pod: ${id}` | none |
| `kube` | a Kubernetes YAML file played with `podman kube play` | `Yaml` |
| `volume` | a named volume, created if it is missing | none |
| `network` | a network, created if it is missing | none |
| `image` | an image pulled ahead of the containers that use it | `Image` |
| `build` | an image built from a Containerfile | `ImageTag`, and `File` or `SetWorkingDirectory` |

A `pod`, a `volume` and a `network` may have no `params` at all.

### Names

- The **service** is the one in the table above unless the resource sets `ServiceName`, in
  which case it is `<ServiceName>.service`. `dependsOn`, `notifies` and `${id.service}`
  all use the name that is in force.
- The **name in Podman** is what `${id.name}` gives. When it is built from a systemd
  specifier (`ContainerName: web-%H`), only systemd knows it, and `${id.name}` is refused.
  A `kube` unit has no single name.
- Two resources may not map to one service (a container `db-volume` and a volume `db`).

### State

| `state` | Meaning |
|---|---|
| left out, `present` or `running` | the unit is installed and its service started |
| `absent` | refused: remove the resource from the document instead |
| anything else | refused |

## Params

`params` are Quadlet's keys, spelled as
[Podman documents them](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html):
`Image`, `PublishPort`, `Volume`, `Environment` and the rest.

| In the document | In the unit file |
|---|---|
| `PublishPort: "8080:80"` | `PublishPort=8080:80` |
| `ReadOnly: true` | `ReadOnly=true` |
| `PublishPort: ["8080:80", "8443:443"]` | one `PublishPort=` line per item |
| `Environment: {TZ: UTC, NOTE: two words}` | `Environment=TZ=UTC` and `Environment="NOTE=two words"` |

- A **string, number or boolean** is one line, `Key=value`. Booleans are written `true`
  and `false`.
- A **list** is one line per item. An empty list leaves the key out. A list inside a list
  is refused.
- A **map** is one `Key=name=value` line per entry, sorted by name, quoted as systemd
  expects. Its values must be single values.
- `~` and values with a line break are refused.
- Keys are sorted within their section.

The other sections of a unit file are four lower-case params, each a map:

```yaml
    - id: web
      type: container
      params:
        Image: docker.io/library/nginx:1.27
        unit:
          Description: The web front end
          After: network-online.target
        service:
          Restart: always
          TimeoutStartSec: 300
        install:
          WantedBy: multi-user.target
        quadlet:
          DefaultDependencies: false
```

| Param | Section | Typical keys |
|---|---|---|
| `unit` | `[Unit]` | `Description`, `After`, `Wants`, `ConditionPathExists` |
| `service` | `[Service]` | `Restart`, `TimeoutStartSec`, `Type`, `RemainAfterExit` |
| `install` | `[Install]` | `WantedBy` |
| `quadlet` | `[Quadlet]` | `DefaultDependencies` (Podman 5.3 and later) |

Their keys are systemd's, unchecked, with the same value rules. Section order: `[Unit]`, the
unit's own section, `[Service]`, `[Quadlet]`, `[Install]`.

Containers, pods and kube units get `[Install] WantedBy=default.target` unless `install` is
set; `install: {}` leaves it out. Volumes, networks, images and builds get no `[Install]`.

### Secrets

A [declared secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) under
`Environment` becomes a Podman secret; the unit file only names it.

```yaml
spec:
  target: podman
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db
      type: container
      params:
        Image: docker.io/library/postgres:16
        Environment:
          POSTGRES_DB: app
          POSTGRES_PASSWORD: ${{ secret.db_password }}
```

The compile writes a `Secret` line:

```ini
[Container]
Image=docker.io/library/postgres:16
Environment=POSTGRES_DB=app
Secret=meridian_secret_db_password,type=env,target=POSTGRES_PASSWORD
```

Before writing any unit, an apply runs `podman secret create meridian_secret_<name> -` with
the value on stdin (over `ssh`, as the unit's user, or with `sudo` for system units). An
existing secret is replaced first. A plan creates nothing. Logs name secrets, not values.

Podman's `file` driver stores it base64-encoded, unencrypted, readable by that user only
(root for system units). A changed value alone does not restart the unit.

A secret anywhere else, or inside a longer value, is refused (`unsupported-secret-place`).
For other cases use Quadlet's `Secret` key and create the secret yourself:

```yaml
        Secret: db-password,type=env,target=POSTGRES_PASSWORD
```

Tested against a stand-in `podman` (`plugins/podman/tests/run.rs`) and real Podman in CI
(`plugins/podman/tests/real.rs`).

### Literal values and systemd's expansions

- **Strings and lists** are expanded by systemd (`HostName: web-%H`).
- **Map values** are literal: `Environment: {PASSWORD: "50%off$now"}` is escaped for systemd.

`${...}` in a document is a Meridian reference (below). To hand `${HOME}` to systemd as
written, escape it as `$${HOME}`.

### Mistakes caught at compile time

- A wrongly capitalised key, such as `image`, is refused with the right spelling.
- An unknown key is kept with a warning; the host's Quadlet refuses it at plan if it is
  unknown there too.
- A container without `Image` or `Rootfs`, a `kube` without `Yaml`, an `image` without
  `Image`, and a `build` without `ImageTag` and a `File` are refused.
- Line breaks, empty values and unknown types are refused.

### Known keys

Keys known per type, as of Podman 5, used to catch typos. See
[Podman's reference](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html).

| `type` | Keys |
|---|---|
| `container` | `AddCapability`, `AddDevice`, `AddHost`, `Annotation`, `AutoUpdate`, `CgroupsMode`, `ContainerName`, `ContainersConfModule`, `DNS`, `DNSOption`, `DNSSearch`, `DropCapability`, `Entrypoint`, `Environment`, `EnvironmentFile`, `EnvironmentHost`, `Exec`, `ExposeHostPort`, `GIDMap`, `GlobalArgs`, `Group`, `GroupAdd`, `HealthCmd`, `HealthInterval`, `HealthLogDestination`, `HealthMaxLogCount`, `HealthMaxLogSize`, `HealthOnFailure`, `HealthRetries`, `HealthStartPeriod`, `HealthStartupCmd`, `HealthStartupInterval`, `HealthStartupRetries`, `HealthStartupSuccess`, `HealthStartupTimeout`, `HealthTimeout`, `HostName`, `IP`, `IP6`, `Image`, `Label`, `LogDriver`, `LogOpt`, `Mask`, `Memory`, `Mount`, `Network`, `NetworkAlias`, `NoNewPrivileges`, `Notify`, `PidsLimit`, `Pod`, `PodmanArgs`, `PublishPort`, `Pull`, `ReadOnly`, `ReadOnlyTmpfs`, `ReloadCmd`, `ReloadSignal`, `RemapGid`, `RemapUid`, `RemapUidSize`, `RemapUsers`, `Retry`, `RetryDelay`, `Rootfs`, `RunInit`, `SeccompProfile`, `Secret`, `SecurityLabelDisable`, `SecurityLabelFileType`, `SecurityLabelLevel`, `SecurityLabelNested`, `SecurityLabelType`, `ServiceName`, `ShmSize`, `StartWithPod`, `StopSignal`, `StopTimeout`, `SubGIDMap`, `SubUIDMap`, `Sysctl`, `Timezone`, `Tmpfs`, `UIDMap`, `Ulimit`, `Unmask`, `User`, `UserNS`, `VolatileTmp`, `Volume`, `WorkingDir` |
| `pod` | `AddHost`, `ContainersConfModule`, `DNS`, `DNSOption`, `DNSSearch`, `GIDMap`, `GlobalArgs`, `HostName`, `IP`, `IP6`, `Network`, `NetworkAlias`, `PodName`, `PodmanArgs`, `PublishPort`, `ServiceName`, `ShmSize`, `SubGIDMap`, `SubUIDMap`, `UIDMap`, `UserNS`, `Volume` |
| `kube` | `AutoUpdate`, `ConfigMap`, `ContainersConfModule`, `ExitCodePropagation`, `GlobalArgs`, `KubeDownForce`, `LogDriver`, `Network`, `PodmanArgs`, `PublishPort`, `RemapGid`, `RemapUid`, `RemapUidSize`, `RemapUsers`, `ServiceName`, `SetWorkingDirectory`, `UserNS`, `Yaml` |
| `volume` | `ContainersConfModule`, `Copy`, `Device`, `Driver`, `GlobalArgs`, `Group`, `Image`, `Label`, `Options`, `PodmanArgs`, `ServiceName`, `Type`, `User`, `VolumeName` |
| `network` | `ContainersConfModule`, `DNS`, `DisableDNS`, `Driver`, `Gateway`, `GlobalArgs`, `IPAMDriver`, `IPRange`, `IPv6`, `Internal`, `Label`, `NetworkDeleteOnStop`, `NetworkName`, `Options`, `PodmanArgs`, `ServiceName`, `Subnet` |
| `image` | `AllTags`, `Arch`, `AuthFile`, `CertDir`, `ContainersConfModule`, `Creds`, `DecryptionKey`, `GlobalArgs`, `Image`, `ImageTag`, `OS`, `PodmanArgs`, `Retry`, `RetryDelay`, `ServiceName`, `TLSVerify`, `Variant` |
| `build` | `Annotation`, `Arch`, `AuthFile`, `ContainersConfModule`, `DNS`, `DNSOption`, `DNSSearch`, `Environment`, `File`, `ForceRM`, `GlobalArgs`, `GroupAdd`, `ImageTag`, `Label`, `Network`, `PodmanArgs`, `Pull`, `Retry`, `RetryDelay`, `Secret`, `ServiceName`, `SetWorkingDirectory`, `TLSVerify`, `Target`, `Variant`, `Volume` |

`PodmanArgs` and `GlobalArgs` pass arguments straight to `podman`.

## Wiring resources together

| Reference | Becomes | Use it for |
|---|---|---|
| `${data}` | the unit file, `data.volume` | `Volume`, `Network`, `Pod`, `Image`: how Quadlet links units |
| `${data.name}` | the name Podman knows it by, `systemd-data` | environment variables, anything outside Quadlet |
| `${data.service}` | the systemd service, `data-volume.service` | the `unit` section |
| `${data.inspect.Mountpoint}` | a field of `podman volume inspect`, read after an apply | outputs only |

Quadlet orders a unit after every unit file it names, so `Volume: ${data}:/srv` needs no
`dependsOn`. An order the params do not show is written out:

```yaml
    - id: web
      type: container
      params:
        Image: docker.io/library/nginx:1.27
      dependsOn:
        - resource: db        # becomes Requires=db.service and After=db.service
```

Lines in the document's `unit` section are kept; `dependsOn` lines follow them.

### Notifications

`notifies` with `restart` becomes `PartOf=`. Other actions are refused.

```yaml
    - id: proxy-config
      type: volume
      notifies:
        - resource: proxy
          action: restart     # proxy.container gets PartOf=proxy-config-volume.service
```

- systemd restarts the notified unit whenever the notifier restarts or stops, by apply
  or by hand.
- It sets no order; use `dependsOn` or a reference.

### What a reference can read

| Written | On a `container`, `pod`, `volume`, `network` | On an `image` or `build` | On a `kube` |
|---|---|---|---|
| `${id}` | the unit file | the unit file | the unit file |
| `${id.service}` | the service | the service | the service |
| `${id.name}` | the name in Podman | the image name or tag | refused |
| `${id.inspect.<Field>}` (outputs only) | a field of `podman <type> inspect` | a field of `podman image inspect` | refused |

Any other reference, such as `${web.address}`, is refused at compile.

## Outputs

Outputs can be text, the names above, or values from Podman:

```yaml
  outputs:
    - name: web_service
      value: ${web.service}
    - name: data_directory
      value: ${data.inspect.Mountpoint}
    - name: started
      value: ${web.inspect.State.StartedAt}
```

After `.inspect.` comes a dotted path of fields from `podman <type> inspect`. A `kube` unit
cannot be inspected.

- Outputs are text; multi-part outputs are joined.
- `.inspect.` parts are read once, after units start. A missing field fails the apply.
- No output is sensitive. Do not put a secret in one.
- Only Infrastructure documents have outputs.

## What plan and apply do

`meridian plan`:

1. Compares each compiled unit file with the host's.
2. Asks systemd whether each service is running.
3. Has the host's Quadlet check the units in a temporary directory, failing on any it refuses.
4. Lists previously installed units the document no longer has.

It changes nothing and needs no extra rights.

| Plan line | Meaning |
|---|---|
| `create web: install web.container and start web.service` | the unit is not on the host |
| `update web: web.container changed; restart web.service` | the file differs |
| `update web: web.service is inactive; start it` | the file matches but the service is not running |
| `delete web: stop web.service and remove web.container` | the document no longer has the resource |

`meridian apply`, in order:

1. Makes the plan's checks. If they fail, it stops here and nothing has changed.
2. Creates the unit directory if it is missing.
3. Stops the service of each deleted unit and removes its file.
4. Writes each new or changed unit file.
5. Runs `systemctl daemon-reload`, which is when Quadlet generates the services.
6. Restarts the services whose files changed.
7. Starts every service of the document. systemd orders them by their dependencies and
   leaves alone what is already running.
8. Reads the outputs.

A unit that does not start fails the apply with its journal tail. Only units Meridian
installed are removed. When nothing differs, steps 2 to 7 are skipped.

### What Meridian keeps

Under `--state-dir` (`.meridian` by default), in the document's work directory:

| File | What it is |
|---|---|
| the unit files | a copy of what was compiled, for you to read |
| `meridian-podman.json` | which service each unit becomes and what the outputs are made of; written by the compile, never installed on the host |
| `installed.json` | the unit files the last apply left on the host, which is how a later apply knows what to remove |

If `installed.json` is lost, units of deleted resources stay on the host.

### When it fails

| Message | What happened | What to do |
|---|---|---|
| `Podman's Quadlet was not found on <host>` | Podman is missing, or older than 4.4 | Install Podman, or set `MERIDIAN_PODMAN_QUADLET` |
| `the Quadlet on <host> refused the units` | Quadlet rejected a key or a value; its own words follow | Fix the param it names |
| `<file> would not become <service> on <host>` | the host's Podman is too old for that type of unit | Upgrade Podman, or do without pods, builds or image units |
| `could not reach <host> over ssh` | `ssh` failed; its own words follow | Check the host, the user and the key; see `MERIDIAN_PODMAN_SSH_ARGS` |
| `could not look at the units on <host>` | the host has no usable shell tools, or no home directory for a user's units | Read the output that follows |
| `could not create the directory the units are installed in` | no permission | Use the `system` scope with `sudo`, or fix the directory |
| `could not write or remove a unit file` | no permission, or the disk is full | As above |
| `systemctl daemon-reload failed` | systemd is not running for this user | For rootless units, log in properly or enable lingering |
| `a changed unit did not restart`, `a unit did not start` | the container failed; the end of its journal follows | Fix what the journal says, such as a missing image or a port in use |
| `podman could not report <Field> of the <type> <name>` | an output reads a field that does not exist | Fix the output's path |
| `nothing has been applied for this document yet` | outputs were asked for before an apply | Apply first |

A failed apply reports what it changed. Running it again is safe.

## Where it runs

- An **Infrastructure** document runs on the machine running Meridian.
- A **ResourceSet** runs on its `targetHost` over `ssh`. The host needs only Podman, and can
  come from an earlier stage:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: edge-proxy
spec:
  target: podman
  targetHost:
    from: infrastructure
    ref: edge-vm
    output: public_ip
  resources:
    - id: proxy
      type: container
      params:
        Image: docker.io/library/caddy:2.9
        PublishPort: ["80:80", "443:443"]
```

Until `edge-vm` exists, `meridian plan` reports this document as deferred.

- Hosts are reached with `ssh -o BatchMode=yes <host>`; use a key or an agent.
- User, key and port come from your `ssh` config or `MERIDIAN_PODMAN_SSH_ARGS`.
- Each step is one shell script over that connection. Only unit files are copied.
- `localhost`, `127.0.0.1` and `::1` do not use `ssh`.

### User units and system units

| | `user` (default) | `system` |
|---|---|---|
| Runs as | the connecting user, rootless | root |
| Unit files | `~/.config/containers/systemd`, or under `$XDG_CONFIG_HOME` | `/etc/containers/systemd` |
| systemd | `systemctl --user` | `systemctl` |
| Rights needed to plan | none | none |
| Rights needed to apply | none | root, or `sudo` without a password |
| Ports below 1024 | not by default | yes |
| Survives logout | only with lingering enabled | yes |

## Settings

The runner is configured through the environment of the `meridian` command.

| Variable | Default | Meaning |
|---|---|---|
| `MERIDIAN_PODMAN_SCOPE` | `user` | `user` installs rootless units in `~/.config/containers/systemd` and uses `systemctl --user`. `system` installs them in `/etc/containers/systemd`, using `sudo -n` for the changes when you are not root. |
| `MERIDIAN_PODMAN_SSH_ARGS` | none | Extra arguments for every `ssh`, such as `-l deploy -i ~/.ssh/deploy`. |
| `MERIDIAN_PODMAN_SSH` | `ssh` | The `ssh` program. |
| `MERIDIAN_SSH_AGENT` | `ssh-agent` | The `ssh-agent` program, started for a credential's key; see [Credentials](#credentials). |
| `MERIDIAN_SSH_ADD` | `ssh-add` | The `ssh-add` program. |
| `MERIDIAN_PODMAN` | `podman` | The `podman` program on the host. |
| `MERIDIAN_PODMAN_SYSTEMCTL` | `systemctl` | The `systemctl` program on the host. |
| `MERIDIAN_PODMAN_QUADLET` | found automatically | Quadlet's generator on the host, when it is not where distributions install it. |
| `MERIDIAN_PODMAN_UNIT_DIR` | by scope | The directory unit files are installed in. |

Settings can also go in `meridian.yaml` under `spec.settings.podman`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#podman).

- Any other `MERIDIAN_PODMAN_SCOPE` value fails the run.
- For rootless units to outlive your login: `loginctl enable-linger <user>`.
- Registry credentials are Podman's own; if `podman pull` works on the host, so does apply.

## Credentials

`targetHost.credential` names an `ssh` [credential](../reference/dsl/credentials.md) with a
user and a private key:

```yaml
spec:
  target: podman
  targetHost:
    from: inline
    host: edge01.example.com
    credential: deploy
  secrets:
    - name: deploy_key
      from: env
      key: DEPLOY_KEY
  credentials:
    - name: deploy
      type: ssh
      user: deploy
      privateKey: ${{ secret.deploy_key }}
```

- **The user** is `ssh`'s `-l`, before the arguments of `MERIDIAN_PODMAN_SSH_ARGS`.
- **A key** goes into an SSH agent Meridian starts for the stage, loaded through `ssh-add`'s
  stdin; `ssh` gets only `SSH_AUTH_SOCK`. A `passphrase` is answered from a private file.
  See [0028](../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md).
- **`agent: true`** uses your own agent. **`from: ambient`** leaves signing in to the tool.
- **Refused at compile** (`unsupported-credential`): a `password` (use a `privateKey`), a
  `certificate` (load it in your own agent, `agent: true`), a `domain`, a user name with a
  space or line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). Leave the credential out, or name another host.

The plan names the identity (`web (podman) on edge01.example.com as deploy (ssh, user
deploy)`), never a value. `ssh-agent` and `ssh-add` come from `PATH` or `MERIDIAN_SSH_AGENT`
and `MERIDIAN_SSH_ADD`. Tested against stand-ins only; CI has no SSH server for Podman.

## What this target cannot express

| In the document | Why it is refused | What to do |
|---|---|---|
| `state: absent` | A unit that is not in the document is removed by the next apply. | Remove the resource. |
| `runtimeWhen` | A unit's start conditions are systemd's fixed `Condition*=` tests, not an expression. | Use `unit: {ConditionPathExists: ...}` or another `Condition*=`. |
| `notifies` with an action other than `restart` | systemd can only propagate a restart. | Use `restart`. |
| `${x.inspect...}` in a param | It is only known after the thing exists; a unit file is written first. | Use it in an output. |

## Every compile-time refusal

Each refusal names the resource, line and rule.

| Rule | When |
|---|---|
| `unsupported-resource-type` | `type` is not one of the seven above |
| `unsupported-state` | `state` is `absent` or anything but `present` and `running` |
| `unsupported-runtime-condition` | the resource has a `runtimeWhen` |
| `unsupported-param` | a key is a known Quadlet key in the wrong capitalisation (`image`), or starts with a lower-case letter and is not `unit`, `service`, `install` or `quadlet` |
| `invalid-param` | a value has a line break or no value; a list holds a list; a map holds a list or a map; `unit`, `service`, `install` or `quadlet` is not a map; `ServiceName`, `ContainerName`, `PodName`, `VolumeName`, `NetworkName` or an image's `Image` is not a non-empty string |
| `invalid-reference` | `${...}` names something that is not a resource of the document, reads something other than `.name`, `.service` or `.inspect.<Field>`, reads `.name` where it cannot be known, uses `.inspect.` in a param, or is never closed |
| `missing-param` | a required param from the [resource types](#resource-types) table is missing |
| `invalid-name` | the `id` cannot be a unit file name |
| `duplicate-resource` | two resources would run as the same service |
| `unsupported-action` | a `notifies` action is not `restart` |
| `unknown-dependency` | an edge names a resource the document does not declare |
| `unsupported-secret-place` | a [secret](#secrets) is anywhere but the whole value of a container's `Environment` variable |

A capitalised key not in [Known keys](#known-keys) is a warning.

## Things to know

- **Volumes and networks are created once.** To apply changed params, remove the existing
  one with `podman volume rm` or `podman network rm`.
- **Removing a volume removes its unit, not its data.**
- **An exited container is restarted by the next apply.** For a job, set
  `service: {Type: oneshot, RemainAfterExit: yes}`.
- **Pods need Podman 5.0, builds 5.2, image units 4.8.**
- **Linux hosts only.** On macOS and Windows, target the Podman VM over `ssh`.
- **Hand edits on the host are reverted** by the next apply.
- **Image updates are Podman's:** `AutoUpdate: registry` and `podman-auto-update.timer`.
- **Relative paths** in `Yaml`, `File` and similar resolve from the host's unit directory.
  Use absolute paths.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| The plan always shows `web.service is inactive; start it` | The container exits. See the note on jobs above, or check `journalctl --user -u web.service` |
| `Failed to connect to bus` during an apply | No systemd session for the user. Enable lingering, or use the `system` scope |
| A warning that an image is not fully qualified | Write the registry in `Image`, for example `docker.io/library/nginx:1.27` |
| A port below 1024 will not publish | Rootless Podman cannot bind it. Use the `system` scope or a higher port |
| A changed volume or network has no effect | It already exists. See the first note above |

`meridian compile <file> --out build` shows the unit files without touching the host.

## Examples

- [`examples/podman/web-app.yaml`](../../examples/podman/web-app.yaml): a web server and a
  database on a private network with a volume, and three outputs.
- [`examples/podman/pod.yaml`](../../examples/podman/pod.yaml): two containers in one pod,
  with a pre-pulled image.
- [`examples/podman/edge-host.yaml`](../../examples/podman/edge-host.yaml): a ResourceSet
  for another machine, with a bind-mounted volume that restarts the container when it
  changes.

## Tests

```
cargo test -p meridian-target-podman
```

runs the compiler against golden files, and the runner against stand-ins for `podman`,
`systemctl`, Quadlet and `ssh`, so it needs nothing installed. Two further levels use the
real tools:

```
MERIDIAN_TEST_REAL_PODMAN=quadlet cargo test -p meridian-target-podman --test real
MERIDIAN_TEST_REAL_PODMAN=system  cargo test -p meridian-target-podman --test real
```

`quadlet` has the machine's own Quadlet read every example and installs nothing. `system`
also installs, starts, changes and removes real units in `/etc/containers/systemd`; it
needs systemd and root or passwordless `sudo`, and CI runs it on every pull request.
