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
| `state: absent` | No. Remove the resource from the document; the next apply stops and removes its unit |
| Plan | Yes: an exact comparison of unit files, plus a check by the host's own Quadlet |
| Outputs | Yes: names known at compile time, or a path into `podman inspect` |
| Documents | Infrastructure (this machine) and ResourceSet (its `targetHost`, over `ssh`) |
| Needs on the host | Linux with systemd, and Podman 4.4 or later (4.8 for image units, 5.0 for pods, 5.2 for builds) |

Podman has no daemon and no `apply` command. Its declarative form is Quadlet: you write a
small unit file for a container, and systemd turns it into a service that runs `podman`.
The containers then start at boot and restart on failure with nothing else running.

This plugin compiles a Meridian document into those unit files, installs them, and has
systemd start them. A plan is an exact comparison of the unit files on the host with the
ones your document compiles to.

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

An `id` starts with a letter or a digit, holds only letters, digits, `_` and `-`, and is
at most 200 characters long, because it is a file name and a systemd unit name.

What each type is for, and what it cannot do without:

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
- Two resources may not end up as the same service. A container `db-volume` and a volume
  `db` would both be `db-volume.service`, and are refused.

### State

| `state` | Meaning |
|---|---|
| left out, `present` or `running` | the unit is installed and its service started |
| `absent` | refused: remove the resource from the document instead |
| anything else | refused |

## Params

`params` are Quadlet's own keys, spelled exactly as
[Podman documents them](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html):
`Image`, `PublishPort`, `Volume`, `Environment`, `Network`, `Pod`, `Exec`, `AutoUpdate`
and the rest. Meridian does not put a smaller vocabulary in front of them, so anything
Quadlet can do, a document can say.

How a value is written:

| In the document | In the unit file |
|---|---|
| `PublishPort: "8080:80"` | `PublishPort=8080:80` |
| `ReadOnly: true` | `ReadOnly=true` |
| `PublishPort: ["8080:80", "8443:443"]` | one `PublishPort=` line per item |
| `Environment: {TZ: UTC, NOTE: two words}` | `Environment=TZ=UTC` and `Environment="NOTE=two words"` |

The full rules:

- A **string, number or boolean** is one line, `Key=value`. Booleans are written `true`
  and `false`.
- A **list** is one line per item. An empty list leaves the key out. A list inside a list
  is refused.
- A **map** is one `Key=name=value` line per entry, in the order of the names. A value
  with a space, a quote or a backslash is quoted the way systemd expects. A map's values
  must be single values.
- A value with **no value** (`~`) is refused; leave the key out. A value with a **line
  break** is refused, because a unit file value is one line.
- Keys are written in alphabetical order within their section, so the same document always
  compiles to the same bytes.

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

Their keys are systemd's and are written as given, with the same value rules as above.
Meridian does not check them against a list. The file is written in the order `[Unit]`,
the unit's own section, `[Service]`, `[Quadlet]`, `[Install]`, under a comment that names
the document it came from.

Containers, pods and kube units get `[Install] WantedBy=default.target` unless the
document has its own `install`, so what an apply starts is started again after a reboot.
Write `install: {}` for a unit that should only run when something starts it. Volumes,
networks, images and builds get no `[Install]`: they start when a unit that uses them does.

### Secrets

A value under `Environment` is written into the unit file, which other users of the host
may be able to read. A [declared secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool)
there is not: it becomes a Podman secret the container reads into the variable, and the
unit file names it and nothing more.

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

**What the compile writes.** The variable leaves `Environment` and becomes a `Secret`
line that names the Podman secret and the variable to read it into:

```ini
[Container]
Image=docker.io/library/postgres:16
Environment=POSTGRES_DB=app
Secret=meridian_secret_db_password,type=env,target=POSTGRES_PASSWORD
```

**How the value reaches Podman.** An apply, after it has surveyed the host and before it
writes or starts any unit, creates each secret in Podman on the host with
`podman secret create meridian_secret_<name> -`, reading the value from the script's own
standard input, the same channel every other step of an apply uses: over `ssh` for a
remote host, as the unit's user for user units and with `sudo` for system units. A secret
that already exists is replaced first (`podman secret rm`), so the value is current every
apply. The value is on no command line and in no environment, and in no file Meridian
writes; a plan creates nothing and reads nothing. The log says which secrets were created
and never what they hold.

**What Podman keeps.** Podman stores the secret with its default driver, `file`, which
writes it under the user's Podman directory base64-encoded and unencrypted, readable by
that user alone. Secrets created for a system unit belong to root. A changed value takes
effect when the container next starts: a changed `Secret` line restarts the unit, a changed
value alone does not, so restart the service or notify it from a resource that changes.

**What is refused.** A secret anywhere but a container's `Environment` is refused at
compile, rule `unsupported-secret-place`: a unit file is written onto the host as it
stands. A secret inside a value, such as `postgres://app:${{ secret.db_password }}@db/app`,
is refused too: a variable's value is one string, so the secret has to be the whole value.
Write a secret for another kind of unit, or a secret Podman already holds, with Quadlet's
own `Secret` key and create it yourself:

```yaml
        Secret: db-password,type=env,target=POSTGRES_PASSWORD
```

The handover is tested against a stand-in `podman` that keeps what it was given on
standard input (`plugins/podman/tests/run.rs`) and against the real Podman in CI
(`plugins/podman/tests/real.rs`): the value reaches the container's environment, and is in
no unit file, no argument, and nothing the plan or apply prints.

### Literal values and systemd's expansions

systemd expands `%` specifiers (`%H` is the host name) and `${VAR}` in what Quadlet hands
it. Meridian keeps the two cases apart:

- A value written as a **string or a list** is systemd's to expand. `HostName: web-%H`
  works as systemd documents it.
- The values of a **map** mean exactly what they say. `Environment: {PASSWORD: "50%off$now"}`
  reaches the container as `50%off$now`; Meridian writes the escapes systemd needs.

`${...}` in a document is a Meridian reference (below). To hand `${HOME}` to systemd as
written, escape it as `$${HOME}`.

### Mistakes caught at compile time

- A key in the wrong capitalisation, such as `image`, is refused with the right spelling.
  Quadlet reads keys exactly, and would otherwise reject the unit where only the journal
  sees it.
- A key this plugin does not know is kept and warned about, since a newer Podman may have
  added it. If the host's Podman does not know it either, the plan refuses the unit.
- A container without `Image` or `Rootfs`, a `kube` without `Yaml`, an `image` without
  `Image`, and a `build` without `ImageTag` and a `File` are refused.
- A value with a line break, an empty value, and a resource type that is not in the table
  above are refused.

### Known keys

These are the keys this plugin knows for each type, as of Podman 5. What each one means is
in [Podman's reference](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html).
The table only exists to catch typing mistakes: a key that is not here is still written,
with a warning, and the host's own Quadlet has the last word when the document is planned.

| `type` | Keys |
|---|---|
| `container` | `AddCapability`, `AddDevice`, `AddHost`, `Annotation`, `AutoUpdate`, `CgroupsMode`, `ContainerName`, `ContainersConfModule`, `DNS`, `DNSOption`, `DNSSearch`, `DropCapability`, `Entrypoint`, `Environment`, `EnvironmentFile`, `EnvironmentHost`, `Exec`, `ExposeHostPort`, `GIDMap`, `GlobalArgs`, `Group`, `GroupAdd`, `HealthCmd`, `HealthInterval`, `HealthLogDestination`, `HealthMaxLogCount`, `HealthMaxLogSize`, `HealthOnFailure`, `HealthRetries`, `HealthStartPeriod`, `HealthStartupCmd`, `HealthStartupInterval`, `HealthStartupRetries`, `HealthStartupSuccess`, `HealthStartupTimeout`, `HealthTimeout`, `HostName`, `IP`, `IP6`, `Image`, `Label`, `LogDriver`, `LogOpt`, `Mask`, `Memory`, `Mount`, `Network`, `NetworkAlias`, `NoNewPrivileges`, `Notify`, `PidsLimit`, `Pod`, `PodmanArgs`, `PublishPort`, `Pull`, `ReadOnly`, `ReadOnlyTmpfs`, `ReloadCmd`, `ReloadSignal`, `RemapGid`, `RemapUid`, `RemapUidSize`, `RemapUsers`, `Retry`, `RetryDelay`, `Rootfs`, `RunInit`, `SeccompProfile`, `Secret`, `SecurityLabelDisable`, `SecurityLabelFileType`, `SecurityLabelLevel`, `SecurityLabelNested`, `SecurityLabelType`, `ServiceName`, `ShmSize`, `StartWithPod`, `StopSignal`, `StopTimeout`, `SubGIDMap`, `SubUIDMap`, `Sysctl`, `Timezone`, `Tmpfs`, `UIDMap`, `Ulimit`, `Unmask`, `User`, `UserNS`, `VolatileTmp`, `Volume`, `WorkingDir` |
| `pod` | `AddHost`, `ContainersConfModule`, `DNS`, `DNSOption`, `DNSSearch`, `GIDMap`, `GlobalArgs`, `HostName`, `IP`, `IP6`, `Network`, `NetworkAlias`, `PodName`, `PodmanArgs`, `PublishPort`, `ServiceName`, `ShmSize`, `SubGIDMap`, `SubUIDMap`, `UIDMap`, `UserNS`, `Volume` |
| `kube` | `AutoUpdate`, `ConfigMap`, `ContainersConfModule`, `ExitCodePropagation`, `GlobalArgs`, `KubeDownForce`, `LogDriver`, `Network`, `PodmanArgs`, `PublishPort`, `RemapGid`, `RemapUid`, `RemapUidSize`, `RemapUsers`, `ServiceName`, `SetWorkingDirectory`, `UserNS`, `Yaml` |
| `volume` | `ContainersConfModule`, `Copy`, `Device`, `Driver`, `GlobalArgs`, `Group`, `Image`, `Label`, `Options`, `PodmanArgs`, `ServiceName`, `Type`, `User`, `VolumeName` |
| `network` | `ContainersConfModule`, `DNS`, `DisableDNS`, `Driver`, `Gateway`, `GlobalArgs`, `IPAMDriver`, `IPRange`, `IPv6`, `Internal`, `Label`, `NetworkDeleteOnStop`, `NetworkName`, `Options`, `PodmanArgs`, `ServiceName`, `Subnet` |
| `image` | `AllTags`, `Arch`, `AuthFile`, `CertDir`, `ContainersConfModule`, `Creds`, `DecryptionKey`, `GlobalArgs`, `Image`, `ImageTag`, `OS`, `PodmanArgs`, `Retry`, `RetryDelay`, `ServiceName`, `TLSVerify`, `Variant` |
| `build` | `Annotation`, `Arch`, `AuthFile`, `ContainersConfModule`, `DNS`, `DNSOption`, `DNSSearch`, `Environment`, `File`, `ForceRM`, `GlobalArgs`, `GroupAdd`, `ImageTag`, `Label`, `Network`, `PodmanArgs`, `Pull`, `Retry`, `RetryDelay`, `Secret`, `ServiceName`, `SetWorkingDirectory`, `TLSVerify`, `Target`, `Variant`, `Volume` |

`PodmanArgs` and `GlobalArgs` pass arguments straight to `podman` for anything Quadlet has
no key for.

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

An order the document's own `unit` section already states is kept, and the `dependsOn`
lines are added after it.

### Notifications

`notifies` with the action `restart` becomes `PartOf=`: when the notifying unit changes
and is restarted, systemd restarts the notified one with it. `restart` is the only action
systemd can carry, so any other is refused.

```yaml
    - id: proxy-config
      type: volume
      notifies:
        - resource: proxy
          action: restart     # proxy.container gets PartOf=proxy-config-volume.service
```

This is a native notify: `PartOf=` is systemd's own mechanism, and nothing is emulated.
What it means in practice:

- When an apply finds that the notifier's unit file changed, it restarts the notifier, and
  systemd restarts every unit that is `PartOf=` it.
- The same happens when someone restarts or stops the notifier by hand with `systemctl`.
- A notified unit that is not running is not started by this; the apply starts it anyway.
- A notify does not order the two units. Add `dependsOn`, or a reference, when the order
  matters.

### What a reference can read

| Written | On a `container`, `pod`, `volume`, `network` | On an `image` or `build` | On a `kube` |
|---|---|---|---|
| `${id}` | the unit file | the unit file | the unit file |
| `${id.service}` | the service | the service | the service |
| `${id.name}` | the name in Podman | the image name or tag | refused |
| `${id.inspect.<Field>}` (outputs only) | a field of `podman <type> inspect` | a field of `podman image inspect` | refused |

A reference to a resource that is not in the document is refused at compile time, with
the line it is on. So is anything else after the id, such as `${web.address}`.

## Outputs

An Infrastructure document's outputs can be plain text, the names above, or values read
from Podman once the thing exists:

```yaml
  outputs:
    - name: web_service
      value: ${web.service}
    - name: data_directory
      value: ${data.inspect.Mountpoint}
    - name: started
      value: ${web.inspect.State.StartedAt}
```

What follows `.inspect.` is a path of field names from `podman container inspect`,
`podman volume inspect` and so on, joined by dots. Each name is letters, digits and
underscores. A `kube` unit cannot be inspected.

- Outputs are text. An output made of several parts, such as
  `http://${web.inspect.NetworkSettings.IPAddress}:8080/`, is joined into one string.
- Parts known at compile time cost nothing at apply time. Podman is only asked for the
  `.inspect.` parts, once, after the units have started.
- No output of this target is marked sensitive. Do not put a secret in one.
- If Podman cannot report a field, the apply fails and names the field, the type and the
  thing it asked about.
- Only an Infrastructure document has outputs. A ResourceSet uses them, through
  `targetHost.from: infrastructure`.

## What plan and apply do

`meridian plan`:

1. Compares each compiled unit file with the one on the host: new, changed or the same.
2. Asks systemd whether each service is running.
3. Has the host's own Quadlet read the compiled units, in a temporary directory. A unit it
   refuses, or a kind of unit the host's Podman is too old for, fails the plan with what
   Quadlet said. Nothing has been touched at that point.
4. Lists unit files a previous apply installed that the document no longer has.

It changes nothing and needs no more rights than you have, even for system units.

| Plan line | Meaning |
|---|---|
| `create web: install web.container and start web.service` | the unit is not on the host |
| `update web: web.container changed; restart web.service` | the file differs |
| `update web: web.service is inactive; start it` | the file matches but the service is not running |
| `delete web: stop web.service and remove web.container` | the document no longer has the resource |

`meridian apply` makes the same checks, then stops and removes what was deleted, writes
what is new or changed, runs `systemctl daemon-reload`, restarts the changed units and
starts all of them. If a unit does not start, the apply fails and shows the end of that
unit's journal.

Only units Meridian installed are ever removed. Other unit files in the same directory are
left alone.

In order, an apply:

1. Makes the plan's checks. If they fail, it stops here and nothing has changed.
2. Creates the unit directory if it is missing.
3. Stops the service of each deleted unit and removes its file.
4. Writes each new or changed unit file.
5. Runs `systemctl daemon-reload`, which is when Quadlet generates the services.
6. Restarts the services whose files changed.
7. Starts every service of the document. systemd orders them by their dependencies and
   leaves alone what is already running.
8. Reads the outputs.

When nothing differs, an apply does none of steps 2 to 7: no reload, no restart.

### What Meridian keeps

Under `--state-dir` (`.meridian` by default), in the document's work directory:

| File | What it is |
|---|---|
| the unit files | a copy of what was compiled, for you to read |
| `meridian-podman.json` | which service each unit becomes and what the outputs are made of; written by the compile, never installed on the host |
| `installed.json` | the unit files the last apply left on the host, which is how a later apply knows what to remove |

If `installed.json` is lost, Meridian no longer knows about units of resources you have
since deleted, and leaves them on the host. Remove those by hand.

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

A failed apply reports which changes it had already made. Running it again is safe: it
compares the host with the document again and does only what is still needed.

## Where it runs

- An **Infrastructure** document runs on the machine running Meridian.
- A **ResourceSet** runs on its `targetHost`, reached over `ssh`. The host needs Podman and
  nothing else. The host can come from an earlier stage, so one workflow can create a
  machine with Terraform and then run containers on it:

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

The first document, `edge-vm`, is an Infrastructure document for Terraform, Bicep or
CloudFormation with an output named `public_ip`. `meridian apply` runs it first and hands
the address to this one. Until the machine exists, `meridian plan` reports this document
as deferred.

How another host is reached:

- With `ssh -o BatchMode=yes <host>`, so a run never stops to ask for a password. Use a key
  or an agent.
- The user, key and port come from your `ssh` configuration or from
  `MERIDIAN_PODMAN_SSH_ARGS`.
- Each step is one shell script sent over that connection. Nothing is copied to the host
  except the unit files, and nothing of Meridian's is installed there.
- `localhost`, `127.0.0.1` and `::1` count as this machine and do not use `ssh`.

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

`MERIDIAN_PODMAN_SCOPE` set to anything but `user` or `system` fails every run with a
message saying so, instead of quietly using the default. `journalctl` and `sudo` are run by
those names from the host's `PATH`.

For rootless units to keep running after you log out, enable lingering for the user once:
`loginctl enable-linger <user>`.

Podman uses its own configuration for registries and credentials. Meridian does not store
or pass any: if `podman pull` works for the user on the host, so will an apply.

## Credentials

`targetHost.credential` names the [credential](../reference/dsl/credentials.md) the host is
reached as. The plugin reaches another machine with `ssh`, so it takes an `ssh` credential with a user and a private key, and gives `ssh` the two the way `ssh` takes them:

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
- **A key is never on disk.** It goes into an SSH agent Meridian starts for the stage:
  `ssh-agent -D -a <socket>`, its socket in a directory only you can enter, loaded with
  `ssh-add -` from standard input. `ssh` is told the socket in `SSH_AUTH_SOCK` and
  nothing else. A `passphrase` is answered to `ssh-add` from a file only you can read. The
  agent is stopped and its directory removed when the stage ends. Nothing of the key is on
  the command line, in the environment, in the compiled files or in a file Meridian keeps.
- **`agent: true`** names a key in your own agent: `ssh` reads `SSH_AUTH_SOCK` as it is,
  and Meridian starts nothing. **`from: ambient`** leaves signing in to the tool's own
  configuration, as with no credential at all.
- **Refused at compile**, rule `unsupported-credential`: a `password` (`ssh` asks for one on a terminal, which a run has not; reach the host as an `ssh` credential with a `privateKey`); a `certificate` (put it
  beside the key in an agent of your own and set `agent: true`); a `domain` (for WinRM
  hosts, which this plugin does not reach); a user name with a space or a line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). The scripts run here as the user running Meridian, whoever the document names. Leave the credential out, or name another host.

The plan line names the identity, `web (podman) on edge01.example.com as deploy (ssh,
user deploy)`, and never a value. `ssh-agent` and `ssh-add` are run from `PATH`, or from
`MERIDIAN_SSH_AGENT` and `MERIDIAN_SSH_ADD`.

The handover is tested against stand-ins for `ssh`, `ssh-agent` and `ssh-add` that keep what they are given: `ssh` sees `-l deploy` and the socket, `ssh-add` sees the key on its standard input and the passphrase from its askpass program, and the key is in nothing printed or left behind. The real `ssh` is not driven in CI; the Podman workflow has no SSH server.

## What this target cannot express

Meridian refuses these at compile time instead of emitting something that does less.

| In the document | Why it is refused | What to do |
|---|---|---|
| `state: absent` | A unit that is not in the document is removed by the next apply. | Remove the resource. |
| `runtimeWhen` | A unit's start conditions are systemd's fixed `Condition*=` tests, not an expression. | Use `unit: {ConditionPathExists: ...}` or another `Condition*=`. |
| `notifies` with an action other than `restart` | systemd can only propagate a restart. | Use `restart`. |
| `${x.inspect...}` in a param | It is only known after the thing exists; a unit file is written first. | Use it in an output. |

## Every compile-time refusal

Each refusal names the resource and the line. The rule is the short name `meridian`
prints with it.

| Rule | When |
|---|---|
| `unsupported-resource-type` | `type` is not one of the seven above |
| `unsupported-state` | `state` is `absent` or anything but `present` and `running` |
| `unsupported-runtime-condition` | the resource has a `runtimeWhen` |
| `unknown-param` | a key is a known Quadlet key in the wrong capitalisation (`image`), or starts with a lower-case letter and is not `unit`, `service`, `install` or `quadlet` |
| `invalid-param` | a value has a line break or no value; a list holds a list; a map holds a list or a map; `unit`, `service`, `install` or `quadlet` is not a map; `ServiceName`, `ContainerName`, `PodName`, `VolumeName`, `NetworkName` or an image's `Image` is not a non-empty string |
| `invalid-reference` | `${...}` names something that is not a resource of the document, reads something other than `.name`, `.service` or `.inspect.<Field>`, reads `.name` where it cannot be known, uses `.inspect.` in a param, or is never closed |
| `missing-param` | a required param from the [resource types](#resource-types) table is missing |
| `invalid-name` | the `id` cannot be a unit file name |
| `duplicate-service` | two resources would run as the same service |
| `unsupported-action` | a `notifies` action is not `restart` |
| `unknown-dependency` | an edge names a resource the document does not declare |
| `unsupported-secret-place` | a [secret](#secrets) is anywhere but the whole value of a container's `Environment` variable |

One thing is a warning and not a refusal: a capitalised key that is not in
[Known keys](#known-keys).

## Things to know

- **Volumes and networks are created once.** Quadlet creates them if they are missing and
  does not alter one that exists. Changing a volume's or a network's params rewrites the
  unit; to apply the change to the real thing, remove it with `podman volume rm` or
  `podman network rm` first.
- **Removing a volume from the document removes its unit, not its data.** Delete the data
  with `podman volume rm` when you mean to.
- **A container that exits is started again by the next apply**, because its service is
  no longer active. For a job that is meant to finish, set
  `service: {Type: oneshot, RemainAfterExit: yes}`.
- **Pods need Podman 5.0, builds 5.2, image units 4.8.** On an older host the plan says so
  by name.
- **Linux hosts only.** Quadlet is part of systemd. On macOS and Windows, Podman runs in a
  virtual machine; point a ResourceSet at that machine over `ssh` instead.
- **Edits on the host are overwritten.** A unit file changed by hand differs from the
  compiled one, so the next plan shows it as changed and the next apply puts it back.
- **Image updates are Podman's.** Set `AutoUpdate: registry` and enable
  `podman-auto-update.timer` on the host. Meridian restarts a container when its unit
  changes, not when a tag moves.
- **Relative paths** in `Yaml`, `File`, `EnvironmentFile` and the like are resolved by
  Quadlet from the unit directory on the host, not from the document. Use absolute paths.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| The plan always shows `web.service is inactive; start it` | The container exits. See the note on jobs above, or check `journalctl --user -u web.service` |
| `Failed to connect to bus` during an apply | No systemd session for the user. Enable lingering, or use the `system` scope |
| A warning that an image is not fully qualified | Write the registry in `Image`, for example `docker.io/library/nginx:1.27` |
| A port below 1024 will not publish | Rootless Podman cannot bind it. Use the `system` scope or a higher port |
| A changed volume or network has no effect | It already exists. See the first note above |

To see exactly what will be installed without touching anything, run
`meridian compile <file> --out build` and read the unit files.

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
