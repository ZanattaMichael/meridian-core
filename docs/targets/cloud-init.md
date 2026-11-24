# Target: `cloud-init`

<img src="../branding/logos/meridian-cloud-init.svg" width="72" alt="Meridian cloud-init plugin logo">

Compiles a ResourceSet into **user-data**, the `#cloud-config` a machine reads on first
boot: the login account, its SSH key, and packages a later stage needs.

## What it does and does not support

| | |
|---|---|
| Resource types | `package`, `packageRepository`, `file`, `service`, `user`, `group`, `cron`, `exec` ([what each takes here](#resource-reference)), and any cloud-config module beside them: `ntp`, `apt`, `timezone`, `bootcmd` ([modules beside the six](#cloud-config-modules-beside-the-six)) |
| `dependsOn` | Yes, within [the order of a first boot](#the-order-of-a-first-boot). cloud-init handles files, accounts, packages and commands at fixed steps, so a dependency that points backwards through those steps is refused. Nothing can depend on a module of cloud-init's own, or be depended on by one |
| `notifies` | Emulated: each notified action is a command that runs once, after every other command, with a warning. Actions `start`, `stop`, `restart`, `reload` on a service, `run` on an exec ([how](#notifications)) |
| `runtimeWhen` | No. It is refused: cloud-init evaluates nothing per resource |
| `adopt` | No. Refused: cloud-init runs at first boot and keeps nothing to adopt into |
| States | Only what a first boot can do. `absent` on a user, group or package and `latest` on a package are refused ([everything refused](#what-is-refused)) |
| References (`${...}`) | `${id.param}` is settled from the document when it is compiled; anything else is refused. `$${` is a literal `${` ([references](#references)) |
| Plan | Yes: `cloud-init schema` checks the user-data, and the plan says whether the seed would change. No machine is contacted |
| Apply | Writes the user-data and a NoCloud seed on the machine running Meridian. The new machine applies it by itself on its first boot. `apply` can wait for that boot over SSH ([plan and apply](#plan-and-apply)) |
| Outputs | Yes: `seed_dir` and `user_data_file` ([outputs](#outputs)) |
| Host | Required. It names the machine in `meta-data` and is where `apply` waits |
| Editor schemas | Yes: `meridian schema sync` reads the cloud-config schema cloud-init publishes, at the installed version's tag ([editor schemas](#editor-schemas)) |
| Plugin binary | `meridian-target-cloud-init` |
| Needs | `cloud-init` for the check (Linux only; the check can be turned off), and `ssh` only to wait ([settings](#settings)) |

## How cloud-init differs from the other targets

The machine applies user-data itself, once, on first boot:

1. **cloud-init sets the order**, at fixed boot steps. See
   [the order of a first boot](#the-order-of-a-first-boot).
2. **Nothing can be removed later.** What has no module becomes a command; the rest is
   [refused](#what-is-refused).
3. **`meridian apply` only checks and stores the user-data**, and can wait for the boot.
   See [plan and apply](#plan-and-apply).

## A first example

[`examples/cloud-init/bootstrap.yaml`](../../examples/cloud-init/bootstrap.yaml): an
account, packages, a hardened SSH daemon and a Python environment.

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: bootstrap
spec:
  target: cloud-init
  targetHost:
    from: inline
    host: app01.example.com
  resources:
    - id: ops_group
      type: group
      params:
        name: ops

    - id: deploy_user
      type: user
      params:
        name: deploy
        gecos: Deployment account
        shell: /bin/bash
        groups: [ops]
        sudo: "ALL=(ALL) NOPASSWD:ALL"
        ssh_authorized_keys:
          - ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKeyOnlyReplaceWithYourOwnPublicKey00 deploy@example.com
      dependsOn:
        - resource: ops_group

    - id: python
      type: package
      params:
        name: python3

    - id: python_venv
      type: package
      params:
        name: python3-venv

    # Owned by a user this document creates, so it is written after the user
    # exists. Nothing here has to say so.
    - id: app_env
      type: file
      params:
        path: /etc/app/env
        content: |
          APP_ENV=production
          APP_PORT=8080
        owner: deploy
        group: ops
        mode: "0640"

    - id: sshd_hardening
      type: file
      params:
        path: /etc/ssh/sshd_config.d/10-meridian.conf
        content: |
          PasswordAuthentication no
          PermitRootLogin no
        mode: "0600"
      notifies:
        - resource: sshd
          action: reload

    - id: sshd
      type: service
      params:
        name: ssh
        enabled: true

    - id: app_dir
      type: file
      state: directory
      params:
        path: /srv/app
        owner: deploy
        group: ops
        mode: "0750"
      dependsOn:
        - resource: deploy_user

    - id: app_venv
      type: exec
      params:
        command: python3 -m venv venv
        chdir: /srv/app
        creates: /srv/app/venv
      dependsOn:
        - resource: app_dir
        - resource: python_venv
```

Compile it:

```console
$ meridian compile examples/cloud-init/bootstrap.yaml --out build
bootstrap (ResourceSet, target cloud-init on app01.example.com)
  warning: cloud-init: cloud-init has no module for services, so resource "sshd" compiled to systemctl commands that run at the end of the first boot; the image must use systemd
  warning: cloud-init: cloud-init applies user-data once and never sees a later change, so notify is emulated: every notified action runs once, after all other first-boot commands, whether or not the notifying resource changed anything
  compiled 2 files into build/bootstrap
```

`build/bootstrap/user-data` is what the machine will read:

```yaml
#cloud-config
# Compiled by Meridian from the document "bootstrap". Change the document, not this file.
write_files:
  - path: /etc/ssh/sshd_config.d/10-meridian.conf
    content: |
      PasswordAuthentication no
      PermitRootLogin no
    permissions: "0600"
  - path: /etc/app/env
    content: |
      APP_ENV=production
      APP_PORT=8080
    owner: deploy:ops
    permissions: "0640"
    defer: true
groups:
  - ops
users:
  - default
  - name: deploy
    gecos: Deployment account
    groups:
      - ops
    shell: /bin/bash
    ssh_authorized_keys:
      - ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleKeyOnlyReplaceWithYourOwnPublicKey00 deploy@example.com
    sudo: ALL=(ALL) NOPASSWD:ALL
packages:
  - python3
  - python3-venv
runcmd:
  - set -e
  - systemctl enable --now ssh
  - install -d -m 0750 -o deploy -g ops /srv/app
  - if [ ! -e /srv/app/venv ]; then ( cd /srv/app && python3 -m venv venv ); fi
  - systemctl reload ssh
```

- `app_env` is owned by `deploy`, a user this document creates, so it is deferred.
- `users` starts with `default`, keeping the image's login user (`ubuntu`, `ec2-user`).
- The service, directory and virtual environment are `runcmd` commands.
- The notified reload is the last line.

`build/bootstrap/meta-data` completes a NoCloud seed:

```yaml
instance-id: meridian-bootstrap
local-hostname: app01.example.com
```

`local-hostname` is the document's host, left out for an IP address.

## A second example: a web server

[`examples/cloud-init/web-server.yaml`](../../examples/cloud-init/web-server.yaml): a
package, its config file, and the service the file reloads.

```yaml
resources:
  - id: nginx_pkg
    type: package
    params:
      name: nginx

  - id: nginx_conf
    type: file
    params:
      path: /etc/nginx/conf.d/site.conf
      content: |
        server {
          listen 80;
          root /srv/www;
        }
      owner: root
      mode: "0644"
    dependsOn:
      - resource: nginx_pkg
    notifies:
      - resource: nginx_svc
        action: reload

  - id: nginx_svc
    type: service
    state: running
    params:
      name: nginx
      enabled: true
    dependsOn:
      - resource: nginx_pkg
```

It compiles to
([the recorded file](../../plugins/cloud-init/testdata/web-server.user-data)):

```yaml
#cloud-config
# Compiled by Meridian from the document "web". Change the document, not this file.
write_files:
  - path: /etc/nginx/conf.d/site.conf
    content: |
      server {
        listen 80;
        root /srv/www;
      }
    owner: root
    permissions: "0644"
    defer: true
packages:
  - nginx
runcmd:
  - set -e
  - systemctl enable --now nginx
  - systemctl reload nginx
```

The file is deferred until nginx is installed. The reload always runs.

## What a document compiles to

Two files, in a directory named after the document:

| File | What it is |
|---|---|
| `user-data` | The `#cloud-config` document. Its first line is `#cloud-config`, which is how cloud-init recognises it, and its second is a comment naming the Meridian document it came from |
| `meta-data` | `instance-id: meridian-<document name>` and, when the host is a name and not an IP address, `local-hostname: <host>` |

`user-data` holds up to five keys, in this order; empty keys are left out:

| Key | Holds |
|---|---|
| `write_files` | every `file` with content: first the ones written at once, then the deferred ones |
| `groups` | every `group` |
| `users` | `default`, then every `user` |
| `packages` | every `package` |
| `runcmd` | `set -e`, then every `exec`, `service`, directory and removal in dependency order, then the notified actions |

Ties keep document order. Other keys come from
[modules](#cloud-config-modules-beside-the-six).

## The order of a first boot

![The five steps of a first boot, in order: write_files, groups and users, packages, deferred write_files, runcmd](images/cloud-init-first-boot.svg)

Each resource goes to one of five steps; each `dependsOn` is then checked.

| Step | cloud-init key | Resources |
|---|---|---|
| 1 | `write_files` | a `file` with content |
| 2 | `groups`, then `users` | `group`, `user` |
| 3 | `packages` | `package` |
| 4 | `write_files` with `defer: true` | a `file` with content that has something to wait for |
| 5 | `runcmd`, top to bottom | `exec`, `service`, a `file` in state `directory` or `absent`, and last the notified actions |

**Deferred files** go to step 4 when they depend on a `group`, `user`, `package` or
deferred file, or their `owner` or `group` is created by the document.

**A `dependsOn` pointing to a later step** is refused:

```text
site.yaml:18:11: cloud-init: resource "conf" [unsatisfiable-order]: depends on "setup", but cloud-init writes files before it runs commands: the order of a first boot is cloud-init's own and a document cannot change it
```

Make the later thing a command too, such as an `exec` that writes the file.

Step 5 runs in dependency order under `set -e`, so the first failure stops the rest.

## Resource reference

Anything not listed is refused.

### `package`

| Param | | |
|---|---|---|
| `name` | required | The package name. |
| `version` | optional | A version to pin, as a string. Compiles to a `[name, version]` pair. |

State: `present` (the default) only.

### `cron`

A file `/etc/cron.d/<name>` under `write_files`: `<minute> <hour> <day> <month> <weekday> <user> <command>`. `name` is letters, digits, `-` and `_`. State: `present` only.

### `packageRepository`

| Param | | |
|---|---|---|
| `name`, `format`, `url` | required | apt: an entry `<name>.list` under `apt: sources:`. yum: an entry under `yum_repos:`. |
| `distribution`, `components` | apt | `distribution` required. |
| `key` | yum only | The key's URL. An apt key URL is refused: cloud-init takes a key's text or id. |

State: `present` (the default) only. Added before packages are installed.

### `file`

| Param | | |
|---|---|---|
| `path` | required | An absolute path. |
| `content` | required unless the state is `directory` or `absent` | The whole content of the file. |
| `owner`, `group` | optional | Compile to cloud-init's single `owner` value: `owner` alone, `owner:group`, or `root:group` when only the group is given. cloud-init defaults each to `root`. Neither may hold a colon. |
| `mode` | optional | An octal mode as a string, such as `"0644"`. Compiles to `permissions`. |
| `encoding` | optional | How `content` is encoded. One of `b64`, `base64`, `gz`, `gzip`, `gz+b64`, `gz+base64`, `gzip+b64`, `gzip+base64`, `text/plain`. Left out, the content is plain text. |
| `append` | optional | `true` adds to the file instead of replacing it. Needed for a second resource with the same path. |

| State | Compiles to |
|---|---|
| `present` or `file` (the default) | an entry under `write_files` |
| `directory` | `install -d -m <mode> -o <owner> -g <group> <path>` under `runcmd` |
| `absent` | `rm -rf -- <path>` under `runcmd` |
| `link` | refused: write an `exec` that runs `ln -s` |
| `source` | refused: write an `exec` that runs `curl` |

- A file with no content is refused.
- `directory` takes no `content`, `encoding` or `append`; `absent` takes `path` only.
- A directory is created with its parents; `mode`, `owner` and `group` apply to the last.
- `absent` removes whatever is at the path, recursively.

### `user`

| Param | | cloud-init key |
|---|---|---|
| `name` | required | `name` |
| `uid` | optional, a whole number | `uid` |
| `home` | optional | `homedir` |
| `shell` | optional | `shell` |
| `groups` | optional, one name or a list | `groups` |
| `primary_group` | optional | `primary_group` |
| `gecos` | optional | `gecos` |
| `system` | optional, `true` or `false` | `system` |
| `lock_passwd` | optional, `true` or `false` | `lock_passwd` |
| `sudo` | optional, one rule or a list | `sudo` |
| `ssh_authorized_keys` | optional, one key or a list | `ssh_authorized_keys` |

State: `present` (the default) only. The last five params are cloud-init's own.

### `group`

| Param | | |
|---|---|---|
| `name` | required | The group name. |
| `members` | optional, one name or a list | Users to add to it. |

State: `present` (the default) only. `gid` is refused.

### `service`

Compiles to `systemctl` commands under `runcmd`, with a warning that the image needs
systemd.

| Param | | |
|---|---|---|
| `name` | required | The unit name. |
| `enabled` | optional, `true` or `false` | Whether it starts at boot. |

| State | `enabled` not given | `enabled: true` | `enabled: false` |
|---|---|---|---|
| `running` or `started` (the default) | `systemctl start` | `systemctl enable --now` | `systemctl disable`, then `start` |
| `stopped` | `systemctl stop` | `systemctl enable`, then `stop` | `systemctl disable --now` |
| `restarted` | `systemctl restart` | `systemctl enable`, then `restart` | `systemctl disable`, then `restart` |
| `reloaded` | `systemctl reload` | `systemctl enable`, then `reload` | `systemctl disable`, then `reload` |

### `exec`

| Param | | |
|---|---|---|
| `command` | required | The command line. |
| `shell` | optional | `true` runs the command as a line of shell: pipes, variables and redirection work. |
| `chdir` | optional | A directory to run it in. |
| `creates` | optional | A path. The command is skipped if it exists. |
| `removes` | optional | A path. The command is skipped if it does not exist. |

`exec` takes no state.

Without `shell: true` each word is quoted, so nothing expands:

```yaml
- id: warm
  type: exec
  params:
    command: /usr/local/bin/warm-cache --tag "nightly build" $HOME
```

```yaml
runcmd:
  - set -e
  - /usr/local/bin/warm-cache --tag 'nightly build' '$HOME'
```

`chdir`, `creates` and `removes` wrap the command; `chdir` runs in a subshell:

```yaml
runcmd:
  - set -e
  - if [ ! -e /srv/app/venv ]; then ( cd /srv/app && python3 -m venv venv ); fi
```

## cloud-config modules beside the six

Any cloud-config module is a type by its key (`ntp`, `apt`, `timezone`, `bootcmd`). Its
`params` are written as given and checked by cloud-init at plan. A non-mapping module
(`timezone`, `bootcmd`) takes one param, `value`.

`native/<name>` is refused. The keys the six write (`write_files`, `packages`, `users`,
`groups`, `runcmd`) are refused as types.

```yaml
resources:
  - id: clock
    type: ntp
    params:
      enabled: true
      servers: [0.pool.ntp.org, 1.pool.ntp.org]
  - id: zone
    type: timezone
    params:
      value: Europe/Amsterdam
  - id: early
    type: bootcmd
    params:
      value: ["echo booting > /dev/kmsg"]
```

compiles to

```yaml
ntp:
  enabled: true
  servers:
    - 0.pool.ntp.org
    - 1.pool.ntp.org
timezone: Europe/Amsterdam
bootcmd:
  - echo booting > /dev/kmsg
```

| | |
|---|---|
| `type` | a cloud-config key: a lower-case word of letters, digits, underscores and hyphens. A wrong case, a dotted name, `native/<name>`, or one of the five keys the shared types write is refused |
| `params` | the module's keys, as written; or the one param `value` for a module whose value is a string or a list. `value` stands alone (`invalid-param` beside other params); a module given no params at all is refused (`missing-param`) |
| `state` | none: a module's own keys say what it does. Any `state` is refused (`unsupported-state`) |
| `dependsOn` | refused at either end (`unsatisfiable-order`). cloud-init applies each module at a point of the first boot that is the module's own, which this target does not know, so a document cannot order anything before or after it |
| `notifies` to a module | refused (`unsupported-action`): a module is applied once and there is no command that runs it again |
| `notifies` from a module | as from any resource: the action runs once, last |
| `runtimeWhen` | refused, as for the six |
| Secrets | not on this target |
| References | `${id.param}` is settled from the document, as for the six |
| two resources of one module | refused (`duplicate-resource`): cloud-init reads one mapping, and one resource writes a module's whole value |

[`examples/cloud-init/native-modules.yaml`](../../examples/cloud-init/native-modules.yaml)
uses both. A document with a module compiles for cloud-init only; the
[`native-type`](../reference/lint.md#native-type) lint marks it. See
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Editor schemas

`meridian schema sync` reads the
[cloud-init](https://github.com/canonical/cloud-init) cloud-config schema at the installed
version's tag, or `main` with a note ([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | every module: a module whose value is a mapping takes its keys as params, with their kinds and values; a module whose value is a string or a list takes the one param `value`, as the compiler reads it |
| Listed by name | nothing: the schema is small and every module is described |
| Pinned | the cloud-init version or branch, and the schema's SHA-256 |

The six types' keys are left out of the cache.

## References

`${id.param}` is settled at compile, as on the other CM targets
([rules](../reference/resource-types.md#references)). Write `$${...}` for a literal `${...}`;
`${HOME}` is refused:

```yaml
- id: profile
  type: file
  params:
    path: /etc/profile.d/app.sh
    content: |
      export APP_HOME=$${HOME}/app
```

```yaml
write_files:
  - path: /etc/profile.d/app.sh
    content: |
      export APP_HOME=${HOME}/app
```

The host can come from an Infrastructure output (`targetHost.from: infrastructure`). See
[waiting for the first boot](#waiting-for-the-first-boot).

## Notifications

Each notified action runs once, after every other command, with a warning.

| Notified type | Actions | Compiles to |
|---|---|---|
| `service` | `start`, `stop`, `restart`, `reload` | `systemctl <action> <name>` |
| `exec` | `run` | the resource's own command again |

Duplicates share one command. Actions sort by action then id (`reload web` before
`restart db`). A notified service still gets its state's commands. Other types, modules and
actions are refused (`unsupported-action`).

## What is refused

Each names the resource, rule and line.

| You wrote | Rule | Why |
|---|---|---|
| `state: absent` on a `user`, `group` or `package`; `state: latest` on a `package` | `unsupported-state` | A first boot adds things. Use an `exec` to remove what an image ships with. |
| `runtimeWhen` on any resource | `unsupported-runtime-condition` | cloud-init evaluates nothing per resource. Put the test inside an `exec` command. |
| a `dependsOn` that points backwards through the five steps, or at either end of a module of cloud-init's own | `unsatisfiable-order` | The order of a first boot cannot be changed, and where a module falls in it is cloud-init's to know. |
| a `file` with no `content` | `missing-param` | It would empty the file. |
| a relative `path` | `invalid-path` | A first boot has no working directory. |
| removing `/` | `invalid-path` | |
| `mode: 644` without quotes, or a mode that is not octal | `invalid-param` | cloud-init takes the mode as a string of octal digits. |
| two files with one `path`, the second without `append: true` | `duplicate-resource` | The second would silently replace the first. |
| two users, groups or packages with one name | `duplicate-resource` | |
| two resources of one cloud-config module | `duplicate-resource` | cloud-init reads one mapping; the second would replace the first. |
| a command with an unclosed quote and no `shell: true` | `invalid-param` | It cannot be split into words. |
| a param the type does not take here, such as `gid` on a `group`; `mode`, `owner` or `group` on a file in state `absent` | `unsupported-param` | The message lists the ones accepted. |
| a resource type that is neither one of the six nor a cloud-config key: a wrong case, a dotted name, `native/<name>`, or a key a shared type writes (`packages`) | `unsupported-resource-type` | The message lists the six and says how a module is written. |
| a required param left out, such as `name` or `path` | `missing-param` | |
| a param that is there and empty, such as `command: ""` | `empty-param` | An empty name or command is a mistake, not a request for nothing. |
| `value` beside other params on a module; a module with no params; a `state` on a module | `invalid-param`, `missing-param`, `unsupported-state` | A module's value is the one param `value`, or its keys as params. |
| a param of the wrong kind, such as a list where one string is expected | `invalid-param` | The message says what was expected and what was found. |
| an `encoding` cloud-init does not read | `invalid-param` | The message lists the ones accepted. |
| an `owner` or `group` on a file that holds a colon | `invalid-param` | cloud-init joins the two with a colon. Give them as separate params. |
| a user or group name with a space or a colon; a package name with a space | `invalid-name` | It would be read as two values. |
| notifying a `file`, `package`, `user` or `group`, a module of cloud-init's own, or an action the type does not have | `unsupported-action` | Only a service or an exec can react. |
| notifying an id that is not in the document | `unknown-notify-target` | |
| a document with no host | `missing-host` | The host names the machine in `meta-data`. |

`ordering-violated` is an internal guard; report it if you see it.

### Warnings

There are three:

| Warning | When | What to do |
|---|---|---|
| services compiled to `systemctl` commands | the document has a `service` | Make sure the image uses systemd. |
| notify is emulated | the document has a `notifies` | Nothing, if a reaction that always runs once is acceptable. |
| user-data is larger than 16 KB | the compiled file is over 16384 bytes | 16 KB is what Amazon EC2 accepts. Other clouds allow more. Move large files out of the document, or set `encoding: gz+b64` on content you have compressed. |

## Plan and apply

**`meridian plan`** runs `cloud-init schema` locally and reports whether the seed would
change.

```console
$ meridian plan bootstrap.yaml
bootstrap (ResourceSet, target cloud-init on app01.example.com)
  + create user-data: seed would be written to /work/.meridian/resourceset/bootstrap/cloud-init/seed; a machine applies it on its first boot
  1 change to make
```

**`meridian apply`** checks, then writes the **seed** directory (`user-data`, `meta-data`).

```console
$ meridian apply bootstrap.yaml
bootstrap (ResourceSet, target cloud-init on app01.example.com)
  + create user-data: seed written to /work/.meridian/resourceset/bootstrap/cloud-init/seed; a machine applies it on its first boot
  applied 1 change
  output seed_dir = "/work/.meridian/resourceset/bootstrap/cloud-init/seed"
  output user_data_file = "/work/.meridian/resourceset/bootstrap/cloud-init/seed/user-data"
```

A current seed is left alone. Rejected user-data never becomes the seed.

### Outputs

| Output | Value |
|---|---|
| `seed_dir` | The absolute path of the seed directory, which holds `user-data` and `meta-data`. |
| `user_data_file` | The absolute path of the `user-data` file inside it. |

Local paths, known after apply.

### Handing the user-data to the document that creates the machine

The document that creates the machine
[imports](../reference/dsl/outputs-and-hand-offs.md#imports) the compiled `user-data` as a
variable:

```yaml
kind: Infrastructure
metadata: {name: machine}
spec:
  target: terraform
  imports:
    - name: user_data
      from: artifact
      ref: bootstrap          # this cloud-init document
      file: user-data
      sensitive: true
  resources:
    - id: user_data
      type: variable
      params: {type: string, sensitive: true}
    - id: web
      type: aws_instance
      params:
        ami: ami-0abcdef1234567890
        instance_type: t3.micro
        user_data: ${user_data}
```

This document runs first. Give it an `inline` host; a host from the machine it bootstraps
is a cycle `validate` refuses. Example:
[`examples/workflow/bootstrap-then-machine.yaml`](../../examples/workflow/bootstrap-then-machine.yaml).
Importing targets: `terraform`, `opentofu`, `packer`, `pulumi`, `arm`.

### Using the seed outside a workflow

Hand `user_data_file` to whatever creates the machine:

```console
$ aws ec2 run-instances --user-data file://.meridian/resourceset/bootstrap/cloud-init/seed/user-data ...
$ az vm create --custom-data .meridian/resourceset/bootstrap/cloud-init/seed/user-data ...
$ multipass launch --cloud-init .meridian/resourceset/bootstrap/cloud-init/seed/user-data
```

Without a metadata service, make a NoCloud disk from the seed:

```console
$ cloud-localds seed.iso .meridian/resourceset/bootstrap/cloud-init/seed/user-data \
    .meridian/resourceset/bootstrap/cloud-init/seed/meta-data
```

### Waiting for the first boot

With `MERIDIAN_CLOUD_INIT_WAIT=ssh`, `apply` runs `cloud-init status --wait --long` on the
host over SSH and fails the stage if the boot failed, so later stages start on a ready
machine:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: bootstrap
spec:
  target: cloud-init
  targetHost:
    from: infrastructure
    ref: network
    output: address
  resources:
    - id: python
      type: package
      params:
        name: python3
```

### Settings

Read from the environment.

| Variable | Default | Meaning |
|---|---|---|
| `MERIDIAN_CLOUD_INIT` | `cloud-init` | The cloud-init program used for the check. |
| `MERIDIAN_CLOUD_INIT_VALIDATE` | `on` | `off` skips the check, for a machine with no cloud-init, such as a Mac. |
| `MERIDIAN_CLOUD_INIT_WAIT` | `off` | `ssh` makes `apply` wait for the first boot. |
| `MERIDIAN_CLOUD_INIT_SSH` | `ssh` | The SSH program used to wait. |
| `MERIDIAN_CLOUD_INIT_SSH_ARGS` | none | Extra SSH arguments, split like a shell command line (quotes hold a word together), such as `-l ubuntu -i ~/.ssh/deploy`. |

Settings can also go in `meridian.yaml` under `spec.settings.cloud-init`; the environment
wins. See the [plugin settings reference](../reference/plugin-settings.md#cloud-init). SSH
uses `-o BatchMode=yes`. A missing cloud-init fails the check rather than skipping it.

### Reading a failure

A rejected user-data is `invalid-config`. When the wait finds the boot failed, each
module `cloud-init status --long` lists under `errors:` is a [resource failure](../reference/state.md#resource-failures) on the host: the
module (`scripts_user`) as the resource, the exception class as the tool code, its
message as the summary. A host SSH cannot reach is `unreachable` (`auth-failed` for a
refused key), tool code `rc=255`.

## Troubleshooting

**`could not run cloud-init: it is not installed`.** Install it (Linux only), set
`MERIDIAN_CLOUD_INIT`, or set `MERIDIAN_CLOUD_INIT_VALIDATE=off`.

**`cloud-init rejected the user-data`.** cloud-init's report is above. Please report it with
the document.

**`cloud-init on <host> reports that the first boot failed`.** See
`cloud-init status --long` and `/var/log/cloud-init-output.log` on the machine.

**`could not reach <host> over ssh`.** Check `MERIDIAN_CLOUD_INIT_SSH_ARGS` and the account.

**Unexpected `defer: true`.** See [the order of a first boot](#the-order-of-a-first-boot).

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `--debug` to the local `cloud-init schema` check.

## How this plugin is tested

- Golden files in `plugins/cloud-init/testdata` for the examples, an all-keys document and
  the shared fixtures.
- Compiled commands run in a real shell.
- Stand-ins for `cloud-init` and `ssh`; the binary is driven through the orchestrator.
- CI checks every recorded user-data with real `cloud-init schema`.
- A test compares this page's example with the files.

## Inspecting one resource

Not supported: cloud-init applies the user-data once, at first boot, and keeps nothing to read
per resource. [`meridian inspect`](../reference/cli.md#inspect) is refused with
`unsupported-inspect`.
