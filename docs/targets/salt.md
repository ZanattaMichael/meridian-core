# Target: `salt`

<img src="../branding/logos/meridian-salt.svg" width="72" alt="Meridian Salt plugin logo">

Compiles a ResourceSet into a [Salt](https://saltproject.io) state file and top file, and
applies it with `salt-call --local`, or `salt-ssh` for another machine.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `packageRepository`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)), and any state function beside them: `pkgrepo.managed`, `file.symlink`, `docker_container.running` ([State functions beside the six](#state-functions-beside-the-six)) |
| `dependsOn` | Yes, as `require`. The states are also written in apply order, so the dependency graph survives into the state file intact and nothing is flattened |
| `notifies` | Native: `watch` and `onchanges`. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec, `watch` on a state function of Salt's own |
| `runtimeWhen` | Yes: a Jinja expression over grains and pillar, which the minion evaluates when it renders the file |
| `adopt` | Accepted, and changes nothing: Salt keeps no state and makes each thing what the document says whether it exists or not. The compile warns |
| Secrets | Yes: as pillar data, from a pillar root only the user can read. A file state given one shows no diff; an `exec` is refused one, because Salt prints the command line. See [Secrets](#secrets) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | Yes: Salt's dry run, `test=True`. Every state reports whether it would change, so a plan is never "unknown" |
| Apply | Yes |
| Outputs | No. Salt states produce no values for a later stage to read |
| Editor schemas | Yes: `meridian schema sync` asks `salt-call` what each state function takes. See [Editor schemas](#editor-schemas) |
| Compiles to | `states/<document>.sls` and `states/top.sls` |
| Runs with | `salt-call --local` on the node itself, `salt-ssh` for another host |

What it does not do:

- **No Salt master.** Plan and apply are masterless; copy the files to a master yourself
  ([with a master](#using-the-files-with-a-salt-master)).
- **No grains, custom modules or pillar** beyond the document's [secrets](#secrets).
- **Salt's own state functions are passed through unchecked**
  ([state functions](#state-functions-beside-the-six)).
- **`restarted` and `reloaded` are not service states.** Use a notification.
- **No infrastructure.** It configures an existing node.

Contents: [Requirements](#requirements) ·
[Quick start](#quick-start) ·
[A worked example](#a-worked-example) ·
[What a document compiles to](#what-a-document-compiles-to) ·
[How resources map](#how-resources-map) ·
[State functions beside the six](#state-functions-beside-the-six) ·
[Ordering](#ordering) ·
[Notifications](#notifications) ·
[Runtime conditions](#runtime-conditions) ·
[Running it](#running-it) ·
[Editor schemas](#editor-schemas) ·
[Using the files with a Salt master](#using-the-files-with-a-salt-master) ·
[Warnings](#warnings) ·
[What the plugin refuses](#what-the-plugin-refuses) ·
[When a run fails](#when-a-run-fails) ·
[How it is tested](#how-it-is-tested)

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | Salt: `salt-call` for a local run, `salt-ssh` for another host. `pip install salt` provides both. Tested with Salt 3008 |
| A host reached over SSH | An SSH server and Python 3. No Salt minion is needed |
| To compile only | Nothing. `meridian compile` never runs Salt |

## Quick start

Select the target in a document:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: salt
  targetHost:
    from: inline
    host: localhost
  resources:
    - id: motd
      type: file
      params:
        path: /etc/motd
        content: "Managed by Meridian.\n"
```

Then:

```
meridian compile site.yaml --out build   # writes build/web/states/web.sls and top.sls
meridian plan site.yaml                  # salt-call --local ... state.apply web test=True
meridian apply site.yaml                 # the same, for real
```

Salt must be installed where Meridian runs. `host: localhost` uses `salt-call --local`
(no master or minion). Any other host uses `salt-ssh`, which needs only Python on the
host.

## A worked example

[`examples/salt/web-server.yaml`](../../examples/salt/web-server.yaml) describes a small web server:

```yaml
# A web server, described once. The Salt plugin compiles it to a state file
# and a top file; the guide in docs/targets/salt.md walks through the result.
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: salt
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: deploy_group
      type: group
      params:
        name: deploy
        gid: 1200
    - id: deploy_user
      type: user
      params:
        name: deploy
        uid: 1200
        shell: /bin/bash
        groups: [deploy]
      dependsOn:
        - resource: deploy_group
    - id: site_root
      type: file
      state: directory
      params:
        path: /srv/www/app
        owner: deploy
        group: deploy
        mode: "0755"
      dependsOn:
        - resource: deploy_user
    - id: nginx_pkg
      type: package
      params:
        name: nginx
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx/nginx.conf
        owner: root
        mode: "0644"
        content: |
          worker_processes auto;
          events { worker_connections 1024; }
          http { include /etc/nginx/sites-enabled/*; }
      dependsOn:
        - resource: nginx_pkg
      notifies:
        - resource: nginx_svc
          action: restart
    - id: nginx_site
      type: file
      params:
        path: /etc/nginx/sites-enabled/app
        content: |
          server {
            listen 80;
            root /srv/www/app;
          }
      dependsOn:
        - resource: nginx_pkg
        - resource: site_root
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
    - id: open_firewall
      type: exec
      runtimeWhen: grains['os_family'] == 'Debian'
      params:
        command: ufw allow 80/tcp
      dependsOn:
        - resource: nginx_svc
```

It compiles to two files. `states/web.sls` is the state file:

```yaml
#!jinja|yaml
# Generated by Meridian. Do not edit; this file is compiled from a Meridian document.
{%- set meridian_brace = "{" %}

deploy_group:
  group.present:
    - name: deploy
    - gid: 1200

deploy_user:
  user.present:
    - name: deploy
    - groups:
        - deploy
    - shell: /bin/bash
    - uid: 1200
    - require:
        - group: deploy_group

site_root:
  file.directory:
    - name: /srv/www/app
    - group: deploy
    - makedirs: true
    - mode: "0755"
    - user: deploy
    - require:
        - user: deploy_user

nginx_pkg:
  pkg.installed:
    - name: nginx

nginx_conf:
  file.managed:
    - name: /etc/nginx/nginx.conf
    - contents: |
        worker_processes auto;
        events { worker_connections 1024; }
        http { include /etc/nginx/sites-enabled/*; }
    - mode: "0644"
    - user: root
    - require:
        - pkg: nginx_pkg

nginx_site:
  file.managed:
    - name: /etc/nginx/sites-enabled/app
    - contents: |
        server {
          listen 80;
          root /srv/www/app;
        }
    - require:
        - pkg: nginx_pkg
        - file: site_root

nginx_svc:
  service.running:
    - name: nginx
    - enable: true
    - require:
        - pkg: nginx_pkg
    - watch:
        - file: nginx_conf

open_firewall:
  cmd.run:
    - name: ufw allow 80/tcp
    - require:
        - service: nginx_svc
{%- if not (grains['os_family'] == 'Debian') %}
    - unless: exit 0
{%- endif %}

reload nginx_svc:
  service.running:
    - name: nginx
    - enable: true
    - reload: true
    - require:
        - service: nginx_svc
    - watch:
        - file: nginx_site
```

and `states/top.sls` assigns it to its host:

```yaml
#!yaml
# Generated by Meridian. Do not edit; this file is compiled from a Meridian document.

base:
  web01.example.com:
    - match: list
    - web
```

- One state per resource, under the resource's ID.
- `dependsOn` became `require`; the file is in apply order.
- The restart is a `watch` on the service.
- The reload is a separate state, `reload nginx_svc`, watching only `nginx_site`.
- `runtimeWhen` became a Jinja test.
- `match: list` makes the top file match the one minion ID.

## What a document compiles to

| File | What it is |
|---|---|
| `states/<document>.sls` | The state file: one state per resource, in apply order, then the states that notifications need |
| `states/top.sls` | The top file, which assigns the state file to the document's host and no other |

`states/` is a Salt file root. The state is applied by the document's name:
`state.apply web` for a document named `web`.

- **The document name is the state file name**: letters, digits, `_` and `-`, not `top`.
  Others are refused.
- **Each resource's `id` is its state ID.** `include`, `extend` and `exclude` are refused.
- **The first line says how Salt must read the file.** `#!yaml` for plain YAML,
  `#!jinja|yaml` when a resource has a `runtimeWhen`. See
  [Runtime conditions](#runtime-conditions).
- **Arguments are in a fixed order**: `name`, the rest alphabetically, then `require`,
  `watch` and `onchanges`.
- **The host must be usable as a minion ID**: not empty, and without spaces or commas.

## How resources map

Every shared type, state and param accepted. Any other type is a
[Salt state function](#state-functions-beside-the-six). Any other param is refused.

### `package`

| `state` | Compiles to | Params |
|---|---|---|
| `present` (default) | `pkg.installed` | `name` (required), `version` |
| `latest` | `pkg.latest` | `name` |
| `absent` | `pkg.removed` | `name` |

`version` is refused with `latest` and `absent`.

### `packageRepository`

| `state` | Compiles to | Params |
|---|---|---|
| `present` (default) | `pkgrepo.managed` | `name`, `format`, `url` (required), `key`; apt: `distribution`, `components` |
| `absent` | `pkgrepo.absent` | the same |

apt: `name` is the `deb` line, written to `/etc/apt/sources.list.d/<name>.list`; `key` is `key_url`.

### `service`

| `state` | Compiles to | Params |
|---|---|---|
| `running` (default), `started` | `service.running` | `name` (required), `enabled` → `enable` |
| `stopped` | `service.dead` | `name`, `enabled` → `enable` |

A service can be notified with `restart`, `reload`, `start` and `stop`; see
[Notifications](#notifications).

### `file`

| `state` | Compiles to | Params |
|---|---|---|
| `present` (default), `file` | `file.managed` | `path` → `name` (required), `content` → `contents`, `owner` → `user`, `group`, `mode` |
| `directory` | `file.directory`, with `makedirs: true` | `path`, `owner` → `user`, `group`, `mode` |
| `link` | `file.symlink` | `path` → `name`, `target`, `owner` → `user`, `group` |
| `absent` | `file.absent` | `path` |

- Without `content`, `replace: false` is added: an existing file's content is kept and a
  missing file is created empty.
- Content without a final newline gets one.
- A directory is created with missing parents.
- Quote `mode`: `"0644"`.
- `content` is refused with `directory` and `absent`. `owner`, `group` and `mode` on an
  absent file are dropped with a warning.

### `user` and `group`

| Type | `state` | Compiles to | Params |
|---|---|---|---|
| `user` | `present` (default) | `user.present` | `name` (required), `uid`, `groups`, `home`, `shell`, `system` |
| `user` | `absent` | `user.absent` | `name` |
| `group` | `present` (default) | `group.present` | `name` (required), `gid`, `system` |
| `group` | `absent` | `group.absent` | `name` |

`groups` is the complete supplementary group list; Salt removes the user from others.
Params on an absent user or group are dropped with a warning.

### `exec`

Compiles to `cmd.run`. Takes no `state`.

| Param | Becomes | Meaning |
|---|---|---|
| `command` (required) | `name` | The command line |
| `chdir` | `cwd` | The directory to run it in |
| `creates` | `creates` | Skip the command when this path exists |
| `removes` | an `onlyif` that asks Salt's `file.access` | Run the command only when this path exists |
| `shell` | nothing | Says the command is meant for a shell |

Without `creates` or `removes` it runs on every apply and every plan shows a change.

Salt always uses a shell. A command holding any of `| & ; < > ( ) $ ` \ * ? [ ] { } ~ #`
is refused unless the resource sets `shell: true`:

```yaml
- id: rotate
  type: exec
  params:
    command: gzip /var/log/app/*.log
    shell: true
```

## State functions beside the six

Any state function is a resource type, written by its dotted name as a state file
writes it: `pkgrepo.managed`, `file.symlink`, `docker_container.running`,
`postgres_database.present`. The function's arguments are the resource's `params`,
written into the state as given. Meridian does not check them. Salt refuses an
unknown argument, or a module the minion does not ship, when the state is applied.
Once the module's schema is synced, the editor reports these in place.

A Salt state is always dotted, so `native/<name>` reaches nothing here.
`native/file` is refused, with `file.managed` as what to write. The shared `file`
and `file.managed` both compile to Salt's `file.managed`:

- the shared `file` takes the six's params and moves between tools
- `file.managed` takes every argument the function has and compiles for Salt only

```yaml
resources:
  - id: nginx_repo
    type: pkgrepo.managed
    params:
      name: deb http://nginx.org/packages/debian bookworm nginx
      file: /etc/apt/sources.list.d/nginx.list
      key_url: https://nginx.org/keys/nginx_signing.key
  - id: nginx_pkg
    type: package
    params:
      name: nginx
    dependsOn:
      - resource: nginx_repo
  - id: app_conf
    type: file
    params:
      path: /etc/app/app.conf
      content: "workers = 4\n"
    notifies:
      - resource: app
        action: watch
  - id: app
    type: docker_container.running
    params:
      image: app:1.2
```

compiles to

```yaml
nginx_repo:
  pkgrepo.managed:
    - name: deb http://nginx.org/packages/debian bookworm nginx
    - file: /etc/apt/sources.list.d/nginx.list
    - key_url: https://nginx.org/keys/nginx_signing.key

nginx_pkg:
  pkg.installed:
    - name: nginx
    - require:
        - pkgrepo: nginx_repo

app_conf:
  file.managed:
    - name: /etc/app/app.conf
    - contents: |
        workers = 4

app:
  docker_container.running:
    - image: app:1.2
    - watch:
        - file: app_conf
```

| | |
|---|---|
| `type` | `module.function`: two lower-case words of letters, digits and underscores, joined by one dot. A bare word, a wrong case, three parts, or `native/<name>` is refused with what to write |
| `params` | the function's arguments, as written, `name` first when there is one. A state with no `name` is named by its `id`, as Salt reads it |
| `state` | none: the function is the state. Any `state` is refused (`unsupported-state`) |
| `notifies` to a state function | action `watch`: the state watches the notifier, and what the function does when something it watches changes is the function's own business (a running service restarts, a managed file is written again). Any other action is refused (`unsupported-action`). When the notifier is applied after the state, the same state is declared once more after both, as `watch <id>`, as a service's restart is |
| `notifies` from a state function | as from any resource |
| `runtimeWhen` | a Jinja expression, as for the six |
| Secrets | as for the six: a pillar lookup the runner answers. A `file.*` state given one shows no changes; a `cmd.*` state is refused one, because Salt prints the command line |
| References | `${id.param}` is settled from the document, as for the six |

Example: [`examples/salt/native-states.yaml`](../../examples/salt/native-states.yaml). A
document using a state function compiles for Salt only; the
[`native-type`](../reference/lint.md#native-type) lint marks it. See
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering

States are written in apply order and every `dependsOn` becomes a `require`. States that
require a failed state do not run.

## Notifications

`notifies` compiles to Salt requisites:

| Notified type | Action | Compiles to |
|---|---|---|
| `service` | `restart` | `watch` on the service's own `service.running` state |
| `service` | `reload` | a state `reload <id>`: `service.running` with `reload: true` and a `watch` |
| `service` | `start` | a state `start <id>`: `service.running` with `onchanges` |
| `service` | `stop` | a state `stop <id>`: `service.dead` with `onchanges` |
| `exec` | `run` | a state `run <id>`: the same `cmd.run` with `onchanges` |
| a state function of Salt's own | `watch` | `watch` on the state itself, or a state `watch <id>` after both when the notifier comes later |

Extra states come after all resource states, in name order. Several notifiers share one
state.

- A `restart` happens where the service state is evaluated. If the notifier is applied
  after the service, a `restart <id>` state is written after both.
- A `stopped` service cannot be restarted or reloaded; refused.
- A notified `exec` runs in its place and again, as `run <id>`, on a change, keeping its
  `creates` and `removes`.
- Reactions ignore `runtimeWhen`.

`package`, `file`, `user` and `group` cannot be notified.

## Runtime conditions

`runtimeWhen` is a **Jinja expression**, without `{{ }}` or `{% %}`, over `grains`,
`pillar`, `salt` and `opts`.

```yaml
- id: tune_kernel
  type: exec
  runtimeWhen: grains['os_family'] == 'RedHat' and grains['osmajorrelease'] >= 8
  params:
    command: /usr/sbin/tuned-adm profile throughput-performance
    creates: /etc/tuned/active_profile
```

The minion renders it each apply. When false, the state gets an always-true `unless`, so
Salt skips it as succeeded and dependents still run.

A file with a runtime condition starts with `#!jinja|yaml`, and every authored string is
escaped so Jinja passes it unchanged. Otherwise it starts with `#!yaml`.

The plugin checks the expression is one line without tag delimiters. Salt's errors appear
in `meridian plan` as "salt-call could not compile the state file".

## Running it

`plan` runs with `test=True`; `apply` without. Both show one line per state:

```
nginx_pkg (pkg.installed): would change: The following packages would be installed/updated: nginx
nginx_conf (file.managed): would change: The file /etc/nginx/nginx.conf is set to be changed
legacy_user (user.absent): ok: User old is not present
```

How the result is read:

- Results are read from Salt's JSON at the end of the run; stderr is relayed live.
- A change is `delete` for `file.absent`, `user.absent`, `group.absent` and `pkg.removed`,
  and `update` otherwise.
- A state skipped by a false `runtimeWhen`, by `creates` or `removes`, or because
  nothing it reacts to changed, is reported as `ok` and is not a change.
- If any state fails, the run fails. See [When a run fails](#when-a-run-fails).

### Where it runs

| The document's host | Transport | Command |
|---|---|---|
| `localhost`, `127.0.0.1`, `::1` | local | `salt-call --local --file-root <work>/states --out json state.apply <document>` |
| anything else | SSH | `salt-ssh --config-dir <work>/ssh --roster-file <work>/ssh/roster --out json <host> state.apply <document>` |

`<work>` is the document's directory under the state directory. For SSH it holds a
`salt-ssh` configuration and a one-host roster.

### Settings

The plugin reads four environment variables:

| Variable | Meaning | Default |
|---|---|---|
| `MERIDIAN_SALT_CALL` | The `salt-call` program | `salt-call` on `PATH` |
| `MERIDIAN_SALT_SSH` | The `salt-ssh` program | `salt-ssh` on `PATH` |
| `MERIDIAN_SALT_TRANSPORT` | `local` or `ssh`, to override the choice made from the host | chosen from the host |
| `MERIDIAN_SALT_ARGS` | Extra arguments for every run, split like a shell command line (quotes hold a word together) | none |
| `MERIDIAN_SSH_AGENT` | The `ssh-agent` program, started for a credential's key; see [Credentials](#credentials) | `ssh-agent` on `PATH` |
| `MERIDIAN_SSH_ADD` | The `ssh-add` program | `ssh-add` on `PATH` |

Also settable in `meridian.yaml` under `spec.settings.salt`; the environment wins. See
[plugin settings](../reference/plugin-settings.md#salt).

Common uses:

```
# Meridian runs on the node it configures, whatever the document calls it.
export MERIDIAN_SALT_TRANSPORT=local

# Sign in over SSH as a deploy user with a key, and become root there.
export MERIDIAN_SALT_ARGS="--user deploy --priv /home/me/.ssh/deploy --sudo -i"

# Run salt-call without root, with a configuration directory of your own.
export MERIDIAN_SALT_ARGS="--config-dir /home/me/salt-config"
```

`salt-call` normally runs as root. To run unprivileged, point `--config-dir` at a
directory whose `minion` file sets a writable `root_dir`.

### Secrets

A [secret](../reference/dsl/secrets.md) reaches Salt as pillar data only.

```yaml
spec:
  target: salt
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_conf
      type: file
      params:
        path: /etc/app/db.conf
        mode: "0600"
        content: ${{ secret.db_password }}
```

**Compile.** The file becomes `#!jinja|yaml` and the param reads the pillar key through
`yaml_encode`, giving one quoted YAML scalar. A file state with a secret gets
`show_changes: false`:

```yaml
db_conf:
  file.managed:
    - name: /etc/app/db.conf
    - contents: {{ pillar['meridian_secret_db_password'] | yaml_encode }}
    - mode: "0600"
    - show_changes: false
```

Other strings are escaped for Jinja. The file holds the key, never the value.

**Run.** The runner writes a private pillar root (`0700`, files `0600`, in memory where
possible) with `meridian.sls` and a `top.sls`. `salt-call` gets `--pillar-root <dir>`;
`salt-ssh` gets it as `pillar_roots`. It is removed when the run ends. The value is never on
the command line, in the environment or in the work directory.

**Refused.** A secret in an `exec` (`unsupported-secret-place`), because Salt prints the
command line in its result. Put it in a file the command reads, or a `runtimeWhen`.

The handover is tested two ways:

- against a stand-in `salt-call` that records the pillar files, their mode, its
  arguments and its environment
- against the real `salt-call` in CI, which applies a file whose content is a
  secret and checks the value appears in the file and nowhere in Salt's output or
  the plugin's

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `-l info`, `-l debug` or `-l trace` to `salt-call` and `salt-ssh`.

## Credentials

`targetHost.credential` names the [credential](../reference/dsl/credentials.md) the host is
reached as. Another machine is reached with `salt-ssh`, which reads how to sign in
from its roster. The plugin writes the credential into the run's roster, and keeps
the roster in a private place when it holds a password:

```yaml
spec:
  target: salt
  targetHost:
    from: inline
    host: web01.example.com
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

- **The user** is the roster's `user:`.
- **A password** is the roster's `passwd:`; the roster is then a private file, removed
  after the run, named by `--roster-file`. Otherwise the roster is in the work directory.
- **A key** is the roster's `priv: agent-forwarding`, which makes `salt-ssh` use the agent
  `SSH_AUTH_SOCK` names.
- Leave `--user`, `--priv` and `--passwd` out of `MERIDIAN_SALT_ARGS`; they override the
  roster.
- **A key** goes into an SSH agent Meridian starts for the stage, loaded through `ssh-add`'s
  stdin; `salt-ssh` gets only `SSH_AUTH_SOCK`. A `passphrase` is answered from a private file.
  See [0028](../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md).
- **`agent: true`** uses your own agent. **`from: ambient`** leaves signing in to the tool.
- **Refused at compile**, rule `unsupported-credential`: a `certificate` (put it
  beside the key in an agent of your own and set `agent: true`); a `domain` (for WinRM
  hosts, which this plugin does not reach); a user name with a space or a line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). Leave the credential out, or name another host.

The plan line names the identity, `web (salt) on web01.example.com as deploy (ssh,
user deploy)`, and never a value. `ssh-agent` and `ssh-add` are run from `PATH`, or from
`MERIDIAN_SSH_AGENT` and `MERIDIAN_SSH_ADD`.

Tested against stand-ins for `salt-ssh`, `ssh-agent` and `ssh-add`, and against the real
`salt-ssh` in CI over SSH with a key delivered only through the agent.

## Editor schemas

`meridian schema sync` asks `salt-call --local` for the arguments of the state functions
used, for editor completion and checks ([tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | each state function the documents use: its arguments from `sys.state_argspec`, those without a default required (`name` never is, since it is the resource's id), each described from the function's docstring through `sys.state_doc` where the docstring documents it. The function's keyword arguments (`**kwargs`) are not listed, so every function stays open to arguments beside the ones it names. A function the installed Salt does not have (a custom state module not yet synced to the minion) is a note and not the end of the sync |
| Listed by name | every other state function `sys.list_state_functions` knows: built in, and from the modules the minion's `file_roots` hold. Salt's own `file`, `pkg`, `service`, `user`, `group` and `cmd` modules are among them; a document reaches them by their dotted names, never under `native/` |
| Pinned | Salt's version, from `salt-call --version` |

It uses `MERIDIAN_SALT_CALL` and `MERIDIAN_SALT_ARGS`. Every function takes the `watch`
action.

## Using the files with a Salt master

Copy the two files into the master's file root, such as `/srv/salt`:

```
meridian compile site.yaml --out build
cp build/web/states/web.sls /srv/salt/
salt 'web01.example.com' state.apply web test=True
```

Merge `top.sls` into an existing top file by hand.

## Warnings

```
web (ResourceSet, target salt on localhost)
  warning: salt: old: param "uid" means nothing for state "absent" and was left out
  compiled 2 files into build/web
```

Warnings:

| Warning | When |
|---|---|
| `param "<name>" means nothing for state "absent" and was left out` | A `file`, `user` or `group` is `absent` and still carries params that describe it, such as `mode` or `uid` |

## What the plugin refuses

A refusal names the resource, rule and line:

```
web (ResourceSet, target salt on localhost)
  FAILED: site.yaml:11:7: salt: resource "rotate" [shell-required]: the command contains '*', which a shell gives a meaning to, and Salt always runs a command through a shell; set param "shell" to true to say that is intended
```

Rules about what you wrote:

| Rule | When |
|---|---|
| `unsupported-resource-type` | The type is not one of the six above and not a dotted state function either: a bare word, a wrong case, or `native/<name>` |
| `unsupported-param` | The type has no such param |
| `param-conflicts-with-state` | The param belongs to another state, such as `content` on a directory or `version` on a package that is `latest` |
| `missing-param` | A required param is absent |
| `empty-param` | A param has no value |
| `unsupported-state` | The state is not in the type's table, or an `exec` or a state function of Salt's own has one |
| `shell-required` | A command holds shell syntax and does not say `shell: true` |
| `unsupported-action` | The type cannot perform the notify action, a stopped service is asked to restart or reload, or a state function of Salt's own is asked for anything but `watch` |
| `invalid-runtime-condition` | `runtimeWhen` is blank, spans lines, or holds a Jinja delimiter |
| `invalid-name` | The document name holds something other than letters, digits, `_` and `-`; Salt reads a dot as a directory |
| `reserved-name` | The document is named `top` |
| `missing-param` | The document has no name |
| `missing-host` | The document has no host |
| `invalid-host` | The host holds a space or a comma and cannot be a minion ID |
| `invalid-name` | A resource's `id` is blank or spans lines |
| `reserved-name` | A resource is named `include`, `extend` or `exclude`, which Salt reads for itself |
| `duplicate-resource` | A resource has the name a notification's state needs, such as `reload nginx_svc` |

Rules that guard Meridian itself. The core checks these things before a plugin sees a
document, so you should never meet them; if you do, it is a bug to report:

| Rule | When |
|---|---|
| `dependency-cycle`, `ordering-violated` | The order the plugin was handed contradicts the document's own dependencies |
| `unknown-dependency`, `unknown-notify-target` | A dependency or notification names a resource that is not in the document |
| `unknown-dependency`, `dependency-cycle` | A compiled state requires or watches a state that is not in the file, or itself |
| `missing-param` | A compiled state has no function, or no `name` (also reported as `missing-param`) |
| `unsupported-runtime-condition`, `unknown-notify-target` | A notification's state carries a condition, or reacts to nothing |

## When a run fails

A failed plan or apply reports the command that ran, its exit status, and what Salt said.

| Message | What happened | What to do |
|---|---|---|
| `could not run salt-call: it is not installed, or not on the PATH…` | Salt is not installed where Meridian runs | Install Salt, or set `MERIDIAN_SALT_CALL` / `MERIDIAN_SALT_SSH` |
| `salt-call reported a failed state` (or `N failed states`) | Salt ran and one or more states failed. Each is listed with Salt's comment. For an apply, what had already changed is listed too | Fix the cause Salt names. A state that says `One or more requisite failed` only failed because one it requires did |
| `salt-call could not compile the state file` | Salt could not render or compile the file. Almost always a `runtimeWhen` that is not a valid Jinja expression | Correct the expression; Salt's message has the line |
| `salt-ssh could not run on the host` | SSH could not connect or sign in, or Python is missing on the host | Check the host, and the sign-in options in `MERIDIAN_SALT_ARGS` |
| `salt-ssh needs a host to connect to, and the document names none` | The SSH transport was forced for a document with no host | Give the document a host, or unset `MERIDIAN_SALT_TRANSPORT` |
| `salt-call failed` | Salt exited without printing a result, for example on an option it does not know | Read Salt's output, which is included; check `MERIDIAN_SALT_ARGS` |
| `salt-call printed something that is not its JSON result` | Something else wrote to Salt's output | Remove options from `MERIDIAN_SALT_ARGS` that change the output format |
| `the artifact has no states/top.sls and one state file beside it…` | The files being run were not compiled by this plugin | Compile the document again |

Messages name `salt-ssh` in place of `salt-call` when that is what ran.

### Reading a failure

Each failed state in Salt's JSON result is a [resource failure](../reference/state.md#resource-failures):

| From the result | Becomes |
|---|---|
| `__id__` | the resource |
| the minion (`local` is the step's host) | the endpoint |
| `comment`, or `changes.stderr` when it is empty | the summary |
| `changes.retcode` | the tool code, `rc=1` |
| `One or more requisite failed` | `dependency-failed` |
| any other failed state | `resource-failed`, or `unreachable`, `auth-failed`, `timed-out` or `reboot-required` when the message says so |

A state file that did not compile is `invalid-config` on no resource; a host
`salt-ssh` could not run on is `unreachable` (or `auth-failed`), with its
`retcode`.

A permission error from `salt-call`, such as being unable to write under `/var/cache/salt`,
means it needs root or a configuration directory of its own; see [Settings](#settings).

## How it is tested

- `plugins/salt/tests/compile.rs` compiles the shared fixtures and this guide's example
  and compares them with golden files in `plugins/salt/testdata`, then checks the mapping
  and every refusal. It also checks that this page shows exactly what the example
  compiles to, and that every rule the code can refuse with is documented here.
- `plugins/salt/tests/run.rs` drives the runner against stand-ins for `salt-call` and
  `salt-ssh` that replay results recorded from Salt.
- `plugins/salt/tests/boundary.rs` starts the real plugin binary and compiles, plans and
  applies across the plugin protocol.
- `plugins/salt/tests/real_salt.rs` uses Salt itself. It dry-runs every golden file, and
  really applies a document to a scratch directory: plan, apply, check the files, plan
  again. It runs when `MERIDIAN_TEST_REAL_SALT_CALL` names a `salt-call`, which the `salt`
  workflow in CI does. The dry run of the golden files covers package and service
  states, so it also needs root, or `MERIDIAN_TEST_REAL_SALT_CALL_ROOT` naming a program
  that runs `salt-call` as root:

  ```
  pip install salt
  MERIDIAN_TEST_REAL_SALT_CALL="$(command -v salt-call)" cargo test -p meridian-target-salt
  ```

## Inspecting one resource

[`meridian inspect`](../reference/cli.md#inspect) runs the one state by its id:
`state.sls_id ID SLS test=True`, with `salt-call` or `salt-ssh` as an apply would.

| Answer | From |
|---|---|
| actual | the state's `comment`, and its `changes` as `wouldChange`. Salt reports no properties in test mode |
| as declared | `result: true` yes, `null` no. `false` fails the command |

A state whose function removes (`absent`, `removed`, `purged`, `dead`) and has nothing to do is
not found.
