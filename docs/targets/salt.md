# Target: `salt`

<img src="../branding/logos/meridian-salt.svg" width="72" alt="Meridian Salt plugin logo">

Compiles a ResourceSet into a [Salt](https://saltproject.io) state file and the top
file that assigns it, and applies it with `salt-call --local`, or with `salt-ssh` for
another machine. Salt still does the work: Meridian writes the state file a person would
have written and runs Salt on it.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)), and any state function beside them: `pkgrepo.managed`, `file.symlink`, `docker_container.running` ([State functions beside the six](#state-functions-beside-the-six)) |
| `dependsOn` | Yes, as `require`. The states are also written in apply order, so the dependency graph survives into the state file intact and nothing is flattened |
| `notifies` | Native: `watch` and `onchanges`. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec, `watch` on a state function of Salt's own |
| `runtimeWhen` | Yes: a Jinja expression over grains and pillar, which the minion evaluates when it renders the file |
| Secrets | Yes: as pillar data, from a pillar root only the user can read. A file state given one shows no diff; an `exec` is refused one, because Salt prints the command line. See [Secrets](#secrets) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | Yes: Salt's dry run, `test=True`. Every state reports whether it would change, so a plan is never "unknown" |
| Apply | Yes |
| Outputs | No. Salt states produce no values for a later stage to read |
| Editor schemas | Yes: `meridian schema sync` asks `salt-call` what each state function takes. See [Editor schemas](#editor-schemas) |
| Compiles to | `states/<document>.sls` and `states/top.sls` |
| Runs with | `salt-call --local` on the node itself, `salt-ssh` for another host |

What it does not do:

- **It does not talk to a Salt master.** Plan and apply are masterless. The compiled
  files work on a master, but you copy them there yourself; see
  [Using the files with a Salt master](#using-the-files-with-a-salt-master).
- **No grains or custom modules are written, and no pillar beyond the document's
  [secrets](#secrets).** A `runtimeWhen` can read the node's grains and pillar; the
  plugin does not create them.
- **A state function of Salt's own is passed through unchecked.** Its arguments are
  written as the document gives them, and an argument the function does not take is
  Salt's to refuse when the state is applied, not Meridian's. See
  [State functions beside the six](#state-functions-beside-the-six).
- **`restarted` and `reloaded` are not service states.** Use a notification.
- **It creates no infrastructure.** It configures a node that already exists, and has
  no resource types for anything else.

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

Salt has to be installed on the machine Meridian runs on. With `host: localhost` the
state is applied to that machine with `salt-call --local`, which needs no master and no
running minion. With any other host it is applied over SSH with `salt-ssh`, which needs
nothing installed on the host but Python.

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

Things to notice:

- **One state per resource, under the resource's own ID.** What Salt reports about
  `nginx_conf` is about the resource you wrote as `nginx_conf`.
- **`dependsOn` became `require`.** The file is in apply order and every dependency is
  also stated, so the result reads the same to Salt as it does to you.
- **The restart is a `watch` on the service itself.** That is how a Salt author writes
  it: when `nginx_conf` changes, `service.running` restarts nginx.
- **The reload is a second state, `reload nginx_svc`.** One Salt state either restarts or
  reloads on a change, never both, so the reload has a state of its own that watches only
  `nginx_site`.
- **`runtimeWhen` became a Jinja test.** The first line of the file says it is rendered
  through Jinja, and on a minion that is not in the Debian family `open_firewall` is
  skipped.
- **The top file matches the host exactly.** A bare key in a top file is a glob;
  `match: list` makes it the one minion ID and nothing else.

## What a document compiles to

| File | What it is |
|---|---|
| `states/<document>.sls` | The state file: one state per resource, in apply order, then the states that notifications need |
| `states/top.sls` | The top file, which assigns the state file to the document's host and no other |

`states/` is a Salt file root. The state is applied by the document's name:
`state.apply web` for a document named `web`.

- **The document name names the state file**, so it may hold only letters, digits, `_`
  and `-`. Salt reads a dot in a state name as a directory, and `top` is the top file.
  A name that cannot be used is refused, not rewritten.
- **Each resource's `id` is its state's ID.** Salt's reports, and requisites between
  states, use it as written. The IDs `include`, `extend` and `exclude` mean something
  else at the top of a state file and are refused.
- **The first line says how Salt must read the file.** `#!yaml` for plain YAML,
  `#!jinja|yaml` when a resource has a `runtimeWhen`. See
  [Runtime conditions](#runtime-conditions).
- **Arguments are in a fixed order**: `name`, then the rest alphabetically, then
  `require`, `watch` and `onchanges`. The same document always compiles to the same
  bytes.
- **The host must be usable as a minion ID**: not empty, and without spaces or commas.

## How resources map

Every shared type, state and param the plugin accepts is listed here; any other type is
a state function of Salt's own, [below](#state-functions-beside-the-six). A param a
shared type does not take is refused at compile time with a message that names what
would have worked; nothing is dropped silently.

### `package`

| `state` | Compiles to | Params |
|---|---|---|
| `present` (default) | `pkg.installed` | `name` (required), `version` |
| `latest` | `pkg.latest` | `name` |
| `absent` | `pkg.removed` | `name` |

`version` pins the package and is only meaningful when it is installed, so it is refused
with `latest` and `absent`.

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
| `absent` | `file.absent` | `path` |

- Without `content`, only the file's existence, owner, group and mode are managed. The
  plugin adds `replace: false` so Salt never touches what an existing file holds; a file
  that is missing is created empty.
- Salt ends a managed file with a newline. Content that already ends with one is written
  as it is; content that does not gets one added.
- A directory is created together with any missing parents.
- Write `mode` as a quoted string, `"0644"`. An unquoted `0644` is a YAML number.
- `content` is refused with `directory` and `absent`. `owner`, `group` and `mode` on an
  absent file are left out with a warning, so a resource can be switched to `absent`
  without first being stripped.

### `user` and `group`

| Type | `state` | Compiles to | Params |
|---|---|---|---|
| `user` | `present` (default) | `user.present` | `name` (required), `uid`, `groups`, `home`, `shell`, `system` |
| `user` | `absent` | `user.absent` | `name` |
| `group` | `present` (default) | `group.present` | `name` (required), `gid`, `system` |
| `group` | `absent` | `group.absent` | `name` |

`groups` is the complete list of the user's supplementary groups: Salt removes the user
from any group that is not in it. Params that describe an absent user or group are left
out with a warning.

### `exec`

`exec` is the escape hatch: a command to run, not a state to converge on. It takes no
`state` and compiles to `cmd.run`.

| Param | Becomes | Meaning |
|---|---|---|
| `command` (required) | `name` | The command line |
| `chdir` | `cwd` | The directory to run it in |
| `creates` | `creates` | Skip the command when this path exists |
| `removes` | an `onlyif` that asks Salt's `file.access` | Run the command only when this path exists |
| `shell` | nothing | Says the command is meant for a shell |

A command with neither `creates` nor `removes` runs on every apply, and every plan
reports it as a change.

Salt always hands a command to a shell. Other targets do not unless `shell: true` is
set, so the same document could mean two things. To keep it meaning one, a command that
a shell would read differently, one holding any of `| & ; < > ( ) $ ` \ * ? [ ] { } ~ #`,
is refused unless the resource says `shell: true`:

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

[`examples/salt/native-states.yaml`](../../examples/salt/native-states.yaml) is a whole
document of both kinds. A document that names a state function compiles for Salt only.
The [`native-type`](../reference/lint.md#native-type) lint rule marks each such
resource. The six shared types stay the portable way to say what they can say; a state
function is for everything else. The split, and why `native/file` is refused here, is
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering

Salt evaluates states in the order the file lists them and honours requisites on top.
The plugin writes the states in Meridian's apply order and turns every `dependsOn` into
a `require`, so the dependency graph survives edge for edge. If a state fails, the states
that require it are not run, and Salt says so.

## Notifications

`notifies` compiles to Salt's own requisites. Nothing is shelled out.

| Notified type | Action | Compiles to |
|---|---|---|
| `service` | `restart` | `watch` on the service's own `service.running` state |
| `service` | `reload` | a state `reload <id>`: `service.running` with `reload: true` and a `watch` |
| `service` | `start` | a state `start <id>`: `service.running` with `onchanges` |
| `service` | `stop` | a state `stop <id>`: `service.dead` with `onchanges` |
| `exec` | `run` | a state `run <id>`: the same `cmd.run` with `onchanges` |
| a state function of Salt's own | `watch` | `watch` on the state itself, or a state `watch <id>` after both when the notifier comes later |

The extra states are written after every resource's own state, in name order. Each
requires the resource it acts on and is tied to every resource that notifies it, so
several notifiers of one change share one state.

- **A watched service reacts in place.** A `restart` happens where the service's state
  is evaluated, not at the end of the run. If the resource that notifies is applied
  *after* the service, a watch on the service would turn the order round, so the plugin
  writes a `restart <id>` state after both instead.
- **A stopped service cannot be restarted or reloaded.** Salt does both through the state
  that keeps a service running, so the plugin refuses the combination.
- **A notified `exec` runs in its own place and again on a change.** The `exec` resource
  is an ordinary command in the run. The `run <id>` state is the same command, run once
  more when a notifier changed. It keeps the resource's `creates` and `removes` guards.
- **Reactions ignore `runtimeWhen`.** A `run <id>` or `reload <id>` state runs because
  something changed, not because the notified resource's condition holds.

`package`, `file`, `user` and `group` cannot be notified.

## Runtime conditions

`runtimeWhen` is a condition evaluated on the node at the moment of apply. On this target
it is a **Jinja expression**, written without `{{ }}` or `{% %}`, over what the minion
knows about itself: `grains`, `pillar`, `salt`, `opts`.

```yaml
- id: tune_kernel
  type: exec
  runtimeWhen: grains['os_family'] == 'RedHat' and grains['osmajorrelease'] >= 8
  params:
    command: /usr/sbin/tuned-adm profile throughput-performance
    creates: /etc/tuned/active_profile
```

A minion renders a state file itself, with its own grains and pillar, each time the state
is applied. The expression is written into the file and the minion decides.

When the condition is false the state is not removed from the file. It is given an
`unless` that always holds, so Salt skips it and counts it as succeeded. That is what
lets another resource depend on a conditional one: its `require` still resolves, and it
still runs.

A file with a runtime condition starts with `#!jinja|yaml`. In such a file every string
you wrote is written so that Jinja passes it on unchanged: a `{{`, `{%` or `{#` in file
content reaches the node as those characters. A file with no runtime condition starts
with `#!yaml` and is not rendered through Jinja at all.

The expression is yours: the plugin checks that it is one line and holds no tag
delimiters, and Salt checks the rest when it renders the file. A mistake in it is
reported by `meridian plan` as "salt-call could not compile the state file", with Salt's
own message.

`runtimeWhen` is target-native by design, so a document that uses it is written for one
target. Prefer an ordinary `when`, which Meridian evaluates itself, wherever the answer
is known before apply.

## Running it

`meridian plan` runs Salt's own dry run, `test=True`, and reports each state that would
change. `meridian apply` runs the same command without it. Both show one line per state
as Salt reported it:

```
nginx_pkg (pkg.installed): would change: The following packages would be installed/updated: nginx
nginx_conf (file.managed): would change: The file /etc/nginx/nginx.conf is set to be changed
legacy_user (user.absent): ok: User old is not present
```

How the result is read:

- The plugin asks Salt for its result as JSON and reads every state from it, in the
  order Salt ran them. Salt prints that result once, at the end, so the lines above
  appear when the run finishes. Anything Salt writes to its error stream is relayed as
  it happens.
- **A plan** counts a state as a change when Salt says it would change. **An apply**
  counts a state as a change when Salt reports it changed something.
- A change is reported as `delete` for `file.absent`, `user.absent`, `group.absent` and
  `pkg.removed`, and as `update` for everything else. Salt does not say whether a
  state created something or altered it.
- A state skipped by a false `runtimeWhen`, by `creates` or `removes`, or because
  nothing it reacts to changed, is reported as `ok` and is not a change.
- If any state fails, the run fails. See [When a run fails](#when-a-run-fails).

### Where it runs

| The document's host | Transport | Command |
|---|---|---|
| `localhost`, `127.0.0.1`, `::1` | local | `salt-call --local --file-root <work>/states --out json state.apply <document>` |
| anything else | SSH | `salt-ssh --config-dir <work>/ssh --roster-file <work>/ssh/roster --out json <host> state.apply <document>` |

`<work>` is the directory Meridian keeps for the document under its state directory.
For SSH the plugin writes two small files there: a configuration that points `salt-ssh`
at the compiled states and keeps its cache inside the work directory, and a roster with
the one host.

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

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.salt`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#salt).

Common uses:

```
# Meridian runs on the node it configures, whatever the document calls it.
export MERIDIAN_SALT_TRANSPORT=local

# Sign in over SSH as a deploy user with a key, and become root there.
export MERIDIAN_SALT_ARGS="--user deploy --priv /home/me/.ssh/deploy --sudo -i"

# Run salt-call without root, with a configuration directory of your own.
export MERIDIAN_SALT_ARGS="--config-dir /home/me/salt-config"
```

`salt-call` normally runs as root, because it writes to `/var/cache/salt` and most
states change the system. To run it unprivileged, point `--config-dir` at a directory
whose `minion` file sets `root_dir` to somewhere you can write.

### Secrets

A [secret](../reference/dsl/secrets.md) in a resource's params reaches Salt as pillar
data, and through nothing else.

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

**What the compile writes.** The state file is rendered through Jinja (its first line
becomes `#!jinja|yaml`, as it is for a `runtimeWhen`), and the param is a read of the
secret's pillar key. The value goes through Salt's `yaml_encode` filter, so whatever it
holds, what Jinja writes in its place is one quoted YAML scalar, and the reference is
written unquoted where YAML reads that scalar. A file state given a secret is told not
to show its diff, since the diff would print the value:

```yaml
db_conf:
  file.managed:
    - name: /etc/app/db.conf
    - contents: {{ pillar['meridian_secret_db_password'] | yaml_encode }}
    - mode: "0600"
    - show_changes: false
```

Every other string in the file is still escaped so that Jinja leaves it alone, as it is
for a condition; see [Runtime conditions](#runtime-conditions). The compiled file holds
the key, never the value.

**How the value reaches the tool.** When a stage runs, the runner writes a pillar root in
a directory only the user can enter (mode `0700`, the files `0600`, on a memory filesystem
where the system has one): `meridian.sls` with each secret under its key, and a `top.sls`
that applies it to every minion. On this machine `salt-call` is given `--pillar-root
<dir>`; for another host the `salt-ssh` configuration the plugin writes names it as
`pillar_roots`, and `salt-ssh` sends the pillar to the node the way it sends every
pillar. The directory is overwritten and removed when the run ends. Neither the command
line nor the tool's environment ever holds the value, and nothing in the stage's work
directory does. The report Meridian reads says the file is set to be changed, as it does
for any file, without the content.

**What is refused.** A secret in an `exec`. Salt writes the command line into its
result's comment, in a dry run (`Command "..." would have been executed`) and in a real
one, and no setting keeps it out, so the value would be in the report Meridian reads. The
compile refuses it, rule `unsupported-secret-place`; put the secret in a file the command
reads, or use it in a `runtimeWhen`.

The handover is tested two ways:

- against a stand-in `salt-call` that records the pillar files, their mode, its
  arguments and its environment
- against the real `salt-call` in CI, which applies a file whose content is a
  secret and checks the value appears in the file and nowhere in Salt's output or
  the plugin's

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
- **A password** (an `ssh` credential with a `password`, or a `usernamePassword` one) is the
  roster's `passwd:`. The roster is then written to a file only you can read, in a
  directory of its own that is removed when the run ends, and `--roster-file` names it; the
  work directory's `ssh/roster` is not written. Without a password the roster is in the
  work directory, as before, and holds the user and `priv: agent-forwarding` at most.
- **A key** is the roster's `priv: agent-forwarding`, which makes `salt-ssh` use the agent
  `SSH_AUTH_SOCK` names.
- Leave `--user`, `--priv` and `--passwd` out of `MERIDIAN_SALT_ARGS` when the document
  names a credential: `salt-ssh` lets a command-line option override the roster.
- **A key is never on disk.** It goes into an SSH agent Meridian starts for the stage:
  `ssh-agent -D -a <socket>`, its socket in a directory only you can enter, loaded with
  `ssh-add -` from standard input. `salt-ssh` is told the socket in `SSH_AUTH_SOCK` and
  nothing else. A `passphrase` is answered to `ssh-add` from a file only you can read. The
  agent is stopped and its directory removed when the stage ends. Nothing of the key is on
  the command line, in the environment, in the compiled files or in a file Meridian keeps.
- **`agent: true`** names a key in your own agent: `salt-ssh` reads `SSH_AUTH_SOCK` as it is,
  and Meridian starts nothing. **`from: ambient`** leaves signing in to the tool's own
  configuration, as with no credential at all.
- **Refused at compile**, rule `unsupported-credential`: a `certificate` (put it
  beside the key in an agent of your own and set `agent: true`); a `domain` (for WinRM
  hosts, which this plugin does not reach); a user name with a space or a line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). `salt-call` configures this machine as the user running Meridian, whoever the document names. Leave the credential out, or name another host.

The plan line names the identity, `web (salt) on web01.example.com as deploy (ssh,
user deploy)`, and never a value. `ssh-agent` and `ssh-add` are run from `PATH`, or from
`MERIDIAN_SSH_AGENT` and `MERIDIAN_SSH_ADD`.

The handover is tested against stand-ins for `salt-ssh`, `ssh-agent` and `ssh-add` that keep what they are given (the roster and its mode, the environment, the key and the passphrase), and against the real `salt-ssh` in CI, which configures the machine the tests run on over SSH as a credential whose key is read into a secret and reaches `salt-ssh` only through the agent Meridian starts.

## Editor schemas

`meridian schema sync` asks `salt-call --local` what the state functions the
documents use take, so an editor completes and checks a `file.symlink` or a
`pkgrepo.managed`'s arguments as it does the six types' params
([Tool schemas](../tooling/schema-sync.md)). Until a function is synced, the
editor knows it by name only, as [State functions beside the six](#state-functions-beside-the-six)
says.

| | |
|---|---|
| Described in full | each state function the documents use: its arguments from `sys.state_argspec`, those without a default required (`name` never is, since it is the resource's id), each described from the function's docstring through `sys.state_doc` where the docstring documents it. The function's keyword arguments (`**kwargs`) are not listed, so every function stays open to arguments beside the ones it names. A function the installed Salt does not have (a custom state module not yet synced to the minion) is a note and not the end of the sync |
| Listed by name | every other state function `sys.list_state_functions` knows: built in, and from the modules the minion's `file_roots` hold. Salt's own `file`, `pkg`, `service`, `user`, `group` and `cmd` modules are among them; a document reaches them by their dotted names, never under `native/` |
| Pinned | Salt's version, from `salt-call --version` |

Every call runs `salt-call --local --out=json` with the target's own
settings (`MERIDIAN_SALT_CALL`, `MERIDIAN_SALT_ARGS`), so the minion
configuration the runs use is the one the sync reads. Every function takes
the `watch` action, which is what `notifies` sends it.

## Using the files with a Salt master

The compiled `states/` directory is an ordinary Salt file root, so it also works where a
master pushes to minions. Copy the two files into the master's file root, for example
`/srv/salt`, and apply as usual:

```
meridian compile site.yaml --out build
cp build/web/states/web.sls /srv/salt/
salt 'web01.example.com' state.apply web test=True
```

`top.sls` assigns the state to the document's host for a highstate. If the master already
has a top file, merge the entry into it by hand; do not overwrite it. `meridian plan`
and `meridian apply` do not talk to a master.

## Warnings

A warning does not stop a compile; it is printed beside the result:

```
web (ResourceSet, target salt on localhost)
  warning: salt: old: param "uid" means nothing for state "absent" and was left out
  compiled 2 files into build/web
```

This target has one:

| Warning | When |
|---|---|
| `param "<name>" means nothing for state "absent" and was left out` | A `file`, `user` or `group` is `absent` and still carries params that describe it, such as `mode` or `uid` |

## What the plugin refuses

A refusal stops the compile. It names the resource, the rule and the line in your
document, for example:

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
