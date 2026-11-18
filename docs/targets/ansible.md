# Target: `ansible`

<img src="../branding/logos/meridian-ansible.svg" width="72" alt="Meridian Ansible plugin logo">

Compiles a ResourceSet into a playbook and an inventory, and runs them with
`ansible-playbook` from the machine Meridian runs on. Ansible still does the
work: Meridian writes the play a person would have written and runs Ansible
on it.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)), and any module by its full name, such as `ansible.builtin.lineinfile` ([modules beside the six](#modules-beside-the-six)) |
| `dependsOn` | Yes, as task order. A play runs top to bottom, so independent resources still run in sequence; a warning says which |
| `notifies` | Native: handlers. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec or on a module. The action is kept: a reload stays a reload |
| `runtimeWhen` | Yes: a task's `when:`, as an Ansible expression |
| Secrets | Yes: by an extra-vars file only the user can read, with `no_log` on the task |
| Credentials | Yes: an `ssh` or `usernamePassword` credential for the host, the user in the inventory and the key or password by a private file. See [Reaching the host as a credential](#reaching-the-host-as-a-credential) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | Yes: `--check --diff`. A task Ansible skips in check mode cannot be previewed, and makes the plan unknown |
| Apply | Yes |
| Outputs | No. A play produces no values for a later stage to read |
| Import | Yes: playbooks and task files. See [Importing](#importing) |
| Editor schemas | Yes: `meridian schema sync` asks `ansible-doc` what each module takes. See [Editor schemas](#editor-schemas) |
| Compiles to | `playbook.yml` and `inventory.ini` |
| Runs with | `ansible-playbook`, over whatever connection Ansible is configured for |

What it does not do:

- **No roles, no vault.** A play is one list of tasks, each a module call.
  What the document says is all the play says. A module from an installed
  collection is called by its full name; the collection itself is not
  installed by Meridian.
- **No short module names.** A module is written in full,
  `ansible.builtin.lineinfile`, never `lineinfile`. The six shared types are
  the only bare words.
- **No inventory beyond the host.** `inventory.ini` holds the document's one
  host. Groups, host variables and connection settings go through
  [`MERIDIAN_ANSIBLE_ARGS`](#settings) or Ansible's own configuration.
- **`package.version` is refused**, though Ansible could express it; see
  [What is refused](#what-is-refused).
- **It creates no infrastructure.** It configures a host that already exists.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resource types](#resource-types) ·
[Modules beside the six](#modules-beside-the-six) ·
[Ordering and notifications](#ordering-and-notifications) ·
[What a compile writes](#what-a-compile-writes) ·
[Importing](#importing) ·
[Running](#running) ·
[Settings](#settings) ·
[Editor schemas](#editor-schemas) ·
[What is refused](#what-is-refused) ·
[Troubleshooting](#troubleshooting) ·
[How it is tested](#how-it-is-tested)

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | `ansible-playbook` (ansible-core; `pip install ansible-core`) |
| The host | What Ansible needs: SSH and Python, or `--connection local` for this machine. Connection settings go in [`MERIDIAN_ANSIBLE_ARGS`](#settings) |
| To compile only | Nothing. `meridian compile` never runs Ansible |

Tested with ansible-core 2.19.

## A first document

[`examples/ansible/web-server.yaml`](../../examples/ansible/web-server.yaml)
installs nginx, writes its configuration and keeps it running, and is walked
through in [your first ResourceSet](../quickstart/first-resourceset.md):

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01.example.com
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
      # Runs only when the file really changed, once, at the end of the play.
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

It compiles to a play of three tasks and one handler:

```yaml
---
- name: web
  hosts: web01.example.com
  tasks:
    - name: nginx_pkg
      ansible.builtin.package:
        name: nginx
        state: present
    - name: nginx_conf
      ansible.builtin.copy:
        content: |
          server {
            listen 80;
            root /srv/www;
          }
        dest: /etc/nginx/conf.d/site.conf
        mode: "0644"
        owner: root
      notify:
        - reload nginx_svc
    - name: nginx_svc
      ansible.builtin.service:
        enabled: true
        name: nginx
        state: started
  handlers:
    - name: reload nginx_svc
      ansible.builtin.service:
        enabled: true
        name: nginx
        state: reloaded
```

The same document compiles for Chef, Salt or cloud-init by changing `target`,
and for Puppet or OpenVox once the reload is a restart; the
[conformance matrix](../reference/conformance-matrix.md) says where the
targets differ.

## Resource types

The six shared types, as this target takes them. A param not listed is refused
(`unsupported-param`); a state not listed is refused (`unsupported-state`).

| Type | Module | States (`state`) | Params | Required |
|---|---|---|---|---|
| `package` | `ansible.builtin.package` | `present` (default), `absent`, `latest` | `name` | `name` |
| `service` | `ansible.builtin.service` | `running` (`started`, default), `started`, `stopped`, `restarted`, `reloaded` | `name`, `enabled` | `name` |
| `file` | `ansible.builtin.copy` when there is `content`, `ansible.builtin.file` otherwise | `present` (`file`, default), `file`, `directory`, `absent` | `path` (`dest` or `path`), `content`, `owner`, `group`, `mode` | `path` |
| `user` | `ansible.builtin.user` | `present` (default), `absent` | `name`, `uid`, `home`, `shell`, `system`, `groups` | `name` |
| `group` | `ansible.builtin.group` | `present` (default), `absent` | `name`, `gid`, `system` | `name` |
| `exec` | `ansible.builtin.command`, or `ansible.builtin.shell` with `shell: true` | none; an exec has no state | `command` (`cmd`), `chdir`, `creates`, `removes`, `shell` | `command` |

Notes on how they map:

- A resource's **id is the task name**, so Ansible's output names the task
  by the id, and Meridian reads the changes back by it. An id must be one
  line and not empty.
- `exec` with `shell: true` runs through `ansible.builtin.shell`, so a pipe or
  a redirection works. Without it `ansible.builtin.command` runs the command
  as written, so a pipe reaches the program as an argument.
- `exec` runs on every play unless `creates` or `removes` says when it is
  already done. In check mode a command with neither is skipped, which makes
  the plan unknown.
- A `mode` is written as Ansible reads it: quote it in the document
  (`"0644"`), or the YAML reads it as a number and the compile refuses it.

## Modules beside the six

Any module Ansible can find is a resource type, written by its fully
qualified collection name: `ansible.builtin.lineinfile`,
`ansible.builtin.template`, `community.general.ufw`. The module's arguments
are the resource's `params`, passed through to the task exactly as written;
Meridian checks nothing against a list of what the module takes. An argument
the module does not have, or a module that is not installed, is Ansible's to
refuse when the play runs, as a provider's type is Terraform's, until the
module's schema is [synced](#editor-schemas) and the editor reports it in
place.

```yaml
resources:
  - id: worker_count
    type: ansible.builtin.lineinfile
    state: present
    params:
      path: /etc/nginx/nginx.conf
      regexp: "^worker_processes"
      line: worker_processes 4;
    notifies:
      - resource: nginx_svc
        action: reload
  - id: facts
    type: ansible.builtin.setup
```

compiles to

```yaml
    - name: worker_count
      ansible.builtin.lineinfile:
        line: worker_processes 4;
        path: /etc/nginx/nginx.conf
        regexp: ^worker_processes
        state: present
      notify:
        - reload nginx_svc
    - name: facts
      ansible.builtin.setup: {}
```

| | |
|---|---|
| `type` | `namespace.collection.module`: three lower-case words joined by dots, as Ansible requires of a collection name. A bare `lineinfile`, a wrong case, or `native/file` is refused with the full name to write |
| `params` | the module's arguments, as written. A module that takes none, such as `ansible.builtin.setup`, is written with none |
| `state` | written as the module's `state` argument, whatever words the module gives it. Writing both `state:` and `params.state` is refused (`param-conflicts-with-state`) |
| `notifies` to a module | action `run`: the handler is the same task, run again when a notifier changed. Any other action is refused (`unsupported-action`), because what a state means is the module's own business |
| `notifies` from a module | as from any resource |
| `runtimeWhen` | the task's `when:`, as for the six |
| Secrets | as for the six: the variable the runner hands over, and `no_log` on the task |
| References | `${id.param}` is settled from the document, as for the six |

[`examples/ansible/native-modules.yaml`](../../examples/ansible/native-modules.yaml)
is a whole document of both kinds. A document that names a module compiles
for Ansible only, since no other target knows the module. The [`native-type`](../reference/lint.md#native-type)
lint rule marks each such resource. The six shared types stay the portable
way to say what they can say; a module is for everything else. The split, and
why `native/file` is refused here, is
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering and notifications

`dependsOn` becomes the order of the tasks. Ansible runs a play top to bottom,
so the resources are written in the apply order Meridian decided, and
resources that are independent of each other still run one after the other.
The compile warns which resources were independent, so the flattening is
visible.

`notifies` becomes a handler, named after the action and the resource, such
as `reload nginx_svc`. Handlers run once, at the end of the play, only when a
task that notifies them changed something:

| Notified type | Actions | What the handler does |
|---|---|---|
| `service` | `restart`, `reload`, `start`, `stop` | `ansible.builtin.service` with `state: restarted`, `reloaded`, `started` or `stopped` |
| `exec` | `run` | runs the command again |
| a module named in full | `run` | runs the module's task again, as written |
| anything else | none | refused: `unsupported-action` |

`runtimeWhen` is copied into the task's `when:` as written, so it is an
Ansible expression, answered by the host's facts when the play runs:

```yaml
runtimeWhen: "ansible_facts['os_family'] == 'Debian'"
```

A handler never carries a condition: it runs because a task that notified it
changed, and nothing else.

## What a compile writes

```
playbook.yml
inventory.ini
```

`playbook.yml` holds one play with one task per resource, named by the
resource's id, and one handler per distinct notification. `inventory.ini`
holds the document's host on one line, in a group named `meridian`. They are separate so the play can be
run against an inventory of your own.

Ansible renders every task string through Jinja on the control machine.

- A Jinja expression written in the document, such as
  `{{ ansible_facts['os_family'] }}`, is passed through and evaluated when the
  play runs.
- A value Meridian places from `--data`, `--set`, a data layer or `env()` is
  not. The plugin declares Jinja's openings (`{{`, `{%`, `{#`), and the core
  writes each one as an expression that yields itself (`{{ '{{' }}`). A data
  value holding `{{ lookup('pipe', 'id') }}` reaches the host as that text and
  never runs `id` on the control machine
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).

The host in `inventory.ini` is one line, so the plugin refuses a host that is
not a name (`invalid-host`); the core refuses one before it gets this far
([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)).

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) in a
resource's params is compiled as `{{ meridian_secret_<name> }}`, and the task
that holds one is marked `no_log: true`, so Ansible prints neither its
arguments, its diff nor its result. When the stage runs, the values are
written into an extra-vars file only the user can read, held in memory where
the system allows, and passed as `--extra-vars @<path>`; the file is removed
when the run ends. The value is never on the command line and never in
Ansible's environment.

## Importing

`meridian import site.yml` reads playbooks and task files and writes one ResourceSet for
each play. [Importing existing configuration](../operations/importing.md) is the guide. `site.yml`, `playbook.yml` and any name with `playbook` in it
are recognised; for another name say `--from ansible`.

A playbook says far more than the six resource types do, so this importer reports more
than any other. What it carries over is exact. What it does not is listed, not
approximated.

- A task one of the six carries whole becomes that type, so the document still moves to
  other tools.
- Any other task becomes the module itself, by its full name, with every argument as
  written, and the document says it is now written for Ansible.
- A task it does not understand never becomes an `exec`.

| In the playbook | In the document |
|---|---|
| A play | a ResourceSet, named after the play. `hosts` is its host |
| A file that is a list of tasks, such as a role's `tasks/main.yml` | a ResourceSet with no host, marked for you to fill in |
| `pre_tasks`, `tasks`, `post_tasks` | one list of resources, in the order Ansible runs them |
| `apt`, `yum`, `dnf`, `apk`, `zypper`, `pacman`, `package` | `package`. `state` is `present`, `absent` or `latest` |
| `service`, `systemd` | `service`. `started` is `running` |
| `file` | `file`, for the states `file`, `directory` and `absent` |
| `copy` with `content` | `file` with `content` |
| `user`, `group` | `user`, `group`. A comma-separated `groups` becomes a list |
| `command`, `shell` | `exec`, with `shell: true` for `shell`. `chdir`, `creates` and `removes` are carried, from the module's arguments or from `args` |
| Any other module (`template`, `lineinfile`, `git`, `debug`, a collection's own), and one of the above with an argument its resource type has no param for, such as `apt` with `update_cache` | the module itself, as `ansible.builtin.<name>` for a module of ansible-core's own or the full name the playbook wrote, with its arguments as written, `state` included. Marked as changed: the document compiles for Ansible only |
| A task's `name` | the resource's id, as lower-case words joined by underscores. A task with no name is named after what it does, such as `package_nginx` |
| `when` | `runtimeWhen`, unchanged. A list of conditions, and the `when` of an enclosing block, are joined with `and` |
| `notify`, where the handler restarts, reloads, starts or stops a service that a task of the play also manages | `notifies` on that service resource, with the action `restart`, `reload`, `start` or `stop`. A handler is found by its name or by what it `listen`s for |
| `loop` or `with_items` over a list written in the playbook | one resource for each item, with `{{ item }}` written in |
| `block` | its tasks |
| A play variable with a plain value, used as `{{ name }}` | the value, written in where it is used |
| `{{ ansible_facts[...] }}` and any other Jinja expression | kept as written: Ansible still evaluates it when the compiled playbook runs |
| `mode: 0644` | `"0644"`. An unquoted mode is a number to Ansible, and the document writes the digits it means |
| A module addressed as `ansible.builtin.apt` | the same as `apt` |

The files are read by YAML 1.1's rules, as Ansible reads them, so `enabled: yes` is
`true`.

What is reported:

| In the playbook | What happens | Level |
|---|---|---|
| A task whose module has no resource type, or whose arguments its resource type cannot all carry | Imported as the module itself, with every argument. The document compiles for Ansible only | changed |
| A module named without its collection that is not one of ansible-core's own, such as one a role brings | Left out: its full name is not known. Write it as `namespace.collection.<name>` and import again | not translated |
| `raw` or `script` with free-form arguments | Left out: only `exec` carries free-form text. Write the arguments as a mapping | not translated |
| `copy` with `src`, and `file` with `state: link`, `hard` or `touch` | Imported as `ansible.builtin.copy` or `ansible.builtin.file` itself, with the `src` or the state as written | changed |
| `roles`, `include_tasks`, `import_tasks`, `include_role`, `import_role`, `import_playbook` | Left out, naming what was brought in. Import that file as well | not translated |
| A task keyword a resource cannot carry: `become`, `register`, `tags`, `environment`, `ignore_errors`, `changed_when`, `delegate_to` and the rest | The resource is imported without it, and the keyword is named | not translated |
| A play keyword: `become`, `remote_user`, `vars_files`, `serial`, `environment` and the rest | Left out. Connection settings belong in `MERIDIAN_ANSIBLE_ARGS` | not translated |
| A `loop` over a variable or an expression | The task is left out | not translated |
| A play variable that is a list, a mapping or a template, and any play variable used inside an expression such as `{{ port | int + 1 }}` or in a `when` | The resource that uses it is marked: Ansible will find the variable undefined | not translated |
| A `notify` whose handler is not a service action, acts on a service no task manages, or does not exist | The notification is left out, saying which. A handler that calls another module is not turned into a resource notified with `run` | not translated |
| A block's `rescue` and `always` | Left out | not translated |
| `hosts` that is a pattern: `all`, `web:&prod`, `web*`, a list | No host is written, and the document is marked. A document configures one host | not translated |
| `hosts` that is one bare word, such as `webservers` | Taken as a host name, and marked in case it is an inventory group | changed |
| `service` or `systemd` with `enabled` and no `state` | A service resource always has a state, so it is given `running`: applying the document starts the service | changed |
| `gather_facts` | Not carried; the compiled play gathers facts | note |
| A handler nothing imported notifies | Left out | note |

The round-trip test imports
[`web/site.yml`](../../plugins/ansible/testdata/import/web/site.yml), compiles the
document, and compares the playbook it compiles to, by meaning, with
[`web.expected.playbook.yml`](../../plugins/ansible/testdata/import/web.expected.playbook.yml):
the same play written by hand the way the target writes one, with tasks named by id and
modules in full. The imported document is
[`web.meridian.yaml`](../../plugins/ansible/testdata/import/web.meridian.yaml). A second
test imports a playbook full of things a document cannot say; its document and report are
[`odd.meridian.yaml`](../../plugins/ansible/testdata/import/odd.meridian.yaml) and
[`odd.notes`](../../plugins/ansible/testdata/import/odd.notes).

## Running

| | |
|---|---|
| Plan | `ansible-playbook --inventory inventory.ini --check --diff playbook.yml` |
| Apply | `ansible-playbook --inventory inventory.ini playbook.yml` |

Both run in the stage's work directory with the output callback forced to
`default` and colour off, so an `ansible.cfg` of your own cannot reshape the
report Meridian reads. Changes are read from that report, task by task and
handler by handler: a `changed:` line under `TASK [nginx_conf]` is a change to
`nginx_conf`. A looped task reports once.

In check mode a task Ansible cannot preview is skipped: a command with no
`creates` or `removes`, or a condition only the host can answer. A plan with
a skipped task is reported as **unknown**, not as empty, so a run never says
"nothing to do" on no evidence.

A failed task fails the stage, with the end of Ansible's output and the
changes made before it. Ansible's own exit statuses are named: `2` is a
failed task, `4` is a host that could not be reached.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_ANSIBLE_PLAYBOOK` | the program to run, when `ansible-playbook` is not on `PATH` |
| `MERIDIAN_ANSIBLE_ARGS` | arguments added to every run, split like a shell command line (quotes hold a word together): `--user deploy --private-key ~/.ssh/deploy`, or `--connection local` for this machine |
| `MERIDIAN_ANSIBLE_DOC` | the `ansible-doc` program `meridian schema sync` runs, when it is not on `PATH` |
| `MERIDIAN_ANSIBLE_GALAXY` | the `ansible-galaxy` program `meridian schema sync` runs for the collections' versions, when it is not on `PATH` |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.ansible`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#ansible).

## Reaching the host as a credential

The target takes a [credential](../reference/dsl/credentials.md) for its
host: `targetHost.credential` names an `ssh` or `usernamePassword`
credential, and the host is reached as it instead of as whoever
`ansible-playbook` is configured to be.

```yaml
spec:
  secrets:
    - name: deploy_key
      from: file
      path: /run/secrets/deploy_key
      keepTrailingNewline: true
  credentials:
    - name: deploy
      type: ssh
      user: deploy
      privateKey: ${{ secret.deploy_key }}
  targetHost:
    from: inline
    host: web01.internal
    credential: deploy
```

| Field | Where it goes |
|---|---|
| `user` | after the host in `inventory.ini`: `web01.internal ansible_user=deploy`. A name that cannot be written on an inventory line (whitespace, `=`, `[`, `#`) is refused |
| `privateKey` | a file only the user can read, with the line break OpenSSH wants, passed as `--private-key <path>` and removed when the run ends |
| `password` | `ansible_password` in the extra-vars file that carries the document's secrets, beside them. Password authentication over SSH needs `sshpass` where `ansible-playbook` runs, as it does for Ansible on its own |
| `agent: true` | nothing: the key comes from the SSH agent `ansible-playbook` already uses |
| `from: ambient` | the user, if given, and nothing else |

What `ansible-playbook` cannot take is refused when the document is compiled,
with the reason (rule `unsupported-credential`): a `passphrase` on the key
(add the key to an agent and set `agent: true`), a `certificate` beside the
key (put it in the agent too), and a `domain` (for WinRM hosts, which this
plugin does not reach). The credential is never in `playbook.yml`, and the
plan names it: `on web01.internal as deploy (ssh, user deploy)`.

`MERIDIAN_ANSIBLE_ARGS` still applies, so `--user` or `--private-key` given
there and a credential in the document would both reach the command line;
name the identity in one place.

## Editor schemas

`meridian schema sync` asks `ansible-doc` what the modules the documents
name take, so an editor completes and checks `ansible.builtin.lineinfile`'s
arguments as it does the six types' params
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | each module the documents name: every option with its type (`str`, `path`, `bool`, `int`, `float`, `list`, `dict`; `raw` and `json` as anything), whether it is required, its choices, its sub-options, and a `no_log` option as sensitive. Read from `ansible-doc -t module --json <module>`, one run a module, so a module that is not installed is a note and not the end of the sync |
| Listed by name | every other module `ansible-doc --list` finds, with its short description: ansible-core's own and every installed collection's |
| Pinned | ansible-core's version as `ansible.builtin`, and each installed collection's version from `ansible-galaxy collection list`. A collection installed under several paths is pinned at its newest version |

An option's aliases are not listed: a document writes the option's own name,
which is what the play holds. The `state` option is listed as a param; a
document may write it as the resource's `state` instead.

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type that is neither one of the six nor a module's full name: a bare `lineinfile`, `native/file`, the wrong case (`Package`) | use one of the six, or write the module in full: `ansible.builtin.lineinfile` |
| `unsupported-param` | a param one of the six does not take on Ansible, including `package.version` | drop it, or use the module itself |
| `param-conflicts-with-state` | a module given both `state:` and `params.state` | write one of them |
| `missing-param` | no `name`, `path` or `command` | add it |
| `empty-param` | a required param that is empty text | give it a value |
| `unsupported-state` | a state this type does not have, or any state on an `exec` | see the table under [Resource types](#resource-types) |
| `unsupported-action` | a notify action the type does not take: anything on a package, file, user or group; anything but `run` on a module | notify a service or an exec, or run the module again |
| `unknown-notify-target` | a notify names a resource the document does not declare | fix the id |
| `ordering-violated` | the order the core handed over puts a resource before its dependency | a core bug; report it |
| `duplicate-resource` | two resources with the same id | rename one |
| `duplicate-resource` | two tasks or two handlers that would share a name | rename one; a handler is named after its action and resource |
| `invalid-name` | an id that cannot be a task name | use one line of letters, digits and underscores |
| `missing-param`, `empty-param` | a task with no module, or a module given nothing | a plugin bug; report it |
| `unknown-notify-target` | a task notifies a handler the play does not define | a plugin bug; report it |
| `unsupported-runtime-condition` | a `runtimeWhen` on a handler | a plugin bug; report it |
| `invalid-reference` | `${id.param}` names a resource or a param that is not there | fix the reference |
| `invalid-host` | a host that is not a name, so it cannot be an inventory line | the core refuses one first ([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)) |
| `unsupported-credential` | a credential field `ansible-playbook` cannot take: a `passphrase`, a `certificate`, a `domain`, a user that cannot be an inventory line, or a type that is not `ssh` or `usernamePassword` | see [Reaching the host as a credential](#reaching-the-host-as-a-credential) |
| `missing-host` | a ResourceSet with no host to write the inventory for | give it a `targetHost` |

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| `could not run ansible-playbook: it is not installed, or not on the PATH` | the plugin runs `ansible-playbook` on the machine running Meridian | `pip install ansible-core`, or set `MERIDIAN_ANSIBLE_PLAYBOOK` |
| `ansible-playbook could not reach the host` (status `4`) | SSH, the user or the key is not what Ansible needs | name a [credential](#reaching-the-host-as-a-credential) for the host, put the connection settings in `MERIDIAN_ANSIBLE_ARGS`, or use `--connection local` for this machine |
| the plan is unknown | a task was skipped in check mode: an exec with no `creates` or `removes`, a condition only the host can answer, or a module that does not support check mode | give the exec a `creates`, or read the apply's report |
| `ansible-playbook reported a failed task` on a module named in full, with "Unsupported parameters" or "couldn't resolve module" | the module's arguments are passed through as written, and Ansible is the judge of them | fix the argument, or install the collection; `meridian schema sync` lets the editor report it before the run |
| `ansible-playbook reported a failed task` (status `2`) | a task failed on the host; the ones before it were applied | read the output above the message; `apply --resume` continues after the fix |
| a handler did not run | handlers run only when a task that notifies them changed | expected; change the notifier, or declare the state you want on the service |
| a warning that resources are independent but run in sequence | a play is one sequence | expected; it says what the target cannot express |

## How it is tested

- **Golden files** under `plugins/ansible/testdata/` record what every fixture
  compiles to, byte for byte, the warnings it raises, and what each refusal
  says; `native_modules` is the play of modules named in full.
- **Schema tests** (`plugins/ansible/src/schemas.rs`) read saved `ansible-doc`
  output: options, sub-options, choices, secrets.
- **The conformance tests** compile the same documents for every shared-type
  target and generate the [conformance matrix](../reference/conformance-matrix.md),
  which fails when this target starts accepting or refusing something new.
- **Run tests** (`plugins/ansible/tests/run.rs`) drive the runner against a
  stand-in for `ansible-playbook`: the exact command, the report read back
  task by task, and the exit statuses.
- **Import tests** (`plugins/ansible/tests/import.rs`) round-trip the
  playbooks under `testdata/import/`; see [Importing](#importing).
- **Real-tool tests** (`plugins/ansible/tests/real.rs`, workflow
  `ansible.yml`) run ansible-core 2.19 over the `local` connection: a
  check-mode plan that creates nothing, a play checked on disk, a second plan
  that is in sync, drift put right, a handler that runs only when its
  notifier changed, a `runtimeWhen` answered by the host's facts, a command
  check mode cannot preview, a failed task, a secret that reaches the
  play through the extra-vars file and is in nothing Ansible prints, check
  mode with diff included, a credential whose user is read from the
  inventory and whose key is named on the command line and is in nothing
  Ansible prints or the plugin writes, a `lineinfile` module named in full
  that keeps a line, is run again when notified, takes its state out again
  and fails on an argument it does not have, and the modules' schemas read
  from `ansible-doc`. They are marked `#[ignore]`
  and run with
  `MERIDIAN_TEST_REAL_ANSIBLE=1 cargo test -p meridian-target-ansible --test real -- --ignored`.
  Reaching another machine over SSH, and the package, service, user and group
  modules, have only met stand-ins. See
  [Real-tool tests](../contributing/real-tool-tests.md).
