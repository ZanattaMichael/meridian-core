# Target: `cloud-init`

<img src="../branding/logos/meridian-cloud-init.svg" width="72" alt="Meridian cloud-init plugin logo">

Compiles a ResourceSet into **user-data**: the `#cloud-config` document a cloud
machine reads the first time it boots. Use it for what has to be true before
anything can reach the machine, such as the account a later stage logs in with,
its SSH key, and the packages a configuration tool needs to find there.

## What it does and does not support

| | |
|---|---|
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([what each takes here](#resource-reference)), and any cloud-config module beside them: `ntp`, `apt`, `timezone`, `bootcmd` ([modules beside the six](#cloud-config-modules-beside-the-six)) |
| `dependsOn` | Yes, within [the order of a first boot](#the-order-of-a-first-boot). cloud-init handles files, accounts, packages and commands at fixed steps, so a dependency that points backwards through those steps is refused. Nothing can depend on a module of cloud-init's own, or be depended on by one |
| `notifies` | Emulated: each notified action is a command that runs once, after every other command, with a warning. Actions `start`, `stop`, `restart`, `reload` on a service, `run` on an exec ([how](#notifications)) |
| `runtimeWhen` | No. It is refused: cloud-init evaluates nothing per resource |
| States | Only what a first boot can do. `absent` on a user, group or package and `latest` on a package are refused ([everything refused](#what-is-refused)) |
| References (`${...}`) | `${id.param}` is settled from the document when it is compiled; anything else is refused. `$${` is a literal `${` ([references](#references)) |
| Plan | Yes: `cloud-init schema` checks the user-data, and the plan says whether the seed would change. No machine is contacted |
| Apply | Writes the user-data and a NoCloud seed on the machine running Meridian. The new machine applies it by itself on its first boot. `apply` can wait for that boot over SSH ([plan and apply](#plan-and-apply)) |
| Outputs | Yes: `seed_dir` and `user_data_file` ([outputs](#outputs)) |
| Host | Required. It names the machine in `meta-data` and is where `apply` waits |
| Plugin binary | `meridian-target-cloud-init` |
| Needs | `cloud-init` for the check (Linux only; the check can be turned off), and `ssh` only to wait ([settings](#settings)) |

On this page:

- [How cloud-init differs from the other targets](#how-cloud-init-differs-from-the-other-targets)
- [A first example](#a-first-example) and [a second: a web server](#a-second-example-a-web-server)
- [What a document compiles to](#what-a-document-compiles-to)
- [The order of a first boot](#the-order-of-a-first-boot)
- [Resource reference](#resource-reference): every shared type, param and state
- [cloud-config modules beside the six](#cloud-config-modules-beside-the-six)
- [References](#references)
- [Notifications](#notifications)
- [What is refused](#what-is-refused) and [warnings](#warnings)
- [Plan and apply](#plan-and-apply), [outputs](#outputs) and [settings](#settings)
- [Troubleshooting](#troubleshooting)

## How cloud-init differs from the other targets

Ansible, Puppet and Chef are pointed at a machine that exists and can be run against it
again and again. cloud-init is not. A machine is created *with* user-data and applies it
by itself, once, on its first boot. Three things follow, and the rest of this page is
those three things in detail:

1. **The order is cloud-init's.** Files, users, packages and commands are each handled
   at a fixed point of the boot, whatever order the document lists them in. See
   [the order of a first boot](#the-order-of-a-first-boot).
2. **Nothing can be undone or changed later.** A first boot can add a user or a package
   and cannot remove one. What cloud-init has no module for becomes a command, and what
   cannot be a command is refused. See [what is refused](#what-is-refused).
3. **Meridian does not apply it, the machine does.** `meridian apply` checks the
   user-data, keeps it where whatever creates the machine can read it, and can wait for
   the machine's first boot to finish. See [plan and apply](#plan-and-apply).

## A first example

[`examples/cloud-init/bootstrap.yaml`](../../examples/cloud-init/bootstrap.yaml)
gives a new machine an account to log in with, the packages a later stage needs, a
hardened SSH daemon and a Python environment:

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

Things to notice in it:

- The document listed `app_env` before `sshd_hardening`, and the output has them the
  other way round. `app_env` is owned by `deploy`, a user this document creates, so it
  carries `defer: true` and is written after the user exists.
- `users` starts with `default`. Listing any user replaces cloud-init's own list, which
  holds the image's login user (`ubuntu`, `ec2-user` and so on). Meridian keeps it,
  because a document that adds a user did not ask to lose the one a cloud's console and
  key injection rely on.
- The service, the directory and the virtual environment are all under `runcmd`,
  because cloud-init has a module for none of them.
- The reload that `sshd_hardening` notifies is the last line.

The second file, `build/bootstrap/meta-data`, makes the directory a complete seed for
cloud-init's NoCloud data source:

```yaml
instance-id: meridian-bootstrap
local-hostname: app01.example.com
```

`local-hostname` is the document's host. It is left out when the host is an IP address,
which reaches a machine but does not name it.

## A second example: a web server

[`examples/cloud-init/web-server.yaml`](../../examples/cloud-init/web-server.yaml)
holds the same three resources as the Ansible, Puppet and Chef web server
examples, with the target changed: a package, its configuration file, and the
service the file reloads.

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

The file depends on the package, so it is deferred and written after nginx is
installed. Where Ansible would reload nginx only if the file changed, here the
reload always runs, because on a first boot the file always changes.

## What a document compiles to

Two files, in a directory named after the document:

| File | What it is |
|---|---|
| `user-data` | The `#cloud-config` document. Its first line is `#cloud-config`, which is how cloud-init recognises it, and its second is a comment naming the Meridian document it came from |
| `meta-data` | `instance-id: meridian-<document name>` and, when the host is a name and not an IP address, `local-hostname: <host>` |

`user-data` holds up to five keys, always in this order, and a key with nothing
under it is left out:

| Key | Holds |
|---|---|
| `write_files` | every `file` with content: first the ones written at once, then the deferred ones |
| `groups` | every `group` |
| `users` | `default`, then every `user` |
| `packages` | every `package` |
| `runcmd` | `set -e`, then every `exec`, `service`, directory and removal in dependency order, then the notified actions |

Resources keep the order the document lists them in wherever dependencies leave
a choice, so the same document always compiles to the same bytes.

Meridian writes no other cloud-init key. For a key it does not cover, such as
`apt` sources, `ntp` or `disk_setup`, use an `exec`, or keep that part of the
user-data outside Meridian.

## The order of a first boot

![The five steps of a first boot, in order: write_files, groups and users, packages, deferred write_files, runcmd](images/cloud-init-first-boot.svg)

cloud-init runs each of its modules at a fixed point. Meridian places every resource at
one of five steps and then checks each `dependsOn` against the result.

| Step | cloud-init key | Resources |
|---|---|---|
| 1 | `write_files` | a `file` with content |
| 2 | `groups`, then `users` | `group`, `user` |
| 3 | `packages` | `package` |
| 4 | `write_files` with `defer: true` | a `file` with content that has something to wait for |
| 5 | `runcmd`, top to bottom | `exec`, `service`, a `file` in state `directory` or `absent`, and last the notified actions |

**When a file is deferred.** Meridian writes a file at step 4 instead of step 1 when
either is true:

- it has a `dependsOn` on a `group`, a `user`, a `package` or another deferred file;
- its `owner` or `group` is one this document creates.

You never write `defer` yourself. A configuration file that depends on the package it
configures is the usual case: it is written after the package is installed, so the
package's own default does not replace it.

**When a dependency is refused.** A `dependsOn` has to point from an earlier step to a
later one, or stay within a step. One that points backwards cannot be honoured, and the
compile says which two steps are the wrong way round:

```text
site.yaml:18:11: cloud-init: resource "conf" [unsatisfiable-order]: depends on "setup", but cloud-init writes files before it runs commands: the order of a first boot is cloud-init's own and a document cannot change it
```

The usual fix is to make the later thing a command too. A file that must be written
after a command can be written by an `exec`.

Within step 5 the commands run in the order the dependencies give them, and the list
starts with `set -e`, so the first command that fails stops the rest. Without it
cloud-init would carry on, run resources whose dependency had failed, and report only
the last command's result.

## Resource reference

A param or state that is not listed here is refused at compile time. Nothing is dropped
silently.

### `package`

| Param | | |
|---|---|---|
| `name` | required | The package name. |
| `version` | optional | A version to pin, as a string. Compiles to a `[name, version]` pair. |

State: `present` (the default) only.

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

A file with no content is refused, because cloud-init would write an empty file over
whatever is there.

A file in state `directory` or `absent` takes no `content`, `encoding` or `append`.
A file in state `absent` takes `path` only. A directory is created with its parents,
and `mode`, `owner` and `group` apply to the last one. A removal takes whatever is at
the path, a directory and its contents included.

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

State: `present` (the default) only. The last five params are cloud-init's own. They
are here because the account and key a first boot sets up are usually the reason for
using cloud-init at all.

### `group`

| Param | | |
|---|---|---|
| `name` | required | The group name. |
| `members` | optional, one name or a list | Users to add to it. |

State: `present` (the default) only. cloud-init cannot choose a group's id, so `gid` is
refused.

### `service`

cloud-init has no module for services. A service compiles to `systemctl` commands under
`runcmd`, and the compile warns that the image must use systemd.

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

Without `shell: true` the command is split into words and each word is quoted, so
nothing in it is expanded:

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

`chdir`, `creates` and `removes` are compiled around the command. The directory change
is kept in a subshell, so it does not carry over to the next command:

```yaml
runcmd:
  - set -e
  - if [ ! -e /srv/app/venv ]; then ( cd /srv/app && python3 -m venv venv ); fi
```

## cloud-config modules beside the six

Any cloud-config module is a resource type, written by the key cloud-init reads it
under: `ntp`, `apt`, `timezone`, `bootcmd`, `ssh_import_id`, `ca_certs`. The resource's
`params` are the module's own keys, written under it exactly as given; Meridian checks
nothing against what the module takes. A key the module does not have is cloud-init's
to refuse when it validates the user-data (`meridian plan` runs that check), as a
provider's type is Terraform's, until the module's schema is synced and the editor
reports it in place. A module whose value is not a mapping, such as `timezone` (a
string) or `bootcmd` (a list), takes the one param `value`.

None of the six shared types is a cloud-config module of its own name, so `native/<name>`
reaches nothing here and is refused. The keys the six write, `write_files`, `packages`,
`users`, `groups` and `runcmd`, are theirs alone: writing `type: packages` is refused,
with the shared type to use, so that two resources never write one key.

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
| two resources of one module | refused (`duplicate-module`): cloud-init reads one mapping, and one resource writes a module's whole value |

[`examples/cloud-init/native-modules.yaml`](../../examples/cloud-init/native-modules.yaml)
is a whole document of both kinds. A document that names a module compiles for
cloud-init only. The [`native-type`](../reference/lint.md#native-type) lint rule marks
each such resource. The six shared types stay the portable way to say what they can
say; a module is for everything else. The split is
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## References

`${id.param}` in a param becomes the value the document gives that param on the
resource `id`, when the document is compiled. A reference that cannot be settled
that way is refused. This is the same on the `ansible`, `puppet`, `chef` and
`salt` targets, and the
[shared types](../reference/resource-types.md#references) page has the rules.

On this target a `${...}` in a file's content or in a command is most often
meant for the shell or for the program that reads the file. Write it `$${...}`
and it reaches the user-data as `${...}`. Left as `${HOME}` it is refused,
because `HOME` is not a resource of the document:

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

Without the second `$` that document is refused, because `HOME` is not a
resource in it. Nothing is written out unresolved.

The one value this target takes from another document is its host. Write
`targetHost` with `from: infrastructure` and the host is the named output of an
Infrastructure document that ran earlier in the same workflow. See
[waiting for the first boot](#waiting-for-the-first-boot).

## Notifications

cloud-init applies user-data once and never sees a later change, so it has nothing like
a handler. On that one run, though, every resource does change. Meridian therefore runs
each notified action once, after every other command, and warns that this is what
`notifies` means on this target.

| Notified type | Actions | Compiles to |
|---|---|---|
| `service` | `start`, `stop`, `restart`, `reload` | `systemctl <action> <name>` |
| `exec` | `run` | the resource's own command again |

Two resources that notify the same action on the same resource share one command.
When several actions are notified they run in the order of their names, by action
and then by resource id, so `reload web` runs before `restart db`. A notified service
still gets the commands of its own state: a service in state `running` that is
notified `reload` is started in its place and reloaded at the end.

Notifying any other type, a module of cloud-init's own, or an action the type does not
have, is refused with `unsupported-action`.

## What is refused

Each of these stops the compile with the resource, the rule and the line.

| You wrote | Rule | Why |
|---|---|---|
| `state: absent` on a `user`, `group` or `package`; `state: latest` on a `package` | `unsupported-state` | A first boot adds things. Use an `exec` to remove what an image ships with. |
| `runtimeWhen` on any resource | `unsupported-runtime-condition` | cloud-init evaluates nothing per resource. Put the test inside an `exec` command. |
| a `dependsOn` that points backwards through the five steps, or at either end of a module of cloud-init's own | `unsatisfiable-order` | The order of a first boot cannot be changed, and where a module falls in it is cloud-init's to know. |
| a `file` with no `content` | `missing-param` | It would empty the file. |
| a relative `path` | `relative-path` | A first boot has no working directory. |
| removing `/` | `unsafe-path` | |
| `mode: 644` without quotes, or a mode that is not octal | `invalid-param`, `invalid-mode` | cloud-init takes the mode as a string of octal digits. |
| two files with one `path`, the second without `append: true` | `duplicate-file-path` | The second would silently replace the first. |
| two users, groups or packages with one name | `duplicate-user`, `duplicate-group`, `duplicate-package` | |
| two resources of one cloud-config module | `duplicate-module` | cloud-init reads one mapping; the second would replace the first. |
| a command with an unclosed quote and no `shell: true` | `invalid-command` | It cannot be split into words. |
| a param the type does not take here, such as `gid` on a `group`; `mode`, `owner` or `group` on a file in state `absent` | `unsupported-param` | The message lists the ones accepted. |
| a resource type that is neither one of the six nor a cloud-config key: a wrong case, a dotted name, `native/<name>`, or a key a shared type writes (`packages`) | `unsupported-resource-type` | The message lists the six and says how a module is written. |
| a required param left out, such as `name` or `path` | `missing-param` | |
| a param that is there and empty, such as `command: ""` | `empty-param` | An empty name or command is a mistake, not a request for nothing. |
| `value` beside other params on a module; a module with no params; a `state` on a module | `invalid-param`, `missing-param`, `unsupported-state` | A module's value is the one param `value`, or its keys as params. |
| a param of the wrong kind, such as a list where one string is expected | `invalid-param` | The message says what was expected and what was found. |
| an `encoding` cloud-init does not read | `invalid-encoding` | The message lists the ones accepted. |
| an `owner` or `group` on a file that holds a colon | `invalid-owner` | cloud-init joins the two with a colon. Give them as separate params. |
| a user or group name with a space or a colon; a package name with a space | `invalid-name` | It would be read as two values. |
| notifying a `file`, `package`, `user` or `group`, a module of cloud-init's own, or an action the type does not have | `unsupported-action` | Only a service or an exec can react. |
| notifying an id that is not in the document | `unknown-notify-target` | |
| a document with no host | `missing-host` | The host names the machine in `meta-data`. |

One more rule, `ordering-violated`, guards the plugin itself: it fires if the commands
would come out in an order that breaks a `dependsOn`. A document cannot cause it. If
you see it, please report it with the document.

### Warnings

A warning does not stop the compile. There are three:

| Warning | When | What to do |
|---|---|---|
| services compiled to `systemctl` commands | the document has a `service` | Make sure the image uses systemd. |
| notify is emulated | the document has a `notifies` | Nothing, if a reaction that always runs once is acceptable. |
| user-data is larger than 16 KB | the compiled file is over 16384 bytes | 16 KB is what Amazon EC2 accepts. Other clouds allow more. Move large files out of the document, or set `encoding: gz+b64` on content you have compressed. |

## Plan and apply

cloud-init cannot be pointed at a machine, so `plan` and `apply` are about the
user-data and where it is kept, and optionally about waiting for the machine.

**`meridian plan`** has `cloud-init schema` check the user-data on the machine running
Meridian, and reports whether the seed would change. It leaves the seed as it is and
contacts no machine.

```console
$ meridian plan bootstrap.yaml
bootstrap (ResourceSet, target cloud-init on app01.example.com)
  + create user-data: seed would be written to /work/.meridian/resourceset/bootstrap/cloud-init/seed; a machine applies it on its first boot
  1 change to make
```

**`meridian apply`** runs the same check and then writes the **seed**: a directory
holding `user-data` and `meta-data`. It reports where the seed is as two outputs.

```console
$ meridian apply bootstrap.yaml
bootstrap (ResourceSet, target cloud-init on app01.example.com)
  + create user-data: seed written to /work/.meridian/resourceset/bootstrap/cloud-init/seed; a machine applies it on its first boot
  applied 1 change
  output seed_dir = "/work/.meridian/resourceset/bootstrap/cloud-init/seed"
  output user_data_file = "/work/.meridian/resourceset/bootstrap/cloud-init/seed/user-data"
```

Applying a document whose seed is already current changes nothing. User-data that
cloud-init rejects never becomes the seed, so the seed always holds the last version
that passed.

### Outputs

| Output | Value |
|---|---|
| `seed_dir` | The absolute path of the seed directory, which holds `user-data` and `meta-data`. |
| `user_data_file` | The absolute path of the `user-data` file inside it. |

Both are paths on the machine that ran Meridian, and `apply` prints them. They
are known only once an apply has written the seed.

### Handing the user-data to the document that creates the machine

In a workflow, the document that creates the machine
[imports](../reference/dsl/outputs-and-hand-offs.md#imports) the `user-data`
file this document compiles to. It arrives as that tool's own variable, and
the machine is created with it:

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

Meridian runs this document first, because the machine needs what it compiles
to. Give this document an `inline` host, as above: if its host were the
address of the machine it bootstraps, each would wait on the other, and
`validate` refuses that. The whole workflow is
[`examples/workflow/bootstrap-then-machine.yaml`](../../examples/workflow/bootstrap-then-machine.yaml).
The targets that can import are `terraform`, `opentofu`, `packer`, `pulumi` and
`arm`.

### Using the seed outside a workflow

When something other than Meridian creates the machine, hand it
`user_data_file`. For example:

```console
$ aws ec2 run-instances --user-data file://.meridian/resourceset/bootstrap/cloud-init/seed/user-data ...
$ az vm create --custom-data .meridian/resourceset/bootstrap/cloud-init/seed/user-data ...
$ multipass launch --cloud-init .meridian/resourceset/bootstrap/cloud-init/seed/user-data
```

For a machine with no metadata service, such as a local virtual machine, turn the seed
directory into a NoCloud disk and attach it:

```console
$ cloud-localds seed.iso .meridian/resourceset/bootstrap/cloud-init/seed/user-data \
    .meridian/resourceset/bootstrap/cloud-init/seed/meta-data
```

### Waiting for the first boot

Set `MERIDIAN_CLOUD_INIT_WAIT=ssh` and `apply` also runs
`cloud-init status --wait --long` on the document's host over SSH. It returns when the
first boot is over, relays what cloud-init reports, and fails the stage if cloud-init
says the boot failed. A later stage, such as an Ansible document for the same host,
then starts only on a machine that is ready.

This is the piece that fits cloud-init into a workflow. The Infrastructure document
creates the machine with the seed's user-data and reports its address. The cloud-init
document takes that address as its host and waits there:

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

Because the machine has to be created with user-data that already exists, the first
run goes in two steps: compile or apply the cloud-init document on its own with an
inline host to produce the seed, then run the whole workflow. Passing the user-data
from one stage straight into the stage that creates the machine is not something
Meridian's orchestrator does yet.

### Settings

The plugin reads its settings from the environment of the `meridian` command.

| Variable | Default | Meaning |
|---|---|---|
| `MERIDIAN_CLOUD_INIT` | `cloud-init` | The cloud-init program used for the check. |
| `MERIDIAN_CLOUD_INIT_VALIDATE` | `on` | `off` skips the check, for a machine with no cloud-init, such as a Mac. |
| `MERIDIAN_CLOUD_INIT_WAIT` | `off` | `ssh` makes `apply` wait for the first boot. |
| `MERIDIAN_CLOUD_INIT_SSH` | `ssh` | The SSH program used to wait. |
| `MERIDIAN_CLOUD_INIT_SSH_ARGS` | none | Extra SSH arguments, split like a shell command line (quotes hold a word together), such as `-l ubuntu -i ~/.ssh/deploy`. |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.cloud-init`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#cloud-init).

SSH is always run with `-o BatchMode=yes`, so a wait fails instead of stopping to ask
for a password.

If cloud-init is not installed and the check is on, `plan` and `apply` fail and say so.
They do not skip the check on their own.

## Troubleshooting

**`could not run cloud-init: it is not installed`.** Install cloud-init on the machine
that runs Meridian, name its path with `MERIDIAN_CLOUD_INIT`, or set
`MERIDIAN_CLOUD_INIT_VALIDATE=off`. cloud-init runs only on Linux.

**`cloud-init rejected the user-data`.** The lines above the error are cloud-init's own
report. Meridian's compile should never produce user-data that fails this check, so
please report it with the document.

**`cloud-init on <host> reports that the first boot failed`.** A command under `runcmd`
failed, or a module did. On the machine, `cloud-init status --long` names the module
and `/var/log/cloud-init-output.log` has the command's output.

**`could not reach <host> over ssh`.** The wait could not log in. Check the user and key
in `MERIDIAN_CLOUD_INIT_SSH_ARGS`, and that the account in the user-data is the one
being used.

**A file came out with `defer: true` and you did not expect it.** It depends on, or is
owned by, something created at a later step. See
[the order of a first boot](#the-order-of-a-first-boot).

## How this plugin is tested

- Golden files in `plugins/cloud-init/testdata` record the exact bytes compiled from
  both examples above, from a document that uses every key the target can write, and
  from the shared fixtures in `fixtures/`. The examples are also compiled by the
  command line's own tests, with every other target's.
- The compiled commands are run by a real shell in a scratch directory, to prove the
  guards, the quoting and the stop on first failure behave as this page says.
- The runner is tested against stand-ins for `cloud-init` and `ssh`, and the plugin
  binary is driven through the host and the orchestrator as it would be in a workflow.
- The `cloud-init` workflow installs the real cloud-init and has `cloud-init schema`
  check every recorded user-data file.
- A test compares the example and its output on this page with the files, so the page
  cannot drift from what the plugin does.
